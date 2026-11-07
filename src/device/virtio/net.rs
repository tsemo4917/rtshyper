use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem::size_of;
use spin::Mutex;

use crate::device::{EmuContext, VirtioMmio, Virtq};
use crate::kernel::IpiMessage;
use crate::kernel::Vm;
use crate::kernel::{current_cpu, vm_if_get_cpu_id};
use crate::kernel::{ipi_send_msg, IpiEthernetMsg, IpiInnerMsg, IpiType};

use super::dev::DevDesc;
use super::iov::VirtioIov;
use super::mmio::VIRTIO_F_VERSION_1;
use super::queue::VIRTIO_RING_F_INDIRECT_DESC;

pub const VIRTQUEUE_NET_MAX_SIZE: usize = 256;

const VIRTIO_NET_OK: u8 = 0;
const VIRTIO_NET_ERR: u8 = 1;

const VIRTIO_NET_F_CSUM: usize = 1 << 0;
const VIRTIO_NET_F_GUEST_CSUM: usize = 1 << 1;
const VIRTIO_NET_F_MAC: usize = 1 << 5;
const VIRTIO_NET_F_GSO_DEPREC: usize = 1 << 6;
// deprecated: host handles GSO
const VIRTIO_NET_F_GUEST_TSO4: usize = 1 << 7;
// guest can rcv TSOv4 *
const VIRTIO_NET_F_GUEST_TSO6: usize = 1 << 8;
// guest can rcv TSOv6
const VIRTIO_NET_F_GUEST_ECN: usize = 1 << 9;
// guest can rcv TSO with ECN
const VIRTIO_NET_F_GUEST_UFO: usize = 1 << 10;
// guest can rcv UFO *
const VIRTIO_NET_F_HOST_TSO4: usize = 1 << 11;
// host can rcv TSOv4 *
const VIRTIO_NET_F_HOST_TSO6: usize = 1 << 12;
// host can rcv TSOv6
const VIRTIO_NET_F_HOST_ECN: usize = 1 << 13;
// host can rcv TSO with ECN
const VIRTIO_NET_F_HOST_UFO: usize = 1 << 14;
// host can rcv UFO *
const VIRTIO_NET_F_MRG_RXBUF: usize = 1 << 15;
// host can merge RX buffers *
const VIRTIO_NET_F_STATUS: usize = 1 << 16;
// config status field available *
const VIRTIO_NET_F_CTRL_VQ: usize = 1 << 17;
// control channel available
const VIRTIO_NET_F_CTRL_RX: usize = 1 << 18;
// control channel RX mode support
const VIRTIO_NET_F_CTRL_VLAN: usize = 1 << 19;
// control channel VLAN filtering
const VIRTIO_NET_F_GUEST_ANNOUNCE: usize = 1 << 21; // guest can send gratuitous pkts

const VIRTIO_NET_HDR_F_DATA_VALID: usize = 2;

const VIRTIO_NET_HDR_GSO_NONE: usize = 0;

#[repr(C)]
struct VirtioNetHdr {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
    pub num_buffers: u16,
}

pub struct NetDesc {
    inner: Mutex<NetDescInner>,
}

impl NetDesc {
    pub fn new(mac: &[usize]) -> NetDesc {
        let mut desc = NetDescInner::default();
        for (i, item) in mac.iter().enumerate().take(6) {
            desc.mac[i] = *item as u8;
        }
        NetDesc {
            inner: Mutex::new(desc),
        }
    }

    pub fn set_status(&self, status: u16) {
        let mut inner = self.inner.lock();
        inner.status = status;
    }

    pub fn status(&self) -> u16 {
        let inner = self.inner.lock();
        inner.status
    }

    pub fn offset_data(&self, emu_ctx: &EmuContext, offset: usize) -> u64 {
        let inner = self.inner.lock();
        let start_addr = inner.mac.as_ptr() as usize;
        match emu_ctx.width {
            1 => unsafe { *((start_addr + offset) as *const u8) as u64 },
            2 => unsafe { *((start_addr + offset) as *const u16) as u64 },
            4 => unsafe { *((start_addr + offset) as *const u32) as u64 },
            8 => unsafe { *((start_addr + offset) as *const u64) },
            _ => 0,
        }
    }
}

pub const VIRTIO_NET_S_LINK_UP: u16 = 1;
pub const VIRTIO_NET_S_ANNOUNCE: u16 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct NetDescInner {
    mac: [u8; 6],
    status: u16,
}

impl NetDescInner {
    fn default() -> NetDescInner {
        NetDescInner {
            mac: [0; 6],
            status: VIRTIO_NET_S_LINK_UP,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct VirtioNetCtrlHdr {
    class: u8,
    command: u8,
}

pub fn net_features() -> usize {
    VIRTIO_F_VERSION_1
        | VIRTIO_NET_F_GUEST_CSUM
        | VIRTIO_NET_F_MAC
        | VIRTIO_NET_F_CSUM
        | VIRTIO_NET_F_GUEST_TSO4
        | VIRTIO_NET_F_GUEST_TSO6
        | VIRTIO_NET_F_GUEST_UFO
        | VIRTIO_NET_F_HOST_TSO4
        | VIRTIO_NET_F_HOST_TSO6
        | VIRTIO_NET_F_HOST_UFO
        | VIRTIO_NET_F_HOST_ECN
        | VIRTIO_NET_F_CTRL_VQ
        | VIRTIO_NET_F_GUEST_ANNOUNCE
        | VIRTIO_NET_F_STATUS
        | VIRTIO_RING_F_INDIRECT_DESC
}

const VIRTIO_NET_CTRL_ANNOUNCE: u8 = 3;
const VIRTIO_NET_CTRL_ANNOUNCE_ACK: u8 = 0;

pub fn virtio_net_handle_ctrl(vq: Arc<Virtq>, nic: Arc<VirtioMmio>, vm: Arc<Vm>) -> bool {
    if vq.ready() == 0 {
        println!("virtio net control queue is not ready!");
        return false;
    }

    while let Some(head_idx) = vq.pop_avail_desc_idx(vq.avail_idx()) {
        let mut len = 0;
        let mut out_iov = VirtioIov::default();
        let mut in_iov = VirtioIov::default();

        let chain = match vq.desc_chain(head_idx, &vm) {
            Ok(chain) => chain,
            Err(e) => {
                println!(
                    "virtio_net_handle_ctrl: invalid desc chain, head idx {}, err {:?}",
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
                        "virtio_net_handle_ctrl: invalid desc chain, head idx {}, err {:?}",
                        head_idx, e
                    );
                    return false;
                }
            };
            let addr = vm.ipa2hva(desc.addr as usize);
            if addr == 0 {
                println!("virtio_net_handle_ctrl: failed to desc addr");
                return false;
            }
            if desc.writable() {
                in_iov.push_data(addr, desc.len as usize);
            } else {
                out_iov.push_data(addr, desc.len as usize);
            }
            len += desc.len as usize;
        }
        let ctrl = VirtioNetCtrlHdr::default();
        out_iov.copy_to_buf(&ctrl as *const _ as usize, size_of::<VirtioNetCtrlHdr>());
        match ctrl.class {
            VIRTIO_NET_CTRL_ANNOUNCE => {
                let status: u8 = if ctrl.command == VIRTIO_NET_CTRL_ANNOUNCE_ACK {
                    match nic.dev().desc() {
                        DevDesc::Net(desc) => {
                            desc.set_status(VIRTIO_NET_S_LINK_UP);
                            VIRTIO_NET_OK
                        }
                        _ => {
                            panic!("illegal dev type for nic");
                        }
                    }
                } else {
                    VIRTIO_NET_ERR
                };
                in_iov.copy_from_buf(&status as *const _ as usize, size_of::<u8>());
            }
            _ => {
                println!("Control queue header class can't match {}", ctrl.class);
            }
        }

        // update ctrl queue used ring
        if !vq.update_used_ring(len as u32, head_idx as u32) {
            return false;
        }
    }
    nic.notify();
    true
}

pub fn virtio_net_notify_handler(vq: Arc<Virtq>, nic: Arc<VirtioMmio>, vm: alloc::sync::Arc<Vm>) -> bool {
    if vq.ready() == 0 {
        println!("net virt_queue is not ready!");
        return false;
    }

    if vq.vq_indx() != 1 {
        // println!("net rx queue notified!");
        return true;
    }

    let mut nics_to_notify = vec![];

    while let Some(head_idx) = vq.pop_avail_desc_idx(vq.avail_idx()) {
        let mut len = 0;
        let mut tx_iov = VirtioIov::default();

        let chain = match vq.desc_chain(head_idx, &vm) {
            Ok(chain) => chain,
            Err(e) => {
                println!(
                    "virtio_net_notify_handler: invalid desc chain, head idx {}, err {:?}",
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
                        "virtio_net_notify_handler: invalid desc chain, head idx {}, err {:?}",
                        head_idx, e
                    );
                    return false;
                }
            };
            let addr = vm.ipa2hva(desc.addr as usize);
            if addr == 0 {
                println!("virtio_net_notify_handler: failed to desc addr");
                return false;
            }
            tx_iov.push_data(addr, desc.len as usize);

            len += desc.len as usize;
        }

        if let Some(list) = ethernet_transmit(tx_iov, len, &vm) {
            nics_to_notify.extend(list);
        }

        if !vq.update_used_ring((len - size_of::<VirtioNetHdr>()) as u32, head_idx as u32) {
            return false;
        }
    }

    if !vq.avail_is_avail() {
        println!("invalid descriptor table index");
        return false;
    }

    nic.notify();
    for nic in nics_to_notify {
        let trgt_vm = nic.upper_vm().unwrap();
        let vcpu = trgt_vm.vcpu(0).unwrap();
        if vcpu.phys_id() == current_cpu().id {
            let rx_vq = match nic.vq(0) {
                Ok(x) => x,
                Err(_) => {
                    println!(
                        "virtio_net_notify_handler: vm[{}] failed to get virtio net rx virt queue",
                        vm.id()
                    );
                    return false;
                }
            };
            if rx_vq.ready() != 0 && rx_vq.avail_flags() == 0 {
                nic.notify();
            }
        } else {
            let msg = IpiEthernetMsg { trgt_nic: nic };
            let cpu_trgt = vm_if_get_cpu_id(trgt_vm.id()).unwrap();
            if !ipi_send_msg(cpu_trgt, IpiType::EthernetMsg, IpiInnerMsg::EnternetMsg(msg)) {
                error!(
                    "virtio_net_notify_handler: failed to send ipi message, target {}",
                    cpu_trgt
                );
            }
        }
    }
    true
}

pub fn ethernet_ipi_rev_handler(msg: IpiMessage) {
    match msg.ipi_message {
        IpiInnerMsg::EnternetMsg(ethernet_msg) => {
            let nic = ethernet_msg.trgt_nic;
            let vm = nic.upper_vm().unwrap();
            let rx_vq = match nic.vq(0) {
                Ok(x) => x,
                Err(_) => {
                    println!(
                        "ethernet_ipi_rev_handler: vm[{}] failed to get virtio net rx virt queue",
                        vm.id()
                    );
                    return;
                }
            };

            if rx_vq.ready() != 0 && rx_vq.avail_flags() == 0 {
                nic.notify();
            }
        }
        _ => {
            panic!("illegal ipi message type in ethernet_ipi_rev_handler");
        }
    }
}

fn ethernet_transmit(tx_iov: VirtioIov, len: usize, vm: &Vm) -> Option<Vec<Arc<VirtioMmio>>> {
    // [ destination MAC - 6 ][ source MAC - 6 ][ EtherType - 2 ][ Payload ]
    if len < size_of::<VirtioNetHdr>() || len - size_of::<VirtioNetHdr>() < 6 + 6 + 2 {
        println!(
            "Too short for an ethernet frame, len {}, size of head {}",
            len,
            size_of::<VirtioNetHdr>()
        );
        return None;
    }

    let frame: &[u8] = tx_iov.get_ptr(size_of::<VirtioNetHdr>());
    if frame[0..6] == [0xff, 0xff, 0xff, 0xff, 0xff, 0xff] {
        if ethernet_is_arp(frame) {
            return ethernet_broadcast(&tx_iov, len, vm);
        } else {
            return None;
        }
    }

    if frame[0] == 0x33 && frame[1] == 0x33 {
        if !(frame[12] == 0x86 && frame[13] == 0xdd) {
            // Only IPV6 multicast packet is allowed to be broadcast
            return None;
        }
        return ethernet_broadcast(&tx_iov, len, vm);
    }

    match ethernet_mac_to_nic(frame) {
        Ok(nic) => {
            let vm = nic.upper_vm().unwrap();
            if ethernet_send_to(&vm, &nic, &tx_iov, len) {
                Some(vec![nic])
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

fn ethernet_broadcast(tx_iov: &VirtioIov, len: usize, cur_vm: &Vm) -> Option<Vec<Arc<VirtioMmio>>> {
    let mut nic_list = vec![];
    super::mac::virtio_nic_list_walker(|nic| {
        let vm = nic.upper_vm().unwrap();
        if vm.id() != cur_vm.id() && ethernet_send_to(&vm, nic, tx_iov, len) {
            nic_list.push(nic.clone());
        }
    });
    if nic_list.is_empty() {
        None
    } else {
        Some(nic_list)
    }
}

fn ethernet_send_to(vm: &Vm, nic: &VirtioMmio, tx_iov: &VirtioIov, len: usize) -> bool {
    if !nic.dev().activated() {
        // println!("ethernet_send_to: vm[{}] nic dev is not activate", vmid);
        return false;
    }

    let rx_vq = match nic.vq(0) {
        Ok(x) => x,
        Err(_) => {
            println!(
                "ethernet_send_to: vm[{}] failed to get virtio net rx virt queue",
                vm.id()
            );
            return false;
        }
    };

    let desc_header_idx_opt = rx_vq.pop_avail_desc_idx(rx_vq.avail_idx());
    if !rx_vq.avail_is_avail() {
        println!("ethernet_send_to: receive invalid avail desc idx");
        return false;
    } else if desc_header_idx_opt.is_none() {
        // println!("ethernet_send_to: desc_header_idx_opt is none");
        return false;
    }

    let desc_idx_header = desc_header_idx_opt.unwrap();
    let mut rx_iov = VirtioIov::default();
    let mut rx_len = 0;

    let chain = match rx_vq.desc_chain(desc_idx_header, vm) {
        Ok(chain) => chain,
        Err(e) => {
            println!(
                "ethernet_send_to: invalid desc chain, head idx {}, err {:?}",
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
                    "ethernet_send_to: invalid desc chain, head idx {}, err {:?}",
                    desc_idx_header, e
                );
                return false;
            }
        };
        let dst = vm.ipa2hva(desc.addr as usize);
        if dst == 0 {
            println!(
                "rx_vq desc base table addr {:#x}, avail table addr {:#x}, avail last idx {}",
                rx_vq.desc_table_addr(),
                rx_vq.avail_addr(),
                rx_vq.avail_idx()
            );
            println!("ethernet_send_to: failed to get dst {}", vm.id());
            return false;
        }
        let desc_len = desc.len as usize;

        rx_iov.push_data(dst, desc_len);
        rx_len += desc_len;
        if rx_len >= len {
            break;
        }
    }

    if rx_len < len {
        rx_vq.put_back_avail_desc_idx();
        println!("ethernet_send_to: rx_len smaller than tx_len");
        return false;
    }
    if tx_iov.get_buf(0) < 0x1000 {
        panic!("illegal header addr {}", tx_iov.get_buf(0));
    }
    let header = unsafe { &mut *(tx_iov.get_buf(0) as *mut VirtioNetHdr) };
    header.num_buffers = 1;

    if tx_iov.write_through_iov(&rx_iov, len) > 0 {
        println!(
            "ethernet_send_to: write through iov failed, rx_iov_num {} tx_iov_num {} rx_len {} tx_len {}",
            rx_iov.num(),
            tx_iov.num(),
            rx_len,
            len
        );
        return false;
    }

    if !rx_vq.update_used_ring(len as u32, desc_idx_header as u32) {
        return false;
    }

    true
}

fn ethernet_is_arp(frame: &[u8]) -> bool {
    frame[12] == 0x8 && frame[13] == 0x6
}

fn ethernet_mac_to_nic(frame: &[u8]) -> Result<Arc<VirtioMmio>, ()> {
    let frame_mac = &frame[0..6];
    super::mac::mac_to_nic(frame_mac).ok_or(())
}

pub fn virtio_net_announce(vm: Arc<Vm>) {
    super::mac::virtio_nic_list_walker(|nic| {
        if let Some(nic_vm) = nic.upper_vm() {
            if Arc::ptr_eq(&nic_vm, &vm) {
                nic.notify_config();
            }
        }
    });
}
