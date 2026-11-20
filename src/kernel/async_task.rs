use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::task::Context;

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, LinkedList};
use alloc::sync::Arc;
use alloc::task::Wake;
use spin::mutex::Mutex;

use crate::device::{mediated_blk_read, mediated_blk_write, virtio_blk_notify_handler, ReadAsyncMsg, WriteAsyncMsg};
use crate::kernel::{active_vm, ipi_process_pending, ipi_send_msg, IpiInnerMsg, IpiMediatedMsg, IpiType};
use crate::util::{memcpy_safe, sleep};

#[derive(Clone, Copy, Debug)]
pub enum AsyncTaskState {
    Pending,
    Running,
    Finish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AsyncExeStatus {
    Pending,
    Scheduling,
}

pub struct Executor {
    status: Mutex<AsyncExeStatus>,
    ipi_task_list: Mutex<LinkedList<Arc<AsyncTask>>>,
    io_task_list: Mutex<FairQueue<AsyncTask>>,
}

impl Executor {
    const fn new() -> Self {
        Self {
            status: Mutex::new(AsyncExeStatus::Pending),
            ipi_task_list: Mutex::new(LinkedList::new()),
            io_task_list: Mutex::new(FairQueue::new()),
        }
    }

    fn status(&self) -> AsyncExeStatus {
        *self.status.lock()
    }

    fn set_status(&self, status: AsyncExeStatus) {
        *self.status.lock() = status;
    }

    pub fn exec(&self) {
        if active_vm().unwrap().id() == 0 {
            match self.status() {
                AsyncExeStatus::Pending => self.set_status(AsyncExeStatus::Scheduling),
                AsyncExeStatus::Scheduling => return,
            }
        }
        loop {
            let ipi_list = self.ipi_task_list.lock();
            let io_list = self.io_task_list.lock();

            let (task, ipi) = if io_list.is_empty() && ipi_list.is_empty() {
                self.set_status(AsyncExeStatus::Pending);
                return;
            } else if !io_list.is_empty() {
                // if io_list is not empty, prioritize IO requests
                (io_list.front().unwrap().clone(), false)
            } else {
                // other VM start an IO which need to be handled by service VM
                (ipi_list.front().unwrap().clone(), true)
            };
            drop(ipi_list);
            drop(io_list);
            if task.handle() || ipi {
                // task finish
                self.finish_task(ipi);
            } else {
                // wait for notify
                self.set_status(AsyncExeStatus::Pending);
                return;
            }
            // not a service VM, end loop
            if active_vm().unwrap().id() != 0 {
                return;
            }
        }
    }

    pub fn set_front_io_task_state(&self, state: AsyncTaskState) {
        if let Some(task) = self.io_task_list.lock().front() {
            task.set_state(state)
        }
    }

    pub fn add_task(&self, task: AsyncTask, ipi: bool) {
        while active_vm().unwrap().id() != 0 && self.io_task_list.lock().len() >= 64 {
            // This backpressure wait holds no lock between rounds and runs
            // with interrupts masked; run pending MapIPA broadcasts (with
            // their all-core barrier) so a VM create on another core does
            // not stall every core until the backlog drains. Other
            // messages stay queued for the normal interrupt path.
            ipi_process_pending();
            sleep(1);
        }
        let mut ipi_list = self.ipi_task_list.lock();
        let mut io_list = self.io_task_list.lock();
        let need_execute = active_vm().unwrap().id() != 0
            && ipi_list.is_empty()
            && io_list.is_empty()
            && self.status() == AsyncExeStatus::Pending;
        if ipi {
            ipi_list.push_back(Arc::new(task));
        } else {
            io_list.push_back(Arc::new(task));
        }
        drop(ipi_list);
        drop(io_list);
        // if this is a normal VM and this is the first IO request
        // (which generate a ipi async task in `virtio_mediated_blk_notify_handler`)
        // invoke the executor to handle it
        if need_execute {
            self.exec();
        }
    }

    fn finish_task(&self, ipi: bool) {
        if let Some(task) = if ipi {
            self.ipi_task_list.lock().pop_front()
        } else {
            self.io_task_list.lock().pop_front()
        } {
            task.callback.finish();
        }
    }
}

pub static EXECUTOR: Executor = Executor::new();

trait TaskOwner {
    fn owner(&self) -> usize;
}

struct FairQueue<T: TaskOwner> {
    len: usize,
    map: BTreeMap<usize, LinkedList<Arc<T>>>,
    queue: LinkedList<usize>,
}

impl<T: TaskOwner> FairQueue<T> {
    const fn new() -> Self {
        Self {
            len: 0,
            map: BTreeMap::new(),
            queue: LinkedList::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    fn len(&self) -> usize {
        self.len
    }

    fn push_back(&mut self, task: Arc<T>) {
        let key = task.owner();
        match self.map.get_mut(&key) {
            Some(sub_queue) => sub_queue.push_back(task),
            None => {
                let mut sub_queue = LinkedList::new();
                sub_queue.push_back(task);
                self.map.insert(key, sub_queue);
                self.queue.push_back(key);
            }
        }
        self.len += 1;
    }

    fn pop_front(&mut self) -> Option<Arc<T>> {
        match self.queue.pop_front() {
            Some(owner) => match self.map.get_mut(&owner) {
                Some(sub_queue) => {
                    let res = sub_queue.pop_front();
                    if !sub_queue.is_empty() {
                        self.queue.push_back(owner);
                    } else {
                        self.map.remove(&owner);
                    }
                    self.len -= 1;
                    res
                }
                None => None,
            },
            None => None,
        }
    }

    fn front(&self) -> Option<&Arc<T>> {
        match self.queue.front() {
            Some(owner) => match self.map.get(owner) {
                Some(sub_queue) => sub_queue.front(),
                None => None,
            },
            None => None,
        }
    }

    fn remove(&mut self, owner: usize) {
        if let Some(sub_queue) = self.map.remove(&owner) {
            self.len -= sub_queue.len();
            self.queue.extract_if(|x| *x == owner).for_each(drop);
        }
    }
}

pub trait AsyncCallback {
    fn preprocess(&self);
    #[inline]
    fn finish(&self) {}
}

impl AsyncCallback for IpiMediatedMsg {
    #[inline]
    fn preprocess(&self) {
        if active_vm().unwrap().id() == 0 {
            virtio_blk_notify_handler(self.vq.clone(), self.blk.clone(), self.src_vm.clone());
        } else {
            // send IPI to target cpu, and the target will invoke `mediated_ipi_handler`
            ipi_send_msg(0, IpiType::MediatedDev, IpiInnerMsg::MediatedMsg(self.clone()));
        }
    }
}

impl AsyncCallback for ReadAsyncMsg {
    #[inline]
    fn preprocess(&self) {
        mediated_blk_read(self.blk_id, self.sector, self.count);
    }

    #[inline]
    fn finish(&self) {
        // let mut sum = 0;
        let mut cache_ptr = self.cache;
        for iov in self.iov_list.iter() {
            let data_bg = iov.data_bg;
            let len = iov.len as usize;
            memcpy_safe(data_bg as *mut u8, cache_ptr as *mut u8, len);
            // sum |= check_sum(data_bg, len);
            cache_ptr += len;
        }
        // println!("read check_sum is {:x}", sum);
        let info = &self.used_info;
        self.vq.update_used_ring(info.used_len, info.desc_chain_head_idx);
        self.dev.notify();
    }
}

impl AsyncCallback for WriteAsyncMsg {
    #[inline]
    fn preprocess(&self) {
        // copy buffer to cache
        let mut buffer = self.buffer.lock();
        memcpy_safe(self.cache as *mut u8, buffer.as_ptr(), buffer.len());
        mediated_blk_write(self.blk_id, self.sector, self.count);
        buffer.clear();
        let info = &self.used_info;
        self.vq.update_used_ring(info.used_len, info.desc_chain_head_idx);
        self.dev.notify();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
struct TaskId(usize);

impl TaskId {
    fn new() -> Self {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

pub struct AsyncTask {
    #[allow(unused)]
    id: TaskId,
    callback: Box<dyn AsyncCallback + Send + Sync>,
    src_vmid: usize,
    state: Mutex<AsyncTaskState>,
    task: Mutex<Pin<Box<dyn Future<Output = ()> + 'static + Send + Sync>>>,
}

impl TaskOwner for AsyncTask {
    fn owner(&self) -> usize {
        self.src_vmid
    }
}

impl Wake for AsyncTask {
    fn wake(self: Arc<Self>) {
        todo!()
    }
}

impl AsyncTask {
    pub fn new(
        callback: impl AsyncCallback + 'static + Send + Sync,
        src_vmid: usize,
        future: impl Future<Output = ()> + 'static + Send + Sync,
    ) -> Self {
        Self {
            id: TaskId::new(),
            callback: Box::new(callback),
            src_vmid,
            state: Mutex::new(AsyncTaskState::Pending),
            task: Mutex::new(Box::pin(future)),
        }
    }

    fn handle(self: &Arc<Self>) -> bool {
        let mut state = self.state.lock();
        match *state {
            AsyncTaskState::Pending => *state = AsyncTaskState::Running,
            AsyncTaskState::Running => {
                return false;
            }
            AsyncTaskState::Finish => {
                return true;
            }
        }
        drop(state);
        let waker = self.clone().into();
        let mut context = Context::from_waker(&waker);
        let _ = self.task.lock().as_mut().poll(&mut context);
        false
    }

    fn set_state(&self, state: AsyncTaskState) {
        let mut cur_state = self.state.lock();
        *cur_state = state;
    }
}

// async req function
pub async fn async_ipi_req() {
    let ipi_list = EXECUTOR.ipi_task_list.lock();
    if let Some(task) = ipi_list.front().cloned() {
        drop(ipi_list);
        task.callback.preprocess();
    }
}

pub async fn async_blk_io_req() {
    let io_list = EXECUTOR.io_task_list.lock();
    if let Some(task) = io_list.front().cloned() {
        drop(io_list);
        task.callback.preprocess();
    }
}
// end async req function

pub fn remove_vm_async_task(vm_id: usize) {
    let mut io_list = EXECUTOR.io_task_list.lock();
    let mut ipi_list = EXECUTOR.ipi_task_list.lock();
    io_list.remove(vm_id);
    ipi_list.extract_if(|x| x.src_vmid == vm_id).for_each(drop);
}
