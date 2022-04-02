use core::any::Any;
use core::mem::size_of;

use crate::arch::{PTE_S2_NORMAL, PTE_S2_RO};
use crate::arch::PAGE_SIZE;
use crate::config::*;
use crate::device::{mediated_blk_notify_handler, mediated_dev_append};
use crate::kernel::{
    active_vm, active_vm_id, AllocError, current_cpu, DIRTY_MEM_THRESHOLD, interrupt_vm_inject, ipi_register,
    ipi_send_msg, IpiHvcMsg, IpiInnerMsg, IpiMessage, IpiType, ivc_update_mq, mem_pages_alloc, vm, vm_if_clear_mem_map,
    vm_if_cpy_mem_map, vm_if_dirty_mem_map, vm_if_get_cpu_id, vm_if_get_type, vm_if_ivc_arg, vm_if_ivc_arg_ptr,
    vm_if_mem_map_dirty_sum, vm_if_mem_map_page_num, vm_if_set_ivc_arg_ptr, vm_if_set_mem_map_cache, VM_NUM_MAX,
    VmType,
};
use crate::kernel::IpiInnerMsg::HvcMsg;
use crate::lib::{memcpy_safe, trace};
use crate::mm::PageFrame;
use crate::vmm::{get_vm_id, vmm_init_vm};

// If succeed, return 0.
const HVC_FINISH: usize = 0;
// If failed, return -1.
const HVC_ERR: usize = usize::MAX;

// hvc_fid
pub const HVC_SYS: usize = 0;
pub const HVC_VMM: usize = 1;
pub const HVC_IVC: usize = 2;
pub const HVC_MEDIATED: usize = 3;
pub const HVC_CONFIG: usize = 0x11;

// hvc_sys_event
pub const HVC_SYS_REBOOT: usize = 0;
pub const HVC_SYS_SHUTDOWN: usize = 1;

// hvc_vmm_event
pub const HVC_VMM_LIST_VM: usize = 0;
pub const HVC_VMM_GET_VM_STATE: usize = 1;
pub const HVC_VMM_BOOT_VM: usize = 2;
pub const HVC_VMM_SHUTDOWN_VM: usize = 3;
pub const HVC_VMM_REBOOT_VM: usize = 4;
pub const HVC_VMM_GET_VM_DEF_CFG: usize = 5;
pub const HVC_VMM_GET_VM_CFG: usize = 6;
pub const HVC_VMM_SET_VM_CFG: usize = 7;
pub const HVC_VMM_GET_VM_ID: usize = 8;
pub const HVC_VMM_TRACE_VMEXIT: usize = 9;
pub const HVC_VMM_MIGRATE_VM: usize = 10; // for src vm: send msg to MVM to ask for migrating
pub const HVC_VMM_MIGRATE_MEMCPY: usize = 11; // for sender: copy dirty memory to receiver
pub const HVC_VMM_MIGRATE_INIT_VM: usize = 12; // for receiver: init new vm but not boot

// hvc_ivc_event
pub const HVC_IVC_UPDATE_MQ: usize = 0;
pub const HVC_IVC_SEND_MSG: usize = 1;
pub const HVC_IVC_BROADCAST_MSG: usize = 2;
pub const HVC_IVC_INIT_KEEP_ALIVE: usize = 3;
pub const HVC_IVC_KEEP_ALIVE: usize = 4;
pub const HVC_IVC_ACK: usize = 5;
pub const HVC_IVC_GET_TIME: usize = 6;
pub const HVC_IVC_SEND_SHAREMEM: usize = 0x10;
//共享内存通信
pub const HVC_IVC_GET_SHARED_MEM_IPA: usize = 0x11;
//用于VM获取共享内存IPA
pub const HVC_IVC_SEND_SHAREMEM_TEST_SPEED: usize = 0x12; //共享内存通信速度测试

// hvc_mediated_event
pub const HVC_MEDIATED_DEV_APPEND: usize = 0x30;
pub const HVC_MEDIATED_DEV_NOTIFY: usize = 0x31;
pub const HVC_MEDIATED_DRV_NOTIFY: usize = 0x32;

pub const HVC_IRQ: usize = 32 + 0x20;

#[repr(C)]
pub enum HvcGuestMsg {
    Default(HvcDefaultMsg),
    Migrate(HvcMigrateMsg),
}

#[repr(C)]
pub struct HvcDefaultMsg {
    pub fid: usize,
    pub event: usize,
}

pub const MIGRATE_START: usize = 0;
pub const MIGRATE_COPY: usize = 1;
pub const MIGRATE_FINISH: usize = 2;
#[repr(C)]
pub struct HvcMigrateMsg {
    pub fid: usize,
    pub event: usize,
    pub vm_id: usize,
    pub oper: usize,
    pub page_num: usize, // bitmap page num
}

pub fn hvc_guest_handler(
    hvc_type: usize,
    event: usize,
    x0: usize,
    x1: usize,
    x2: usize,
    x3: usize,
    x4: usize,
    x5: usize,
    x6: usize,
) -> Result<usize, ()> {
    let mut res = false;
    match hvc_type {
        HVC_SYS => {
            res = hvc_sys_handler(event, x0);
        }
        HVC_VMM => {
            return hvc_vmm_handler(event, x0, x1);
        }
        HVC_IVC => {
            res = hvc_ivc_handler(event, x0, x1, x2, x3, x4);
        }
        HVC_MEDIATED => {
            res = hvc_mediated_handler(event, x0, x1, x2, x3);
        }
        HVC_CONFIG => {
            return hvc_config_handler(event, x0, x1, x2, x3, x4, x5, x6);
        }
        _ => {
            println!("hvc_guest_handler: unknown hvc type {} event {}", hvc_type, event);
            return Err(());
        }
    }
    if res {
        Ok(HVC_FINISH)
    } else {
        Err(())
    }
}

fn hvc_config_handler(
    event: usize,
    x0: usize,
    x1: usize,
    x2: usize,
    x3: usize,
    x4: usize,
    x5: usize,
    x6: usize,
) -> Result<usize, ()> {
    match event {
        // HVC_CONFIG_ADD_VM
        0 => vm_cfg_add_vm(x0, x1, x2, x3, x4, x5, x6),
        // HVC_CONFIG_DELETE_VM
        1 => vm_cfg_del_vm(x0),
        // HVC_CONFIG_CPU
        2 => vm_cfg_set_cpu(x0, x1, x2, x3),
        // HVC_CONFIG_MEMORY_REGION
        3 => vm_cfg_add_mem_region(x0, x1, x2),
        // HVC_CONFIG_EMULATED_DEVICE
        4 => vm_cfg_add_emu_dev(x0, x1, x2, x3, x4, x5, x6),
        // HVC_CONFIG_PASSTHROUGH_DEVICE_REGION
        5 => vm_cfg_add_passthrough_device_region(x0, x1, x2, x3),
        // HVC_CONFIG_PASSTHROUGH_DEVICE_IRQS
        6 => vm_cfg_add_passthrough_device_irqs(x0, x1, x2),
        // HVC_CONFIG_PASSTHROUGH_DEVICE_STREAMS_IDS
        7 => vm_cfg_add_passthrough_device_streams_ids(x0, x1, x2),
        // HVC_CONFIG_DTB_DEVICE
        8 => vm_cfg_add_dtb_dev(x0, x1, x2, x3, x4, x5, x6),
        //
        _ => {
            println!("hvc_config_handler unknown event {}", event);
            Err(())
        }
    }
}

fn hvc_sys_handler(event: usize, x0: usize) -> bool {
    true
}

fn hvc_vmm_handler(event: usize, x0: usize, x1: usize) -> Result<usize, ()> {
    match event {
        HVC_VMM_LIST_VM => {
            todo!();
            Ok(0)
        }
        HVC_VMM_GET_VM_STATE => {
            todo!();
            Ok(0)
        }
        HVC_VMM_BOOT_VM => {
            vmm_init_vm(x0, true);
            Ok(0)
        }
        HVC_VMM_SHUTDOWN_VM => {
            todo!();
            Ok(0)
        }
        HVC_VMM_REBOOT_VM => {
            todo!();
            Ok(0)
        }
        HVC_VMM_GET_VM_ID => {
            get_vm_id(x0);
            Ok(0)
        }
        HVC_VMM_MIGRATE_VM => {
            // demo: migration for bma1
            let vm = active_vm().unwrap();
            if vm.id() == 0 {
                println!("migration for mvm is not supported");
            }
            // init vm dirty memory bitmap
            vm_if_dirty_mem_map(vm.id());
            // reset vm stage 2 page table to read only
            vm.pt_read_only();
            println!("bma set dirty mem map and stage 2 page table");

            match vm_if_get_type(vm.id()) {
                VmType::VmTOs => {
                    todo!();
                }
                VmType::VmTBma => {
                    println!("migration for bma");
                    hvc_send_msg_to_vm(
                        0,
                        &HvcGuestMsg::Migrate(HvcMigrateMsg {
                            fid: HVC_VMM,
                            event: HVC_VMM_MIGRATE_VM,
                            vm_id: vm.id(),
                            oper: MIGRATE_START,
                            page_num: 0,
                        }),
                    );
                }
            }
            Ok(0)
        }
        HVC_VMM_MIGRATE_MEMCPY => {
            let dirty_mem_num = vm_if_mem_map_dirty_sum(x0);
            if dirty_mem_num < DIRTY_MEM_THRESHOLD {
                // TODO: FINISH
                // TODO: End live vm, copy dirty mem and vm register struct
            } else {
                // TODO: Copy
            }

            let trgt_vm = vm(x0).unwrap();
            // TODO: should not map every time
            for i in 0..trgt_vm.region_num() {
                // TODO: 0xf00000000 is hard code, need to rewrite
                active_vm()
                    .unwrap()
                    .pt_map_range(0xf00000000, trgt_vm.pa_length(i), trgt_vm.pa_start(i), PTE_S2_RO);
                // TODO: after finish, maybe need to unmap these pages;
            }
            // TODO: should not alloc every time
            match mem_pages_alloc(vm_if_mem_map_page_num(trgt_vm.id())) {
                Ok(pf) => {
                    println!("bitmap size to page num {}", vm_if_mem_map_page_num(trgt_vm.id()));
                    // map dirty bitmap
                    active_vm().unwrap().pt_map_range(
                        0xe00000000,
                        vm_if_mem_map_page_num(trgt_vm.id()),
                        pf.pa(),
                        PTE_S2_RO,
                    );
                    vm_if_set_mem_map_cache(trgt_vm.id(), pf);
                    // copy trgt_vm mem map to kernel module
                    vm_if_cpy_mem_map(trgt_vm.id());
                    vm_if_clear_mem_map(trgt_vm.id());
                }
                Err(_) => {
                    panic!("HVC_VMM_MIGRATE_MEMCPY: mem_pages_alloc failed");
                }
            }
            hvc_send_msg_to_vm(
                0,
                &HvcGuestMsg::Migrate(HvcMigrateMsg {
                    fid: HVC_VMM,
                    event: HVC_VMM_MIGRATE_VM,
                    vm_id: trgt_vm.id(),
                    oper: MIGRATE_COPY,
                    page_num: vm_if_mem_map_page_num(trgt_vm.id()),
                }),
            );
            Ok(0)
        }
        HVC_VMM_MIGRATE_INIT_VM => {
            // TODO: vm_id = 2 is hard code
            vmm_init_vm(2, false);
            let vm = vm(2).unwrap();
            for i in 0..vm.region_num() {
                // TODO: 0xd00000000 is hard code, need to rewrite
                active_vm()
                    .unwrap()
                    .pt_map_range(0xd00000000, vm.pa_length(i), vm.pa_start(i), PTE_S2_NORMAL);
                // TODO: after finish, maybe need to unmap these pages;
                println!(
                    "ipa 0xd00000000, length {:x}, pa start {:x}",
                    vm.pa_length(i),
                    vm.pa_start(i)
                );
            }
            // panic!("end");
            Ok(2)
        }
        _ => {
            println!("hvc_vmm unknown event {}", event);
            Err(())
        }
    }
}

fn hvc_ivc_handler(event: usize, x0: usize, x1: usize, x2: usize, x3: usize, x4: usize) -> bool {
    match event {
        // HVC_IVC_UPDATE_MQ
        0 => {
            return ivc_update_mq(x0, x1);
        }
        _ => {
            println!("hvc_ivc_handler: unknown event {}", event);
            false
        }
    }
}

fn hvc_mediated_handler(event: usize, x0: usize, x1: usize, x2: usize, x3: usize) -> bool {
    match event {
        // HVC_MEDIATED_DEV_APPEND
        48 => {
            // println!("mediated dev_append");
            mediated_dev_append(x0, x1);
        }
        // HVC_DEVICE_USER_NOTIFY
        49 => {
            // println!("mediated notify");
            mediated_blk_notify_handler(x0);
        }
        _ => {
            println!("unknown mediated event {}", event);
            return false;
        }
    }
    true
}

pub fn hvc_send_msg_to_vm(vm_id: usize, guest_msg: &HvcGuestMsg) -> bool {
    let mut target_addr = 0;
    let mut arg_ptr_addr = vm_if_ivc_arg_ptr(vm_id);
    let arg_addr = vm_if_ivc_arg(vm_id);

    if arg_ptr_addr != 0 {
        arg_ptr_addr += PAGE_SIZE / VM_NUM_MAX;
        if arg_ptr_addr - arg_addr >= PAGE_SIZE {
            vm_if_set_ivc_arg_ptr(vm_id, arg_addr);
            target_addr = arg_addr;
        } else {
            vm_if_set_ivc_arg_ptr(vm_id, arg_ptr_addr);
            target_addr = arg_ptr_addr;
        }
    }

    if target_addr == 0 {
        println!("hvc_send_msg_to_vm: target VM{} interface is not prepared", vm_id);
        return false;
    }

    if trace() && (target_addr < 0x1000 || (guest_msg as *const _ as usize) < 0x1000) {
        panic!(
            "illegal des addr {:x}, src addr {:x}",
            target_addr, guest_msg as *const _ as usize
        );
    }
    let (fid, event) = match guest_msg {
        HvcGuestMsg::Default(msg) => {
            memcpy_safe(
                target_addr as *const u8,
                msg as *const _ as *const u8,
                size_of::<HvcDefaultMsg>(),
            );
            (msg.fid, msg.event)
        }
        HvcGuestMsg::Migrate(msg) => {
            memcpy_safe(
                target_addr as *const u8,
                msg as *const _ as *const u8,
                size_of::<HvcMigrateMsg>(),
            );
            (msg.fid, msg.event)
        }
    };

    let cpu_trgt = vm_if_get_cpu_id(vm_id);
    if cpu_trgt != current_cpu().id {
        println!("cpu {} send hvc msg to cpu {}", current_cpu().id, cpu_trgt);
        let ipi_msg = IpiHvcMsg {
            src_vmid: 0,
            trgt_vmid: vm_id,
            fid,
            event,
        };
        if !ipi_send_msg(cpu_trgt, IpiType::IpiTHvc, IpiInnerMsg::HvcMsg(ipi_msg)) {
            println!(
                "hvc_send_msg_to_vm: Failed to send ipi message, target {} type {:#?}",
                cpu_trgt,
                IpiType::IpiTHvc
            );
        }
    } else {
        hvc_guest_notify(vm_id);
        return true;
    }

    true
}

// notify current cpu's vcpu
pub fn hvc_guest_notify(vm_id: usize) {
    let vm = vm(vm_id).unwrap();
    match current_cpu().vcpu_pool().pop_vcpu_through_vmid(vm_id) {
        None => {
            println!(
                "hvc_guest_notify: Core {} failed to find vcpu of VM {}",
                current_cpu().id,
                vm_id
            );
        }
        Some(vcpu) => {
            interrupt_vm_inject(vm.clone(), vcpu.clone(), HVC_IRQ, 0);
        }
    };
}

pub fn hvc_ipi_handler(msg: &IpiMessage) {
    match &msg.ipi_message {
        IpiInnerMsg::HvcMsg(msg) => {
            if current_cpu().vcpu_pool().pop_vcpu_through_vmid(msg.trgt_vmid).is_none() {
                println!(
                    "hvc_ipi_handler: Core {} failed to find vcpu of VM {}",
                    current_cpu().id,
                    msg.trgt_vmid
                );
                return;
            }

            match msg.fid {
                HVC_MEDIATED => {
                    hvc_guest_notify(msg.trgt_vmid);
                }
                HVC_VMM => match msg.event {
                    HVC_VMM_MIGRATE_VM => {
                        println!("vm0 receive ipi and hvc notify");
                        hvc_guest_notify(msg.trgt_vmid);
                    }
                    _ => {}
                },
                _ => {
                    todo!();
                }
            }
        }
        _ => {
            println!("vgic_ipi_handler: illegal ipi");
            return;
        }
    }
}

pub fn hvc_init() {
    if !ipi_register(IpiType::IpiTHvc, hvc_ipi_handler) {
        panic!("hvc_init: failed to register hvc ipi {}", IpiType::IpiTHvc as usize)
    }
}
