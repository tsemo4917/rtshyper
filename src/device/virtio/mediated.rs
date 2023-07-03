use alloc::sync::Arc;
use alloc::vec::Vec;

use spin::Mutex;

use crate::device::{virtio_blk_notify_handler, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT};
use crate::kernel::{
    active_vm, EXECUTOR, AsyncTaskState, hvc_send_msg_to_vm, HvcDefaultMsg, HvcGuestMsg, IpiInnerMsg, vm, vm_id_list,
    HVC_MEDIATED, HVC_MEDIATED_DEV_NOTIFY, HVC_MEDIATED_DRV_NOTIFY, Vm,
};
use crate::kernel::{ipi_register, IpiMessage, IpiType};
use shyper::MediatedBlkContent;

use super::{Virtq, VirtioMmio, BlkIov};

pub static MEDIATED_BLK_LIST: Mutex<Vec<MediatedBlk>> = Mutex::new(Vec::new());

pub fn mediated_blk_list_push(mut blk: MediatedBlk) {
    let mut list = MEDIATED_BLK_LIST.lock();
    let vm_id_list = vm_id_list();
    for vm_id in vm_id_list {
        let vm = vm(vm_id).unwrap();
        if let Some(id) = vm.config().mediated_block_index() {
            if id == list.len() {
                info!("Assign blk[{}] to VM {}", list.len(), vm.id());
                blk.avail = false;
                #[cfg(feature = "static-config")]
                {
                    // NOTE: here, VM0 must monopolize Core 0
                    use crate::vmm::vmm_boot_vm;
                    vmm_boot_vm(vm.id());
                }
                break;
            }
        }
    }
    list.push(blk);
}

// TODO: not concern abort the num of sectors
pub fn mediated_blk_request() -> Result<usize, ()> {
    let mut list = MEDIATED_BLK_LIST.lock();
    for (idx, blk) in list.iter_mut().enumerate() {
        if blk.avail {
            blk.avail = false;
            return Ok(idx);
        }
    }
    Err(())
}

pub fn mediated_blk_free(idx: usize) {
    let mut list = MEDIATED_BLK_LIST.lock();
    list[idx].avail = true;
}

pub fn mediated_blk_list_get(idx: usize) -> MediatedBlk {
    let list = MEDIATED_BLK_LIST.lock();
    list[idx].clone()
}

pub fn mediated_blk_list_get_from_pa(pa: usize) -> Option<MediatedBlk> {
    let list = MEDIATED_BLK_LIST.lock();
    for blk in &*list {
        if blk.base_addr == pa {
            return Some(blk.clone());
        }
    }
    None
}

#[derive(Clone)]
pub struct MediatedBlk {
    pub base_addr: usize,
    pub avail: bool, // mediated blk will not be removed after append
}

impl MediatedBlk {
    pub fn content(&self) -> &mut MediatedBlkContent {
        if self.base_addr < 0x1000 {
            panic!("illeagal addr {:x}", self.base_addr);
        }
        unsafe { &mut *(self.base_addr as *mut MediatedBlkContent) }
    }

    pub fn dma_block_max(&self) -> usize {
        self.content().cfg.dma_block_max
    }

    pub fn nreq(&self) -> usize {
        self.content().nreq
    }

    pub fn cache_ipa(&self) -> usize {
        self.content().cfg.cache_ipa
    }

    pub fn cache_pa(&self) -> usize {
        self.content().cfg.cache_pa
    }

    pub fn set_nreq(&self, nreq: usize) {
        self.content().nreq = nreq;
    }

    pub fn set_type(&self, req_type: usize) {
        self.content().req.req_type = req_type as u32;
    }

    pub fn set_sector(&self, sector: usize) {
        self.content().req.sector = sector;
    }

    pub fn set_count(&self, count: usize) {
        self.content().req.count = count;
    }

    pub fn set_cache_pa(&self, cache_pa: usize) {
        self.content().cfg.cache_pa = cache_pa;
    }
}

pub fn mediated_dev_init() {
    ipi_register(IpiType::IpiTMediatedDev, mediated_ipi_handler);
}

// only run in vm0
pub fn mediated_dev_append(_class_id: usize, mmio_ipa: usize) -> Result<usize, ()> {
    let vm = active_vm().unwrap();
    let blk_pa = vm.ipa2hva(mmio_ipa);
    let mediated_blk = MediatedBlk {
        base_addr: blk_pa,
        avail: true,
    };
    mediated_blk.set_nreq(0);

    let cache_pa = vm.ipa2hva(mediated_blk.cache_ipa());
    info!(
        "mediated_dev_append: dev_ipa_reg {:#x}, cache ipa {:#x}, cache_pa {:#x}, dma_block_max {:#x}",
        mmio_ipa,
        mediated_blk.cache_ipa(),
        cache_pa,
        mediated_blk.dma_block_max()
    );
    mediated_blk.set_cache_pa(cache_pa);
    mediated_blk_list_push(mediated_blk);
    Ok(0)
}

// service VM finish blk request, and inform the requested VM
pub fn mediated_blk_notify_handler(dev_ipa_reg: usize) -> Result<usize, ()> {
    let dev_pa_reg = active_vm().unwrap().ipa2hva(dev_ipa_reg);

    // check weather src vm is still alive
    let mediated_blk = match mediated_blk_list_get_from_pa(dev_pa_reg) {
        Some(blk) => blk,
        None => {
            println!("illegal mediated blk pa {:x} ipa {:x}", dev_pa_reg, dev_ipa_reg);
            return Err(());
        }
    };
    if !mediated_blk.avail {
        // finish current IO task
        EXECUTOR.set_front_io_task_state(AsyncTaskState::Finish);
    } else {
        println!("Mediated blk not belong to any VM");
    }
    // invoke the excuter to handle finished IO task
    EXECUTOR.exec();
    Ok(0)
}

#[allow(dead_code)]
fn check_sum(addr: usize, len: usize) -> usize {
    let slice = unsafe { core::slice::from_raw_parts(addr as *const usize, len / 8) };
    let mut sum = 0;
    for num in slice {
        sum ^= num;
    }
    sum
}

// call by normal VMs ipi request (generated by mediated virtio blk)
pub fn mediated_ipi_handler(msg: IpiMessage) {
    // println!("core {} mediated_ipi_handler", current_cpu().id);
    if let IpiInnerMsg::MediatedMsg(mediated_msg) = msg.ipi_message {
        // generate IO request in `virtio_blk_notify_handler`
        virtio_blk_notify_handler(mediated_msg.vq, mediated_msg.blk, mediated_msg.src_vm);
        // invoke the executor to do IO request
        EXECUTOR.exec();
    }
}

pub fn mediated_blk_read(blk_idx: usize, sector: usize, count: usize) {
    let mediated_blk = mediated_blk_list_get(blk_idx);
    let nreq = mediated_blk.nreq();
    mediated_blk.set_nreq(nreq + 1);
    mediated_blk.set_type(VIRTIO_BLK_T_IN);
    mediated_blk.set_sector(sector);
    mediated_blk.set_count(count);

    let med_msg = HvcDefaultMsg {
        fid: HVC_MEDIATED,
        event: HVC_MEDIATED_DEV_NOTIFY,
    };

    if !hvc_send_msg_to_vm(0, &HvcGuestMsg::Default(med_msg)) {
        println!("mediated_blk_read: failed to notify VM 0");
    }
}

pub fn mediated_blk_write(blk_idx: usize, sector: usize, count: usize) {
    let mediated_blk = mediated_blk_list_get(blk_idx);
    let nreq = mediated_blk.nreq();
    mediated_blk.set_nreq(nreq + 1);
    mediated_blk.set_type(VIRTIO_BLK_T_OUT);
    mediated_blk.set_sector(sector);
    mediated_blk.set_count(count);

    let med_msg = HvcDefaultMsg {
        fid: HVC_MEDIATED,
        event: HVC_MEDIATED_DRV_NOTIFY,
    };

    // println!("mediated_blk_write send msg to vm0");
    if !hvc_send_msg_to_vm(0, &HvcGuestMsg::Default(med_msg)) {
        println!("mediated_blk_write: failed to notify VM 0");
    }
}

pub struct UsedInfo {
    pub desc_chain_head_idx: u32,
    pub used_len: u32,
}

pub struct ReadAsyncMsg {
    pub src_vm: Arc<Vm>,
    pub vq: Virtq,
    pub dev: VirtioMmio,
    pub blk_id: usize,
    pub sector: usize,
    pub count: usize,
    pub cache: usize,
    pub iov_list: Arc<Vec<BlkIov>>,
    pub used_info: UsedInfo,
}

pub struct WriteAsyncMsg {
    pub src_vm: Arc<Vm>,
    pub vq: Virtq,
    pub dev: VirtioMmio,
    pub blk_id: usize,
    pub sector: usize,
    pub count: usize,
    pub cache: usize,
    pub buffer: Arc<Mutex<Vec<u8>>>,
    pub used_info: UsedInfo,
}
