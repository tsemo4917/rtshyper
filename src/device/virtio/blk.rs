use alloc::ffi::CString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem::{align_of, size_of};
use spin::Mutex;

use crate::arch::PAGE_SIZE;
use crate::device::{mediated_blk_list_get, EmuContext, ReadAsyncMsg, UsedInfo, VirtioMmio, Virtq, WriteAsyncMsg};
use crate::kernel::{async_blk_io_req, async_ipi_req, AsyncTask, IpiMediatedMsg, Vm, EXECUTOR};
use crate::util::memcpy_safe;

use super::mmio::VIRTIO_F_VERSION_1;
use super::queue::VIRTIO_RING_F_INDIRECT_DESC;

pub const VIRTQUEUE_BLK_MAX_SIZE: usize = 256;

/* VIRTIO_BLK_FEATURES*/
const VIRTIO_BLK_F_SIZE_MAX: usize = 1 << 1;
const VIRTIO_BLK_F_SEG_MAX: usize = 1 << 2;

/* BLOCK PARAMETERS*/
pub const SECTOR_BSIZE: usize = 512;
pub const BLOCKIF_SIZE_MAX: usize = 128 * PAGE_SIZE;
pub const BLOCKIF_IOV_MAX: usize = 512;

/* BLOCK REQUEST TYPE*/
pub const VIRTIO_BLK_T_IN: usize = 0;
pub const VIRTIO_BLK_T_OUT: usize = 1;
pub const VIRTIO_BLK_T_FLUSH: usize = 4;
pub const VIRTIO_BLK_T_GET_ID: usize = 8;

/* BLOCK REQUEST STATUS*/
pub const VIRTIO_BLK_S_OK: usize = 0;
// pub const VIRTIO_BLK_S_IOERR: usize = 1;
pub const VIRTIO_BLK_S_UNSUPP: usize = 2;

pub fn blk_features() -> usize {
    VIRTIO_F_VERSION_1 | VIRTIO_BLK_F_SIZE_MAX | VIRTIO_BLK_F_SEG_MAX | VIRTIO_RING_F_INDIRECT_DESC
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct BlkGeometry {
    cylinders: u16,
    heads: u8,
    sectors: u8,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct BlkTopology {
    // # of logical blocks per physical block (log2)
    physical_block_exp: u8,
    // offset of first aligned logical block
    alignment_offset: u8,
    // suggested minimum I/O size in blocks
    min_io_size: u16,
    // optimal (suggested maximum) I/O size in blocks
    opt_io_size: u32,
}

pub struct BlkDesc {
    inner: BlkDescInner,
}

impl BlkDesc {
    pub fn new(bsize: usize) -> BlkDesc {
        let desc = BlkDescInner {
            capacity: bsize,
            size_max: BLOCKIF_SIZE_MAX as u32,
            seg_max: BLOCKIF_IOV_MAX as u32,
            ..Default::default()
        };
        BlkDesc { inner: desc }
    }

    fn start_addr(&self) -> usize {
        &self.inner.capacity as *const _ as usize
    }

    pub fn offset_data(&self, emu_ctx: &EmuContext, offset: usize) -> u64 {
        let start_addr = self.start_addr();
        match emu_ctx.width {
            1 => unsafe { *((start_addr + offset) as *const u8) as u64 },
            2 => unsafe { *((start_addr + offset) as *const u16) as u64 },
            4 => unsafe { *((start_addr + offset) as *const u32) as u64 },
            8 => unsafe { *((start_addr + offset) as *const u64) },
            _ => 0,
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct BlkDescInner {
    capacity: usize,
    size_max: u32,
    seg_max: u32,
    geometry: BlkGeometry,
    blk_size: usize,
    topology: BlkTopology,
    writeback: u8,
    unused0: [u8; 3],
    max_discard_sectors: u32,
    max_discard_seg: u32,
    discard_sector_alignment: u32,
    max_write_zeroes_sectors: u32,
    max_write_zeroes_seg: u32,
    write_zeroes_may_unmap: u8,
    unused1: [u8; 3],
}

#[repr(C)]
#[derive(Clone)]
pub struct BlkIov {
    pub data_bg: usize,
    pub len: u32,
}

#[repr(C)]
struct BlkReqRegion {
    pub start: usize,
    pub size: usize,
}

#[repr(C)]
pub struct VirtioBlkReq {
    region: BlkReqRegion,
    mediated: bool,
}

impl VirtioBlkReq {
    pub fn default() -> VirtioBlkReq {
        VirtioBlkReq {
            region: BlkReqRegion { start: 0, size: 0 },
            mediated: false,
        }
    }

    pub fn set_start(&mut self, start: usize) {
        self.region.start = start;
    }

    pub fn set_size(&mut self, size: usize) {
        self.region.size = size;
    }

    pub fn set_mediated(&mut self, mediated: bool) {
        self.mediated = mediated;
    }

    pub fn mediated(&self) -> bool {
        self.mediated
    }

    pub fn region_start(&self) -> usize {
        self.region.start
    }

    pub fn region_size(&self) -> usize {
        self.region.size
    }
}

/// On-wire virtio-blk request header (virtio spec: type, reserved, sector).
#[repr(C)]
struct BlkReqHeader {
    req_type: u32,
    _reserved: u32,
    sector: u64,
}

#[repr(C)]
#[derive(Clone)]
pub struct VirtioBlkReqNode {
    req_type: u32,
    reserved: u32,
    sector: usize,
    desc_chain_head_idx: u32,
    iov: Vec<BlkIov>,
    // sum up byte for req
    iov_sum_up: usize,
    // total byte for current req
    iov_total: usize,
}

impl VirtioBlkReqNode {
    pub fn default() -> VirtioBlkReqNode {
        VirtioBlkReqNode {
            req_type: 0,
            reserved: 0,
            sector: 0,
            desc_chain_head_idx: 0,
            iov: vec![],
            iov_sum_up: 0,
            iov_total: 0,
        }
    }
}

fn generate_blk_req(
    req: &VirtioBlkReq,
    vq: Arc<Virtq>,
    dev: Arc<VirtioMmio>,
    cache: usize,
    vm: Arc<Vm>,
    req_node_list: Vec<VirtioBlkReqNode>,
) {
    let region_start = req.region_start();
    let region_size = req.region_size();
    let mut cache_ptr = cache;
    for req_node in req_node_list {
        let sector = req_node.sector;
        if sector + req_node.iov_sum_up / SECTOR_BSIZE > region_start + region_size {
            println!(
                "blk_req_handler: {} out of vm range",
                if req_node.req_type == VIRTIO_BLK_T_IN as u32 {
                    "read"
                } else {
                    "write"
                }
            );
            continue;
        }
        match req_node.req_type as usize {
            VIRTIO_BLK_T_IN => {
                if req.mediated() {
                    // mediated blk read
                    let task = AsyncTask::new(
                        ReadAsyncMsg {
                            src_vm: vm.clone(),
                            vq: vq.clone(),
                            dev: dev.clone(),
                            blk_id: vm.med_blk_id(),
                            sector: sector + region_start,
                            count: req_node.iov_sum_up / SECTOR_BSIZE,
                            cache,
                            iov_list: Arc::new(req_node.iov),
                            used_info: UsedInfo {
                                desc_chain_head_idx: req_node.desc_chain_head_idx,
                                used_len: req_node.iov_total as u32,
                            },
                        },
                        vm.id(),
                        async_blk_io_req(),
                    );
                    EXECUTOR.add_task(task, false);
                } else {
                    for iov in req_node.iov.iter() {
                        let data_bg = iov.data_bg;
                        let len = iov.len as usize;

                        if len < SECTOR_BSIZE {
                            println!("blk_req_handler: read len < SECTOR_BSIZE");
                            continue;
                        }
                        memcpy_safe(data_bg as *mut u8, cache_ptr as *mut u8, len);
                        cache_ptr += len;
                    }
                }
            }
            VIRTIO_BLK_T_OUT => {
                if req.mediated() {
                    let mut buffer = vec![];
                    for iov in req_node.iov.iter() {
                        let data_bg =
                            unsafe { core::slice::from_raw_parts(iov.data_bg as *const u8, iov.len as usize) };
                        buffer.extend_from_slice(data_bg);
                    }
                    // mediated blk write
                    let task = AsyncTask::new(
                        WriteAsyncMsg {
                            src_vm: vm.clone(),
                            vq: vq.clone(),
                            dev: dev.clone(),
                            blk_id: vm.med_blk_id(),
                            sector: sector + region_start,
                            count: req_node.iov_sum_up / SECTOR_BSIZE,
                            cache,
                            buffer: Arc::new(Mutex::new(buffer)),
                            used_info: UsedInfo {
                                desc_chain_head_idx: req_node.desc_chain_head_idx,
                                used_len: req_node.iov_total as u32,
                            },
                        },
                        vm.id(),
                        async_blk_io_req(),
                    );
                    EXECUTOR.add_task(task, false);
                } else {
                    for iov in req_node.iov.iter() {
                        let data_bg = iov.data_bg;
                        let len = iov.len as usize;
                        if len < SECTOR_BSIZE {
                            println!("blk_req_handler: read len < SECTOR_BSIZE");
                            continue;
                        }
                        memcpy_safe(cache_ptr as *mut u8, data_bg as *mut u8, len);
                        cache_ptr += len;
                    }
                }
            }
            VIRTIO_BLK_T_FLUSH => {
                todo!();
            }
            VIRTIO_BLK_T_GET_ID => {
                let name = CString::new("virtio-blk").unwrap();
                let cstr = name.to_bytes_with_nul();
                let data_bg =
                    unsafe { core::slice::from_raw_parts_mut(req_node.iov[0].data_bg as *mut u8, cstr.len()) };
                data_bg.copy_from_slice(cstr);
                if !vq.update_used_ring(req_node.iov_total as u32, req_node.desc_chain_head_idx) {
                    println!("blk_req_handler: fail to update used ring");
                }
                dev.notify();
            }
            _ => {
                println!("Wrong block request type {} ", req_node.req_type);
                continue;
            }
        }

        // update used ring
        if !req.mediated() {
            todo!("reset num to vq size");
            // if !vq.update_used_ring(req_node.iov_total as u32, req_node.desc_chain_head_idx as u32) {
            //     println!("blk_req_handler: fail to update used ring");
            // }
        }
    }
}

pub fn virtio_mediated_blk_notify_handler(vq: Arc<Virtq>, blk: Arc<VirtioMmio>, vm: Arc<Vm>) -> bool {
    let src_vmid = vm.id();
    let task = AsyncTask::new(IpiMediatedMsg { src_vm: vm, vq, blk }, src_vmid, async_ipi_req());
    EXECUTOR.add_task(task, true);
    true
}

pub fn virtio_blk_notify_handler(vq: Arc<Virtq>, blk: Arc<VirtioMmio>, vm: Arc<Vm>) -> bool {
    let avail_idx = vq.avail_idx();

    // let begin = time_current_us();
    if vq.ready() == 0 {
        println!("blk virt_queue is not ready!");
        return false;
    }

    // let mediated = blk.mediated();
    let dev = blk.dev();
    let req = match dev.req() {
        Some(blk_req) => blk_req,
        _ => {
            panic!("virtio_blk_notify_handler: illegal req");
        }
    };

    let mut req_node_list = vec![];
    let mut process_count: i32 = 0;
    // let mut desc_chain_head_idx;

    // let time0 = time_current_us();

    while let Some(head_idx) = vq.pop_avail_desc_idx(avail_idx) {
        vq.disable_notify();
        if vq.check_avail_idx(avail_idx) {
            vq.enable_notify();
        }

        let mut chain = match vq.desc_chain(head_idx, &vm) {
            Ok(chain) => chain,
            Err(e) => {
                println!(
                    "virtio_blk_notify_handler: invalid desc chain, head idx {}, err {:?}",
                    head_idx, e
                );
                blk.notify();
                return false;
            }
        };

        let mut req_node = VirtioBlkReqNode::default();
        req_node.desc_chain_head_idx = head_idx as u32;

        // header descriptor; a valid chain is at least header + status
        let header = match chain.next() {
            Some(Ok(desc)) => desc,
            Some(Err(e)) => {
                println!(
                    "virtio_blk_notify_handler: invalid desc chain, head idx {}, err {:?}",
                    head_idx, e
                );
                blk.notify();
                return false;
            }
            None => {
                println!("virtio_blk_notify_handler: desc chain too short, head idx {}", head_idx);
                blk.notify();
                return false;
            }
        };
        if header.writable() {
            println!(
                "Failed to get virt blk queue desc header, idx = {}, flag = {:x}",
                head_idx, header.flags
            );
            blk.notify();
            return false;
        }
        let vreq_addr = vm.ipa2hva(header.addr as usize);
        if vreq_addr == 0
            || (header.len as usize) < size_of::<BlkReqHeader>()
            || vreq_addr % align_of::<BlkReqHeader>() != 0
        {
            println!("virtio_blk_notify_handler: failed to get vreq");
            return false;
        }
        let vreq = unsafe { &*(vreq_addr as *const BlkReqHeader) };
        req_node.req_type = vreq.req_type;
        req_node.sector = vreq.sector as usize;

        // Hold the latest descriptor as the status candidate. Every time
        // another one follows, the candidate is a data descriptor instead.
        let mut status = match chain.next() {
            Some(Ok(desc)) => desc,
            Some(Err(e)) => {
                println!(
                    "virtio_blk_notify_handler: invalid desc chain, head idx {}, err {:?}",
                    head_idx, e
                );
                blk.notify();
                return false;
            }
            None => {
                println!("virtio_blk_notify_handler: desc chain too short, head idx {}", head_idx);
                blk.notify();
                return false;
            }
        };
        loop {
            let next = match chain.next() {
                Some(Ok(desc)) => desc,
                Some(Err(e)) => {
                    println!(
                        "virtio_blk_notify_handler: invalid desc chain, head idx {}, err {:?}",
                        head_idx, e
                    );
                    blk.notify();
                    return false;
                }
                None => break,
            };
            if status.writable() as u32 == req_node.req_type {
                println!(
                    "Failed to get virt blk queue desc data, req.type = {}, desc.flags = {}",
                    req_node.req_type, status.flags
                );
                blk.notify();
                return false;
            }
            let data_bg = vm.ipa2hva(status.addr as usize);
            if data_bg == 0 {
                println!("virtio_blk_notify_handler: failed to get iov data begin");
                return false;
            }

            let iov = BlkIov {
                data_bg,
                len: status.len,
            };
            req_node.iov_sum_up += iov.len as usize;
            req_node.iov.push(iov);
            status = next;
        }

        // status descriptor
        if !status.writable() {
            println!("Failed to get virt blk queue desc status, idx = {}", head_idx);
            blk.notify();
            return false;
        }
        let vstatus_addr = vm.ipa2hva(status.addr as usize);
        if vstatus_addr == 0 {
            println!("virtio_blk_notify_handler: vm[{}] failed to vstatus", vm.id());
            return false;
        }
        let vstatus = unsafe { &mut *(vstatus_addr as *mut u8) };
        if req_node.req_type > 1 && req_node.req_type != VIRTIO_BLK_T_GET_ID as u32 {
            *vstatus = VIRTIO_BLK_S_UNSUPP as u8;
        } else {
            *vstatus = VIRTIO_BLK_S_OK as u8;
        }

        req_node.iov_total = req_node.iov_sum_up;
        // req.add_req_node(req_node, &vm);
        req_node_list.push(req_node);

        process_count += 1;
    }

    if !req.mediated() {
        // generate_blk_req(&req, &vq, &blk, dev.cache(), &vm);
        unimplemented!("!req.mediated()");
    } else {
        let mediated_blk = mediated_blk_list_get(vm.med_blk_id());
        let cache = mediated_blk.cache_pa();
        generate_blk_req(req, vq.clone(), blk.clone(), cache, vm, req_node_list);
    };

    // let time1 = time_current_us();

    if vq.avail_flags() == 0 && process_count > 0 && !req.mediated() {
        println!("virtio blk notify");
        blk.notify();
    }

    // let end = time_current_us();
    // println!("init time {}us, while handle desc ring time {}us, finish task {}us", time0 - begin, time1 - time0, end - time1);
    true
}
