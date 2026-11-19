/**
 * This module is used to forward the unilib request from Unishyper to MVM.
 * Note:
 *  Currently it's a synchronous process, which means it's unefficient.
 *  In the case of 1-to-1 CPU partion strategy, I don't think it's a problem.
 *  But for different Unishypers runs on one single CPU, stucking in EL2 may cause a big efficiency problem,
 *  We still need some notify mechanism to improve the CPU usage.
 */
use alloc::collections::BTreeMap;
use core::ops::Deref;

use spin::Mutex;

use crate::kernel::HVC_UNILIB;
use crate::kernel::{active_vm, ipi_process_pending, HVC_UNILIB_FS_INIT, HVC_UNILIB_FS_LSEEK};
use crate::kernel::{hvc_send_msg_to_vm, HvcGuestMsg, HvcUniLibMsg};
use crate::kernel::{HVC_UNILIB_FS_CLOSE, HVC_UNILIB_FS_OPEN, HVC_UNILIB_FS_READ, HVC_UNILIB_FS_WRITE};
use crate::util::{memcpy_safe, sleep};

struct UnilibFSList {
    fs: BTreeMap<usize, UnilibFS>,
    /// The VM whose fs operation is waiting for the MVM; fs operations are sent one at a time.
    in_flight: Option<usize>,
}

static UNILIB_FS_LIST: Mutex<UnilibFSList> = Mutex::new(UnilibFSList {
    fs: BTreeMap::new(),
    in_flight: None,
});

#[repr(C)]
pub struct UnilibFSCfg {
    /// The name of this UnilibFS, it may used to identify the path of UnilibFS in MVM.
    name: [u8; 32],
    /// The "client" VM who owns this `UnilibFSCfg`, currently it should be our Unishyper.
    vmid: usize,
    /// Size of FS opration buffer, it's provided by the MVM with HUGE_TLB enabled.
    cache_size: usize,
    /// Virtual address of FS opration buffer, it's provided by the MVM with HUGE_TLB enabled.
    buf_va: usize,
    /// Intermediate physical address of FS opration buffer, it's provided by the MVM with HUGE_TLB enabled.
    buf_ipa: usize,
    /// Acutal physical address of FS opration buffer, it's set by the hypervisor during `unilib_fs_append` process.
    buf_pa: usize,
}

#[repr(C)]
pub struct UnilibFSOpsRes {
    /// Before each operation, we need to zero the flag field.
    /// When operation is completed by MVM, MVM's kernel module will set this field as the "client" OS's vm_id.
    flag: usize,
    /// The field `value` stores the operation result of MVM.
    /// It's set by the MVM, we alse need to reset it before each operation.
    value: usize,
}

/// This struct contains basic information about the UnilibFS, which shared between MVM and hypervisor.
/// * UnilibFSCfg: stores the config information.
/// * UnilibFSOpsRes: stores the operation result of MVM.
#[repr(C)]
pub struct UnilibFSContent {
    cfg: UnilibFSCfg,
    res: UnilibFSOpsRes,
}

#[derive(Clone)]
pub struct UnilibFS {
    pub base_addr: usize,
}

impl UnilibFS {
    /// Get the actual content of UnilibFS config, it's just a dereference from base_addr, which is unsafe.
    fn content(&self) -> &mut UnilibFSContent {
        unsafe { &mut *(self.base_addr as *mut UnilibFSContent) }
    }

    fn vm_id(&self) -> usize {
        self.content().cfg.vmid
    }

    fn get_buf(&self) -> *mut u8 {
        self.content().cfg.buf_pa as *mut u8
    }

    fn buf_ipa(&self) -> usize {
        self.content().cfg.buf_ipa
    }

    fn set_buf_pa(&self, buf_pa: usize) {
        self.content().cfg.buf_pa = buf_pa
    }

    fn flag(&self) -> usize {
        self.content().res.flag
    }

    fn zero_flag(&self) {
        self.content().res.flag = 0;
    }

    fn value(&self) -> usize {
        self.content().res.value
    }

    fn zero_value(&self) {
        self.content().res.value = 0
    }

    /// Before each fs operation is sent to MVM, this function is use to reset the `UnilibFSOpsRes` field.
    fn prepare_for_request(&self) {
        self.zero_flag();
        self.zero_value();
    }

    /// The main loop logic of synchronous process, it's stupid!!!
    /// The fs operation from guest VM should be synchronous,
    /// but current CPU need to wait for the implementation of MVM, and the IPI process is a asynchronous process.
    /// So we need to wait here and periodicly check the flag of `UnilibFSOpsRes`.
    fn loop_for_response(&self) -> usize {
        loop {
            if self.flag() != 0 {
                println!(
                    "unilib operation finished, flag {}, value {}",
                    self.flag(),
                    self.value()
                );
                break self.value();
            }
            sleep(1);
        }
    }
}

/// The right to send one fs operation of a VM to the MVM and wait for its result.
/// `UNILIB_FS_LIST` is only locked to take and to return it, not across the MVM round-trip:
/// the MVM takes the same lock in `unilib_fs_append` and `unilib_fs_remove`,
/// and cannot answer while it spins on a lock held by the waiting request.
struct UnilibFSRequest {
    fs: UnilibFS,
}

impl UnilibFSRequest {
    /// Wait until no fs operation is in flight, then take the registered `UnilibFS` of `vm_id`.
    fn start(vm_id: usize) -> Option<Self> {
        loop {
            let mut lock = UNILIB_FS_LIST.lock();
            if lock.in_flight.is_none() {
                let fs = lock.fs.get(&vm_id)?.clone();
                lock.in_flight = Some(vm_id);
                return Some(Self { fs });
            }
            drop(lock);
            sleep(1);
        }
    }
}

impl Deref for UnilibFSRequest {
    type Target = UnilibFS;
    fn deref(&self) -> &UnilibFS {
        &self.fs
    }
}

impl Drop for UnilibFSRequest {
    fn drop(&mut self) {
        UNILIB_FS_LIST.lock().in_flight = None;
    }
}

pub fn unilib_fs_remove(vm_id: usize) {
    // println!("unilib_fs_remove: VM[{}] umount unilib-fs", vm_id);
    // Wait until no fs operation is in flight, as the lock used to make the removal wait:
    // the operation may still use the memory of the VM being removed.
    loop {
        let mut lock = UNILIB_FS_LIST.lock();
        if lock.in_flight.is_none() {
            lock.fs.remove(&vm_id);
            return;
        }
        drop(lock);
        sleep(1);
    }
}

/// Init the UnilibFS of guest VM, triggered by GVM through a HVC call, HVC_UNILIB | HVC_UNILIB_FS_INIT.
/// This function generates a HvcGuestMsg to tell the MVM to init a new `UnilibFS` structure for this GVM.
/// The MVM's setting up process mainly happens in shyper-cli.
/// It's also a synchronous process, after send out the hvc guest msg, this function will enter a loop,
/// wait for the MVM(VM 0) to initialize the `UnilibFS` structure and insert it into `UNILIB_FS_LIST`.
pub fn unilib_fs_init() -> Result<usize, ()> {
    let vm = active_vm().unwrap();
    let vm_id = vm.id();
    println!("unilib_fs_init: VM[{}] init unilib-fs", vm.id());
    let unilib_msg = HvcUniLibMsg {
        fid: HVC_UNILIB,
        event: HVC_UNILIB_FS_INIT,
        vm_id: vm.id(),
        arg_1: 0,
        arg_2: 0,
        arg_3: 0,
    };
    if !hvc_send_msg_to_vm(0, &HvcGuestMsg::UniLib(unilib_msg)) {
        println!("unilib fs init: failed to notify VM 0");
        return Err(());
    }
    // Enter a loop, wait for VM0 to setup the unilib fs config struct.
    loop {
        let lock = UNILIB_FS_LIST.lock();
        if lock.fs.contains_key(&vm_id) {
            println!("unilib_fs_init, fs append success, return");
            drop(lock);
            return Ok(0);
        }
        drop(lock);
        // This wait holds no lock between rounds and runs with interrupts
        // masked; run pending MapIPA broadcasts (with their all-core
        // barrier) so a VM create on another core does not stall every
        // core until the registration arrives. Other messages stay queued.
        ipi_process_pending();
        sleep(5);
    }
}

/// Init the UnilibFS for guest VM, triggered by MVM through a HVC call, HVC_UNILIB | HVC_UNILIB_FS_APPEND.
/// After MVM's user daemon thread allocate the memory space for fs operation buffer, it'll tell the kernel module to setup the `UnilibFS` struct,
/// and then passed the ipa of `UnilibFS` struct to the hypervisor.
/// In this function, hypervisor calculates the actual physical address for fs operation buffer, set up the `UnilibFS` struct,
/// then insert it to `UNILIB_FS_LIST`.
/// After this function, `unilib_fs_init` should finished and return to GVM on EL1.
/// ## Arguments
/// * `mmio_ipa`        - The intermediated physical address of target GVM's `UnilibFS` struct provided ny MVM.
pub fn unilib_fs_append(mmio_ipa: usize) -> Result<usize, ()> {
    let vm = active_vm().unwrap();
    let mmio_pa = vm.ipa2hva(mmio_ipa);
    let unilib_fs = UnilibFS { base_addr: mmio_pa };
    let buf_pa = vm.ipa2hva(unilib_fs.buf_ipa());
    println!(
        "unilib_fs_append: VM[{}] fs_mmio_ipa {:#x}, buf ipa {:#x}, buf_pa {:#x}",
        unilib_fs.vm_id(),
        mmio_ipa,
        unilib_fs.buf_ipa(),
        buf_pa
    );
    unilib_fs.set_buf_pa(buf_pa);
    // The MVM answers by VM id only, so replace the registration of a VM only after its fs operation
    // in flight is done, as the lock used to make it wait; registering any other VM does not wait.
    // The wait compares the VM id sampled once here, so waiting never reads the caller's (guest)
    // memory, let alone under the lock. Once the wait is over, the registration is inserted under
    // the VM id read while the lock is held, at the same place as before, so the id a registration
    // lands on does not change.
    let vm_id = unilib_fs.vm_id();
    loop {
        let mut lock = UNILIB_FS_LIST.lock();
        if lock.in_flight != Some(vm_id) {
            lock.fs.insert(unilib_fs.vm_id(), unilib_fs);
            return Ok(0);
        }
        drop(lock);
        sleep(1);
    }
}

/// Finished one unilib fs operation.
/// Currently this function is unused, cause we use a loop for polling.
/// We may need to design a nofity mechanism in the future.
/// ## Arguments
/// * `vm_id`        - The target GVM's VM id of this unilib fs operation.
pub fn unilib_fs_finished(vm_id: usize) -> Result<usize, ()> {
    println!(
        "unilib_fs_finished: VM[{}] fs io request is finished, currently unused",
        vm_id
    );
    Ok(0)
}

/// **Open** API for unilib fs.
/// HVC_UNILIB | HVC_UNILIB_FS_OPEN
/// This function performs the open operation by send a HvcGuestMsg to MVM.
/// It's a synchronous process trigger by GVM.
/// If success, returns the **fd** of opened file wrapped by `Result` structure.
/// ## Arguments
/// * `path_start_ipa`  - The intermediated physical address of the path that GVM wants to open through unilib-fs API.
/// * `path_length`     - The string length of the path that GVM wants to open through unilib-fs API.
/// * `flags`           - The flags of open API, we need to care about the transfer between C and Rust.
pub fn unilib_fs_open(path_start_ipa: usize, path_length: usize, flags: usize) -> Result<usize, ()> {
    let vm = active_vm().unwrap();
    let vm_id = vm.id();
    // println!(
    //     "VM[{}] unilib fs open path_ipa: {:x}, path_length {}, flags {}",
    //     vm_id, path_start_ipa, path_length, flags
    // );
    // Get fs_cfg struct according to vm_id.
    let fs_cfg = match UnilibFSRequest::start(vm_id) {
        Some(cfg) => cfg,
        None => {
            println!("VM[{}] doesn't register a unilib fs, return", vm_id);
            return Err(());
        }
    };

    // Copy path to unilib_fs buf, see UnilibFSCfg.
    let path_pa = active_vm().unwrap().ipa2hva(path_start_ipa);
    memcpy_safe(fs_cfg.get_buf(), path_pa as *mut u8, path_length);
    // Add end '\0' for path buf.
    unsafe {
        *((fs_cfg.get_buf() as usize + path_length) as *mut u8) = 0u8;
    }

    fs_cfg.prepare_for_request();

    // Notify MVM to operate the fs operation.
    let unilib_msg = HvcUniLibMsg {
        fid: HVC_UNILIB,
        event: HVC_UNILIB_FS_OPEN,
        vm_id: vm.id(),
        arg_1: path_length,
        arg_2: flags,
        arg_3: 0,
    };
    if !hvc_send_msg_to_vm(0, &HvcGuestMsg::UniLib(unilib_msg)) {
        println!("unilib fs open: failed to notify VM 0");
        return Err(());
    }

    // Still, we need to enter a loop, wait for VM to complete operation.
    Ok(fs_cfg.loop_for_response())
}

/// **Close** API for unilib fs.
/// HVC_UNILIB | HVC_UNILIB_FS_CLOSE
/// This function performs the close operation by send a HvcGuestMsg to MVM.
/// It's a synchronous process trigger by GVM.
/// If success, returns the return value of close opreation passed from MVM's C lib, wrapped by `Result` structure.
/// ## Arguments
/// * `fd`  - The file descriptor of file to be closed.
pub fn unilib_fs_close(fd: usize) -> Result<usize, ()> {
    let vm = active_vm().unwrap();
    let vm_id = vm.id();
    // println!("VM[{}] unilib fs close fd {}", vm_id, fd);

    // Get fs_cfg struct according to vm_id.
    let fs_cfg = match UnilibFSRequest::start(vm_id) {
        Some(cfg) => cfg,
        None => {
            println!("VM[{}] doesn't register a unilib fs, return", vm_id);
            return Err(());
        }
    };

    fs_cfg.prepare_for_request();

    // Notify MVM to operate the fs operation.
    let unilib_msg = HvcUniLibMsg {
        fid: HVC_UNILIB,
        event: HVC_UNILIB_FS_CLOSE,
        vm_id,
        arg_1: fd,
        arg_2: 0,
        arg_3: 0,
    };
    if !hvc_send_msg_to_vm(0, &HvcGuestMsg::UniLib(unilib_msg)) {
        println!("unilib fs close: failed to notify VM 0");
        return Err(());
    }
    // Still, we need to enter a loop, wait for VM to complete operation.
    Ok(fs_cfg.loop_for_response())
}

/// **Read** API for unilib fs.
/// HVC_UNILIB | HVC_UNILIB_FS_READ
/// This function performs the read operation by send a HvcGuestMsg to MVM.
/// Read NBYTES into BUF from FD.
/// It's a synchronous process trigger by GVM.
/// If success, returns the number read, -1 for errors or 0 for EOF, wrapped by `Result` structure.
/// ## Arguments
/// * `fd`      - The file descriptor of file to read.
/// * `buf_ipa` - The intermediated physical address of the buffer to be read into.
/// * `len`     - Number of bytes to be read.
pub fn unilib_fs_read(fd: usize, buf_ipa: usize, len: usize) -> Result<usize, ()> {
    let vm = active_vm().unwrap();
    let vm_id = vm.id();
    // println!(
    //     "VM[{}] unilib fs read fd {}, buf_ipa {:x}, len {}",
    //     vm_id, fd, buf_ipa, len
    // );
    // Get fs_cfg struct according to vm_id.
    let fs_cfg = match UnilibFSRequest::start(vm_id) {
        Some(cfg) => cfg,
        None => {
            println!("VM[{}] doesn't register a unilib fs, return", vm_id);
            return Err(());
        }
    };
    fs_cfg.prepare_for_request();
    // Notify MVM to operate the fs operation.
    let unilib_msg = HvcUniLibMsg {
        fid: HVC_UNILIB,
        event: HVC_UNILIB_FS_READ,
        vm_id: vm.id(),
        arg_1: fd,
        arg_2: len,
        arg_3: 0,
    };
    if !hvc_send_msg_to_vm(0, &HvcGuestMsg::UniLib(unilib_msg)) {
        println!("unilib fs read: failed to notify VM 0");
        return Err(());
    }

    // Still, we need to enter a loop, wait for VM to complete operation.
    let res = fs_cfg.loop_for_response() as i64;

    if res < 0 {
        return Ok(res as usize);
    }
    let buf_pa = vm.ipa2hva(buf_ipa);
    memcpy_safe(buf_pa as *mut u8, fs_cfg.get_buf(), fs_cfg.value());
    Ok(fs_cfg.value())
}

/// **Write** API for unilib fs.
/// HVC_UNILIB | HVC_UNILIB_FS_WRITE
/// This function performs the write operation by send a HvcGuestMsg to MVM.
/// Write N bytes of BUF to FD. Return the number written.
/// It's a synchronous process trigger by GVM.
/// If success, returns the number written, or -1, wrapped by `Result` structure.
/// ## Arguments
/// * `fd`      - The file descriptor of file to write to.
/// * `buf_ipa` - The intermediated physical address of the buffer waiting to be written to the target file.
/// * `len`     - Number of bytes to be written.
pub fn unilib_fs_write(fd: usize, buf_ipa: usize, len: usize) -> Result<usize, ()> {
    let vm = active_vm().unwrap();
    let vm_id = vm.id();
    // println!(
    //     "VM[{}] unilib fs write fd {}, buf_ipa {:x}, len {}",
    //     vm_id, fd, buf_ipa, len
    // );

    // Get fs_cfg struct according to vm_id.
    let fs_cfg = match UnilibFSRequest::start(vm_id) {
        Some(cfg) => cfg,
        None => {
            println!("VM[{}] doesn't register a unilib fs, return", vm_id);
            return Err(());
        }
    };
    let buf_pa = vm.ipa2hva(buf_ipa);
    memcpy_safe(fs_cfg.get_buf(), buf_pa as *mut u8, len);

    fs_cfg.prepare_for_request();

    // Notify MVM to operate the fs operation.
    let unilib_msg = HvcUniLibMsg {
        fid: HVC_UNILIB,
        event: HVC_UNILIB_FS_WRITE,
        vm_id: vm.id(),
        arg_1: fd,
        arg_2: len,
        arg_3: 0,
    };
    if !hvc_send_msg_to_vm(0, &HvcGuestMsg::UniLib(unilib_msg)) {
        println!("unilib fs write: failed to notify VM 0");
        return Err(());
    }

    // Still, we need to enter a loop, wait for VM to complete operation.
    Ok(fs_cfg.loop_for_response())
}

/// **Lseek** API for unilib fs.
/// HVC_UNILIB | HVC_UNILIB_FS_LSEEK
/// This function performs the lseek operation by send a HvcGuestMsg to MVM.
/// Reposition read/write file offset.
/// lseek() repositions the file offset of the open file description associated with the file descriptor fd to the argument offset according to the directive whence.
/// It's a synchronous process trigger by GVM.
/// Upon successful completion, lseek() returns the resulting offset
/// location as measured in bytes from the beginning of the file, wrapped by `Result` structure.
/// ## Arguments
/// * `fd`     - The file descriptor of file.
/// * `offset` - The file offset of the open file.
/// * `whence` - Only can be these three following types currently:
///                 SEEK_SET 0 : Seek from beginning of file, the file offset is set to offset bytes.
///                 SEEK_CUR 1 : Seek from current position, the file offset is set to its current location plus offset bytes.
///                 SEEK_END 2 : Seek from end of file, the file offset is set to the size of the file plus offset bytes.
pub fn unilib_fs_lseek(fd: usize, offset: usize, whence: usize) -> Result<usize, ()> {
    let vm = active_vm().unwrap();
    let vm_id = vm.id();
    // println!(
    //     "VM[{}] unilib fs lseek fd {}, offset {}, whence {}",
    //     vm_id, fd, offset, whence
    // );
    // Get fs_cfg struct according to vm_id.
    let fs_cfg = match UnilibFSRequest::start(vm_id) {
        Some(cfg) => cfg,
        None => {
            println!("VM[{}] doesn't register a unilib fs, return", vm_id);
            return Err(());
        }
    };
    fs_cfg.prepare_for_request();

    // Notify MVM to operate the fs operation.
    let unilib_msg = HvcUniLibMsg {
        fid: HVC_UNILIB,
        event: HVC_UNILIB_FS_LSEEK,
        vm_id: vm.id(),
        arg_1: fd,
        arg_2: offset,
        arg_3: whence,
    };
    if !hvc_send_msg_to_vm(0, &HvcGuestMsg::UniLib(unilib_msg)) {
        println!("unilib fs read: failed to notify VM 0");
        return Err(());
    }
    // Still, we need to enter a loop, wait for VM to complete operation.
    Ok(fs_cfg.loop_for_response())
}

/// **Stat** API for unilib fs.
/// HVC_UNILIB | HVC_UNILIB_FS_STAT
/// Currently unsupported.
pub fn unilib_fs_stat() -> Result<usize, ()> {
    unimplemented!("stat is unimplemented");
}
