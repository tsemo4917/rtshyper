use alloc::sync::Arc;

use spin::Mutex;

use crate::arch::PAGE_SIZE;
use crate::device::{EmuContext, VirtioMmio, Virtq};
use crate::kernel::vm_by_id;
use crate::kernel::Vm;
use crate::util::round_down;

use super::dev::DevDesc;
use super::iov::VirtioIov;
use super::mmio::VIRTIO_F_VERSION_1;
use super::queue::VIRTIO_RING_F_INDIRECT_DESC;

pub const VIRTQUEUE_CONSOLE_MAX_SIZE: usize = 64;

const VIRTQUEUE_SERIAL_MAX_SIZE: usize = 64;

const VIRTIO_CONSOLE_F_SIZE: usize = 1 << 0;
const VIRTIO_CONSOLE_F_MULTIPORT: usize = 1 << 1;
const VIRTIO_CONSOLE_F_EMERG_WRITE: usize = 1 << 2;

const VIRTIO_CONSOLE_DEVICE_READY: usize = 0;
const VIRTIO_CONSOLE_DEVICE_ADD: usize = 1;
const VIRTIO_CONSOLE_DEVICE_REMOVE: usize = 2;
const VIRTIO_CONSOLE_PORT_READY: usize = 3;
const VIRTIO_CONSOLE_CONSOLE_PORT: usize = 4;
const VIRTIO_CONSOLE_RESIZE: usize = 5;
const VIRTIO_CONSOLE_PORT_OPEN: usize = 6;
const VIRTIO_CONSOLE_PORT_NAME: usize = 7;

pub struct ConsoleDesc {
    inner: Mutex<ConsoleDescInner>,
}

impl ConsoleDesc {
    pub fn new(oppo_end_vmid: u16, oppo_end_ipa: u64) -> ConsoleDesc {
        let mut desc = ConsoleDescInner::default();
        desc.oppo_end_vmid = oppo_end_vmid;
        desc.oppo_end_ipa = oppo_end_ipa;
        desc.cols = 80;
        desc.rows = 25;
        ConsoleDesc {
            inner: Mutex::new(desc),
        }
    }

    fn start_addr(&self) -> usize {
        let inner = self.inner.lock();
        &inner.cols as *const _ as usize
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

    pub fn target_console(&self) -> (u16, u64) {
        let inner = self.inner.lock();
        (inner.oppo_end_vmid, inner.oppo_end_ipa)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ConsoleDescInner {
    oppo_end_vmid: u16,
    oppo_end_ipa: u64,
    // vm access
    cols: u16,
    rows: u16,
    max_nr_ports: u32,
    emerg_wr: u32,
}

impl ConsoleDescInner {
    fn default() -> ConsoleDescInner {
        ConsoleDescInner {
            oppo_end_vmid: 0,
            oppo_end_ipa: 0,
            cols: 0,
            rows: 0,
            max_nr_ports: 0,
            emerg_wr: 0,
        }
    }
}

pub fn console_features() -> usize {
    VIRTIO_F_VERSION_1 | VIRTIO_CONSOLE_F_SIZE | VIRTIO_RING_F_INDIRECT_DESC
}

pub fn virtio_console_notify_handler(vq: Arc<Virtq>, console: Arc<VirtioMmio>, vm: Arc<Vm>) -> bool {
    if vq.vq_indx() % 4 != 1 {
        // println!("console rx queue notified!");
        return true;
    }

    if vq.ready() == 0 {
        println!("virtio_console_notify_handler: console virt_queue is not ready!");
        return false;
    }

    let dev = console.dev();

    let (trgt_vmid, trgt_console_ipa) = match dev.desc() {
        DevDesc::Console(desc) => desc.target_console(),
        _ => {
            println!("virtio_console_notify_handler: console desc should not be None");
            return false;
        }
    };

    while let Some(head_idx) = vq.pop_avail_desc_idx(vq.avail_idx()) {
        let mut len = 0;
        let mut tx_iov = VirtioIov::default();

        let chain = match vq.desc_chain(head_idx, &vm) {
            Ok(chain) => chain,
            Err(e) => {
                println!(
                    "virtio_console_notify_handler: invalid desc chain, head idx {}, err {:?}",
                    head_idx, e
                );
                return false;
            }
        };
        for desc in chain {
            let desc = match desc {
                Ok(desc) => desc,
                Err(e) => {
                    println!(
                        "virtio_console_notify_handler: invalid desc chain, head idx {}, err {:?}",
                        head_idx, e
                    );
                    return false;
                }
            };
            let addr = vm.ipa2hva(desc.addr as usize);
            if addr == 0 {
                println!("virtio_console_notify_handler: failed to desc addr");
                return false;
            }
            tx_iov.push_data(addr, desc.len as usize);

            len += desc.len as usize;
        }

        if !virtio_console_recv(trgt_vmid, trgt_console_ipa, tx_iov, len) {
            println!("virtio_console_notify_handler: failed send");
            // return false;
        }
        if !vq.update_used_ring(len as u32, head_idx as u32) {
            return false;
        }
    }

    if !vq.avail_is_avail() {
        println!("invalid descriptor table index");
        return false;
    }

    console.notify();

    true
}

fn virtio_console_recv(trgt_vmid: u16, trgt_console_ipa: u64, tx_iov: VirtioIov, len: usize) -> bool {
    let trgt_vm = match vm_by_id(trgt_vmid as usize) {
        None => {
            println!("target vm [{}] is not ready or not exist", trgt_vmid);
            return true;
        }
        Some(vm) => vm,
    };

    let console = match trgt_vm
        .find_emu_dev(trgt_console_ipa as usize)
        .and_then(|dev| dev.into_any_arc().downcast::<VirtioMmio>().ok())
    {
        Some(x) => x,
        _ => {
            warn!("virtio_console_recv: trgt_vm[{trgt_vmid}] failed to get virtio console ipa {trgt_console_ipa:x}");
            return true;
        }
    };

    if !console.dev().activated() {
        println!(
            "virtio_console_recv: trgt_vm[{}] virtio console dev is not ready",
            trgt_vmid
        );
        return false;
    }

    let rx_vq = match console.vq(0) {
        Ok(x) => x,
        Err(_) => {
            println!(
                "virtio_console_recv: trgt_vm[{}] failed to get virtio console rx virt queue",
                trgt_vmid
            );
            return false;
        }
    };

    let desc_header_idx_opt = rx_vq.pop_avail_desc_idx(rx_vq.avail_idx());
    if !rx_vq.avail_is_avail() {
        println!("virtio_console_recv: receive invalid avail desc idx");
        return false;
    } else if desc_header_idx_opt.is_none() {
        // println!("virtio_console_recv: desc_header_idx_opt.is_none()");
        return true;
    }

    let desc_idx_header = desc_header_idx_opt.unwrap();
    let mut rx_iov = VirtioIov::default();
    let mut rx_len = 0;
    let chain = match rx_vq.desc_chain(desc_idx_header, &trgt_vm) {
        Ok(chain) => chain,
        Err(e) => {
            println!(
                "virtio_console_recv: invalid desc chain, head idx {}, err {:?}",
                desc_idx_header, e
            );
            return false;
        }
    };
    for desc in chain {
        let desc = match desc {
            Ok(desc) => desc,
            Err(e) => {
                println!(
                    "virtio_console_recv: invalid desc chain, head idx {}, err {:?}",
                    desc_idx_header, e
                );
                return false;
            }
        };
        let dst = trgt_vm.ipa2hva(desc.addr as usize);
        if dst == 0 {
            println!(
                "virtio_console_recv: failed to get dst, avail idx {}",
                rx_vq.avail_idx()
            );
            return false;
        }
        let desc_len = desc.len as usize;
        // dirty pages
        if trgt_vmid != 0 {
            let mut ipa_addr = round_down(desc.addr as usize, PAGE_SIZE);
            while ipa_addr <= round_down(desc.addr as usize + desc_len, PAGE_SIZE) {
                ipa_addr += PAGE_SIZE;
            }
        }
        rx_iov.push_data(dst, desc_len);
        rx_len += desc_len;
        if rx_len >= len {
            break;
        }
    }

    if rx_len < len {
        rx_vq.put_back_avail_desc_idx();
        println!("virtio_console_recv: rx_len smaller than tx_len");
        return false;
    }

    if tx_iov.write_through_iov(&rx_iov, len) > 0 {
        println!(
            "virtio_console_recv: write through iov failed, rx_iov_num {} tx_iov_num {} rx_len {} tx_len {}",
            rx_iov.num(),
            tx_iov.num(),
            rx_len,
            len
        );
        return false;
    }

    if !rx_vq.update_used_ring(len as u32, desc_idx_header as u32) {
        println!(
            "virtio_console_recv: update used ring failed len {} rx_vq num {}",
            len,
            rx_vq.num()
        );
        return false;
    }

    console.notify();
    true
}
