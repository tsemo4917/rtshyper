// see virtio 1.1 5.5 Traditional Memory Balloon Device

use alloc::sync::Arc;
use core::{mem::size_of, ops::Deref};

use crate::device::EmuContext;
use crate::kernel::Vm;

use super::{iov::VirtioIov, mmio::VIRTIO_F_VERSION_1, queue::VIRTIO_RING_F_INDIRECT_DESC, VirtioMmio, Virtq};

// Size of a PFN in the balloon interface.
const VIRTIO_BALLOON_PFN_SHIFT: usize = 12;

#[allow(dead_code, non_camel_case_types)]
#[repr(u64)]
enum Features {
    VIRTIO_BALLOON_F_MUST_TELL_HOST = 1 << 0,
    VIRTIO_BALLOON_F_STATS_VQ = 1 << 1,
    VIRTIO_BALLOON_F_DEFLATE_ON_OOM = 1 << 2,
}

impl From<Features> for u64 {
    fn from(feature: Features) -> Self {
        feature as u64
    }
}

pub fn balloon_features() -> usize {
    VIRTIO_F_VERSION_1 | u64::from(Features::VIRTIO_BALLOON_F_MUST_TELL_HOST) as usize | VIRTIO_RING_F_INDIRECT_DESC
    // | u64::from(Features::VIRTIO_BALLOON_F_DEFLATE_ON_OOM) as usize
}

// num_pages configuration field is examined. If this is greater than the actual number of pages, the
// balloon wants more memory from the guest. If it is less than actual, the balloon doesn’t need it all.
#[derive(Debug)]
#[repr(C)]
pub struct VirtioBallonConfig {
    // Number of pages host wants Guest to give up.
    num_pages: u32,
    // Number of pages we've actually got in balloon.
    actual: u32,
}

impl VirtioBallonConfig {
    pub fn new(give_up: usize) -> Self {
        Self {
            num_pages: (give_up >> VIRTIO_BALLOON_PFN_SHIFT) as u32,
            actual: 0,
        }
    }

    pub fn read_config(&self, emu_ctx: &EmuContext, offset: usize) -> u64 {
        if offset < size_of::<Self>() {
            match emu_ctx.width {
                1 => unsafe { *((self as *const _ as usize + offset) as *const u8) as u64 },
                2 => unsafe { *((self as *const _ as usize + offset) as *const u16) as u64 },
                4 => unsafe { *((self as *const _ as usize + offset) as *const u32) as u64 },
                8 => unsafe { *((self as *const _ as usize + offset) as *const u64) },
                _ => 0,
            }
        } else {
            0
        }
    }

    pub fn write_config(&self, emu_ctx: &EmuContext, offset: usize, val: u64) {
        if offset < size_of::<Self>() {
            debug!("before: VirtioBallonConfig {:x?}", self);
            match emu_ctx.width {
                1 => unsafe { *((self as *const _ as usize + offset) as *mut u8) = val as u8 },
                2 => unsafe { *((self as *const _ as usize + offset) as *mut u16) = val as u16 },
                4 => unsafe { *((self as *const _ as usize + offset) as *mut u32) = val as u32 },
                8 => unsafe { *((self as *const _ as usize + offset) as *mut u64) = val },
                _ => {}
            }
            debug!("after: VirtioBallonConfig {:x?}", self);
        }
    }
}

// Virtqueues
// 0 inflateq Apply for memory in the virtual machine, and then release the requested memory
// 1 deflateq Release memory in the virtual machine, the VM gets more memory from the host
// 2 statsq.
// Virtqueue 2 only exists if VIRTIO_BALLON_F_STATS_VQ set.
pub fn virtio_balloon_notify_handler(vq: Arc<Virtq>, balloon: Arc<VirtioMmio>, vm: Arc<Vm>) -> bool {
    if vq.ready() == 0 {
        return false;
    }

    while let Some(next_desc_idx) = vq.pop_avail_desc_idx(vq.avail_idx()) {
        let mut len = 0;
        let mut iov = VirtioIov::default();
        let chain = match vq.desc_chain(next_desc_idx, &vm) {
            Ok(chain) => chain,
            Err(e) => {
                println!(
                    "virtio_balloon_notify_handler: invalid desc chain, head idx {}, err {:?}",
                    next_desc_idx, e
                );
                return false;
            }
        };
        for desc in chain {
            let desc = match desc {
                Ok(desc) => desc,
                Err(e) => {
                    println!(
                        "virtio_balloon_notify_handler: invalid desc chain, head idx {}, err {:?}",
                        next_desc_idx, e
                    );
                    return false;
                }
            };
            let addr = vm.ipa2hva(desc.addr as usize);
            if addr == 0 {
                return false;
            }
            let desc_len = desc.len as usize;
            iov.push_data(addr, desc_len);
            len += desc_len;
        }
        match vq.vq_indx() {
            0 => release_memory_range(&vm, &iov),
            1 => alloc_memory_range(&vm, &iov),
            _ => return false,
        }
        if !vq.update_used_ring(len as u32, next_desc_idx as u32) {
            return false;
        }
    }
    balloon.notify();
    true
}

fn release_memory_range(vm: &Vm, iov: &VirtioIov) {
    for iov_data in iov.iter() {
        debug!("iov data: {:x?}", iov_data);
        for (i, addr) in (iov_data.buf..iov_data.buf + iov_data.len)
            .into_iter()
            .step_by(4)
            .enumerate()
        {
            let pfn = unsafe { *(addr as *const u32) };
            let range_base = (pfn as usize) << VIRTIO_BALLOON_PFN_SHIFT;
            let range_len = 1 << VIRTIO_BALLOON_PFN_SHIFT;
            vm.inflate_balloon(range_base, range_len);
            debug!(
                "release_memory_range: VM [{}] {i}: range_base {range_base:#x}, range_len {range_len:#x}",
                vm.id()
            );
        }
    }
}

// currently unsupported, VIRTIO_BALLOON_F_DEFLATE_ON_OOM is not set
fn alloc_memory_range(vm: &Vm, iov: &VirtioIov) {
    info!("alloc_memory_range: VM {} iov {:x?}", vm.id(), iov.deref());
}

// Memory Statistics Tags
#[allow(dead_code, non_camel_case_types)]
#[repr(u16)]
enum Tags {
    VIRTIO_BALLOON_S_SWAP_IN = 0,
    VIRTIO_BALLOON_S_SWAP_OUT = 1,
    VIRTIO_BALLOON_S_MAJFLT = 2,
    VIRTIO_BALLOON_S_MINFLT = 3,
    VIRTIO_BALLOON_S_MEMFREE = 4,
    VIRTIO_BALLOON_S_MEMTOT = 5,
    VIRTIO_BALLOON_S_AVAIL = 6,
    VIRTIO_BALLOON_S_CACHES = 7,
    VIRTIO_BALLOON_S_HTLB_PGALLOC = 8,
    VIRTIO_BALLOON_S_HTLB_PGFAIL = 9,
}

#[allow(dead_code)]
#[repr(C, packed)]
struct VirtioBalloonStat {
    tag: u16,
    val: u64,
}
