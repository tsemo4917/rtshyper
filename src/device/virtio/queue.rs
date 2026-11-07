use alloc::sync::{Arc, Weak};
use core::mem::{align_of, size_of};
use core::slice;

use spin::Mutex;

use crate::device::VirtioMmio;
use crate::kernel::{active_vm, Vm};

use virtq::VringDesc;

pub use virtq::{ChainError, DescIter, VIRTIO_RING_F_INDIRECT_DESC};

pub const VIRTQ_READY: usize = 1;

/* The device uses this in used->flags to advise the driver: don't kick me
 * when you add a buffer. It's unreliable, so it's simply an
 * optimization. */
pub const VRING_USED_F_NO_NOTIFY: usize = 1;

const DESC_QUEUE_SIZE: usize = 512;

#[repr(C)]
#[derive(Copy, Clone)]
struct VringAvail {
    flags: u16,
    idx: u16,
    ring: [u16; DESC_QUEUE_SIZE],
}

#[repr(C)]
#[derive(Copy, Clone)]
struct VringUsedElem {
    id: u32,
    len: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct VringUsed {
    flags: u16,
    idx: u16,
    ring: [VringUsedElem; DESC_QUEUE_SIZE],
}

pub struct Virtq {
    vq_index: usize,
    notify_handler: fn(Arc<Self>, Arc<VirtioMmio>, Arc<Vm>) -> bool,
    mmio: Weak<VirtioMmio>,
    inner: Mutex<VirtqInner<'static>>,
}

impl Virtq {
    pub fn new(
        vq_index: usize,
        mmio: Weak<VirtioMmio>,
        notify_handler: fn(Arc<Self>, Arc<VirtioMmio>, Arc<Vm>) -> bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            vq_index,
            notify_handler,
            mmio,
            inner: Mutex::new(VirtqInner::default()),
        })
    }

    pub fn reset(&self) {
        let mut inner = self.inner.lock();
        inner.reset();
    }

    pub fn pop_avail_desc_idx(&self, avail_idx: u16) -> Option<u16> {
        let mut inner = self.inner.lock();
        match &inner.avail {
            Some(avail) => {
                if avail_idx == inner.last_avail_idx {
                    return None;
                }
                let idx = inner.last_avail_idx as usize % inner.num;
                let avail_desc_idx = avail.ring[idx];
                inner.last_avail_idx = inner.last_avail_idx.wrapping_add(1);
                Some(avail_desc_idx)
            }
            None => {
                println!("pop_avail_desc_idx: failed to avail table");
                None
            }
        }
    }

    pub fn put_back_avail_desc_idx(&self) {
        let mut inner = self.inner.lock();
        match &inner.avail {
            Some(_) => {
                inner.last_avail_idx -= 1;
            }
            None => {
                println!("put_back_avail_desc_idx: failed to avail table");
            }
        }
    }

    pub fn avail_is_avail(&self) -> bool {
        let inner = self.inner.lock();
        inner.avail.is_some()
    }

    pub fn disable_notify(&self) {
        let mut inner = self.inner.lock();
        if inner.used_flags & VRING_USED_F_NO_NOTIFY as u16 != 0 {
            return;
        }
        inner.used_flags |= VRING_USED_F_NO_NOTIFY as u16;
    }

    pub fn enable_notify(&self) {
        let mut inner = self.inner.lock();
        if inner.used_flags & VRING_USED_F_NO_NOTIFY as u16 == 0 {
            return;
        }
        inner.used_flags &= !VRING_USED_F_NO_NOTIFY as u16;
    }

    pub fn check_avail_idx(&self, avail_idx: u16) -> bool {
        let inner = self.inner.lock();
        inner.last_avail_idx == avail_idx
    }

    /// Iterate the descriptor chain whose head is the avail ring entry
    /// `head_idx`, including an indirect table when the head descriptor has
    /// `VIRTQ_DESC_F_INDIRECT`.
    ///
    /// `vm` translates the guest-physical address of an indirect table. Each
    /// step reads one descriptor in place; the indirect table is not copied.
    /// The iterator never yields the indirect head itself. The used ring must
    /// still be updated with the main-table `head_idx`.
    ///
    /// The `'static` lifetime does not borrow `self`. Descriptor memory lives
    /// in the guest mapping, so a notify handler can still call queue methods
    /// while iterating. The iterator must be finished before the queue is
    /// reset; the mapping stays valid for the handler, the same assumption the
    /// previous raw-pointer walks made.
    pub fn desc_chain(&self, head_idx: u16, vm: &Vm) -> Result<DescIter<'static>, ChainError> {
        let (table_ptr, num) = {
            let inner = self.inner.lock();
            match &inner.desc_table {
                Some(table) => (table.as_ptr(), inner.num.min(DESC_QUEUE_SIZE)),
                None => {
                    println!("desc_chain: failed to get desc table");
                    return Err(ChainError::HeadOutOfBounds);
                }
            }
        };
        // `num` is clamped to the table allocation. The slice outlives this
        // function because the guest mapping does; see the method comment.
        // An unaligned or empty table must not become a reference.
        if num == 0 || table_ptr.is_null() || (table_ptr as usize) % align_of::<VringDesc>() != 0 {
            println!("desc_chain: desc table is empty or unaligned");
            return Err(ChainError::HeadOutOfBounds);
        }
        let desc_table: &'static [VringDesc] = unsafe { slice::from_raw_parts(table_ptr, num) };
        virtq::iter_chain(desc_table, head_idx, |gpa, len| {
            let hva = vm.ipa2hva(gpa as usize);
            // VringDesc is align(16). An unaligned guest address would make
            // the reference below undefined behavior.
            if hva == 0 || hva % align_of::<VringDesc>() != 0 {
                return None;
            }
            let count = (len as usize) / size_of::<VringDesc>();
            Some(unsafe { slice::from_raw_parts(hva as *const VringDesc, count) })
        })
    }

    pub fn update_used_ring(&self, len: u32, desc_chain_head_idx: u32) -> bool {
        let mut inner = self.inner.lock();
        let num = inner.num;
        let flag = inner.used_flags;
        match &mut inner.used {
            Some(used) => {
                used.flags = flag;
                used.ring[used.idx as usize % num].id = desc_chain_head_idx;
                used.ring[used.idx as usize % num].len = len;
                used.idx = used.idx.wrapping_add(1);
                true
            }
            None => {
                println!("update_used_ring: failed to used table");
                false
            }
        }
    }

    pub fn call_notify_handler(self: &Arc<Self>) -> bool {
        if let Some(mmio) = self.mmio.upgrade() {
            (self.notify_handler)(self.clone(), mmio, active_vm().unwrap())
        } else {
            false
        }
    }

    // pub fn show_desc_info(&self, size: usize, vm: Vm) {
    //     let inner = self.inner.lock();
    //     let desc = inner.desc_table.as_ref().unwrap();
    //     println!("[*desc_ring*]");
    //     for i in 0..size {
    //         let desc_addr = vm.ipa2hva(desc[i].addr as usize);
    //         println!(
    //             "index {}   desc_addr_ipa {:#x}   desc_addr_pa {:#x}   len {:#x}   flags {}  next {}",
    //             i, desc[i].addr, desc_addr, desc[i].len, desc[i].flags, desc[i].next
    //         );
    //     }
    // }

    // pub fn show_avail_info(&self, size: usize) {
    //     let inner = self.inner.lock();
    //     let avail = inner.avail.as_ref().unwrap();
    //     println!("[*avail_ring*]");
    //     for i in 0..size {
    //         println!("index {} ring_idx {}", i, avail.ring[i]);
    //     }
    // }

    // pub fn show_used_info(&self, size: usize) {
    //     let inner = self.inner.lock();
    //     let used = inner.used.as_ref().unwrap();
    //     println!("[*used_ring*]");
    //     for i in 0..size {
    //         println!(
    //             "index {} ring_id {} ring_len {:x}",
    //             i, used.ring[i].id, used.ring[i].len
    //         );
    //     }
    // }

    // pub fn show_addr_info(&self) {
    //     let inner = self.inner.lock();
    //     println!(
    //         "avail_addr {:x}, desc_addr {:x}, used_addr {:x}",
    //         inner.avail_addr, inner.desc_table_addr, inner.used_addr
    //     );
    // }

    // pub fn set_last_used_idx(&self, last_used_idx: u16) {
    //     let mut inner = self.inner.lock();
    //     inner.last_used_idx = last_used_idx;
    // }

    pub fn set_num(&self, num: usize) {
        let mut inner = self.inner.lock();
        inner.num = num;
    }

    pub fn set_ready(&self, ready: usize) {
        let mut inner = self.inner.lock();
        inner.ready = ready;
    }

    pub fn or_desc_table_addr(&self, addr: usize) {
        let mut inner = self.inner.lock();
        inner.desc_table_addr |= addr;
    }

    pub fn or_avail_addr(&self, addr: usize) {
        let mut inner = self.inner.lock();
        inner.avail_addr |= addr;
    }

    pub fn or_used_addr(&self, addr: usize) {
        let mut inner = self.inner.lock();
        inner.used_addr |= addr;
    }

    pub fn set_desc_table(&self, addr: usize) {
        let mut inner = self.inner.lock();
        if addr < 0x1000 {
            panic!("illegal desc ring addr {:x}", addr);
        }
        inner.desc_table = Some(unsafe { slice::from_raw_parts_mut(addr as *mut VringDesc, DESC_QUEUE_SIZE) });
    }

    pub fn set_avail(&self, addr: usize) {
        if addr < 0x1000 {
            panic!("illegal avail ring addr {:x}", addr);
        }
        let mut inner = self.inner.lock();
        inner.avail = Some(unsafe { &mut *(addr as *mut VringAvail) });
    }

    pub fn set_used(&self, addr: usize) {
        if addr < 0x1000 {
            panic!("illegal used ring addr {:x}", addr);
        }
        let mut inner = self.inner.lock();
        inner.used = Some(unsafe { &mut *(addr as *mut VringUsed) });
    }

    // pub fn last_used_idx(&self) -> u16 {
    //     let inner = self.inner.lock();
    //     inner.last_used_idx
    // }

    pub fn desc_table_addr(&self) -> usize {
        let inner = self.inner.lock();
        inner.desc_table_addr
    }

    pub fn avail_addr(&self) -> usize {
        let inner = self.inner.lock();
        inner.avail_addr
    }

    pub fn used_addr(&self) -> usize {
        let inner = self.inner.lock();
        inner.used_addr
    }

    // pub fn desc_table(&self) -> usize {
    //     let inner = self.inner.lock();
    //     match &inner.desc_table {
    //         None => 0,
    //         Some(desc_table) => &(desc_table[0]) as *const _ as usize,
    //     }
    // }

    // pub fn avail(&self) -> usize {
    //     let inner = self.inner.lock();
    //     match &inner.avail {
    //         None => 0,
    //         Some(avail) => (*avail) as *const _ as usize,
    //     }
    // }

    // pub fn used(&self) -> usize {
    //     let inner = self.inner.lock();
    //     match &inner.used {
    //         None => 0,
    //         Some(used) => (*used) as *const _ as usize,
    //     }
    // }

    pub fn ready(&self) -> usize {
        let inner = self.inner.lock();
        inner.ready
    }

    pub fn vq_indx(&self) -> usize {
        self.vq_index
    }

    pub fn num(&self) -> usize {
        let inner = self.inner.lock();
        inner.num
    }

    pub fn avail_flags(&self) -> u16 {
        let inner = self.inner.lock();
        let avail = inner.avail.as_ref().unwrap();
        avail.flags
    }

    pub fn avail_idx(&self) -> u16 {
        let inner = self.inner.lock();
        let avail = inner.avail.as_ref().unwrap();
        avail.idx
    }

    // pub fn last_avail_idx(&self) -> u16 {
    //     let inner = self.inner.lock();
    //     inner.last_avail_idx
    // }

    // pub fn used_idx(&self) -> u16 {
    //     let inner = self.inner.lock();
    //     let used = inner.used.as_ref().unwrap();
    //     used.idx
    // }
}

#[derive(Default)]
struct VirtqInner<'a> {
    ready: usize,
    num: usize,
    desc_table: Option<&'a mut [VringDesc]>,
    avail: Option<&'a mut VringAvail>,
    used: Option<&'a mut VringUsed>,
    last_avail_idx: u16,
    last_used_idx: u16,
    used_flags: u16,

    desc_table_addr: usize,
    avail_addr: usize,
    used_addr: usize,
}

impl VirtqInner<'_> {
    // virtio_queue_reset
    pub fn reset(&mut self) {
        self.ready = 0;
        self.num = 0;
        self.last_avail_idx = 0;
        self.last_used_idx = 0;
        self.used_flags = 0;
        self.desc_table_addr = 0;
        self.avail_addr = 0;
        self.used_addr = 0;

        self.desc_table = None;
        self.avail = None;
        self.used = None;
    }
}
