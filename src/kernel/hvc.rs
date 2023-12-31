use core::mem::size_of;

use crate::arch::PAGE_SIZE;
use crate::device::{mediated_blk_notify_handler, mediated_dev_append};
use crate::kernel::{
    active_vm, current_cpu, interrupt_vm_inject, ipi_send_msg, ivc_update_mq, vm_by_id, vm_if_get_cpu_id,
    vm_if_ivc_arg, vm_if_ivc_arg_ptr, vm_if_set_ivc_arg_ptr, IpiHvcMsg, IpiInnerMsg, IpiMessage, IpiType,
};
use crate::util::memcpy_safe;
use crate::vmm::{get_vm_id, vmm_boot_vm, vmm_list_vm, vmm_reboot_vm, vmm_remove_vm};

use shyper::VM_NUM_MAX;

// If succeed, return 0.
const HVC_FINISH: usize = 0;
// If failed, return -1.
// const HVC_ERR: usize = usize::MAX;

// hvc_fid
pub const HVC_SYS: usize = 0;
pub const HVC_VMM: usize = 1;
pub const HVC_IVC: usize = 2;
pub const HVC_MEDIATED: usize = 3;
pub const HVC_CONFIG: usize = 0x11;
#[cfg(feature = "unilib")]
pub const HVC_UNILIB: usize = 0x12;

// hvc_sys_event
pub const HVC_SYS_REBOOT: usize = 0;
pub const HVC_SYS_SHUTDOWN: usize = 1;
pub const HVC_SYS_UPDATE: usize = 3;
pub const HVC_SYS_TEST: usize = 4;

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
// for src vm: send msg to MVM to ask for migrating
pub const HVC_VMM_MIGRATE_START: usize = 10;
pub const HVC_VMM_MIGRATE_READY: usize = 11;
// for sender: copy dirty memory to receiver
pub const HVC_VMM_MIGRATE_MEMCPY: usize = 12;
pub const HVC_VMM_MIGRATE_FINISH: usize = 13;
// for receiver: init new vm but not boot
pub const HVC_VMM_MIGRATE_INIT_VM: usize = 14;
pub const HVC_VMM_MIGRATE_VM_BOOT: usize = 15;
pub const HVC_VMM_VM_REMOVE: usize = 16;

// hvc_ivc_event
pub const HVC_IVC_UPDATE_MQ: usize = 0;
pub const HVC_IVC_SEND_MSG: usize = 1;
pub const HVC_IVC_BROADCAST_MSG: usize = 2;
pub const HVC_IVC_INIT_KEEP_ALIVE: usize = 3;
pub const HVC_IVC_KEEP_ALIVE: usize = 4;
pub const HVC_IVC_ACK: usize = 5;
pub const HVC_IVC_GET_TIME: usize = 6;
pub const HVC_IVC_SHARE_MEM: usize = 7;
pub const HVC_IVC_SEND_SHAREMEM: usize = 0x10;
//共享内存通信
pub const HVC_IVC_GET_SHARED_MEM_IPA: usize = 0x11;
//用于VM获取共享内存IPA
pub const HVC_IVC_SEND_SHAREMEM_TEST_SPEED: usize = 0x12; //共享内存通信速度测试

// hvc_mediated_event
pub const HVC_MEDIATED_DEV_APPEND: usize = 0x30;
pub const HVC_MEDIATED_DEV_NOTIFY: usize = 0x31;
pub const HVC_MEDIATED_DRV_NOTIFY: usize = 0x32;

cfg_if::cfg_if! {
    if #[cfg(feature = "unilib")] {
        pub const HVC_UNILIB_FS_INIT: usize = 0;
        pub const HVC_UNILIB_FS_OPEN: usize = 1;
        pub const HVC_UNILIB_FS_CLOSE: usize = 2;
        pub const HVC_UNILIB_FS_READ: usize = 3;
        pub const HVC_UNILIB_FS_WRITE: usize = 4;
        pub const HVC_UNILIB_FS_LSEEK: usize = 5;
        pub const HVC_UNILIB_FS_STAT: usize = 6;
        pub const HVC_UNILIB_FS_APPEND: usize = 7;
        pub const HVC_UNILIB_FS_FINISHED: usize = 8;
    }
}

// hvc_config_event
pub const HVC_CONFIG_ADD_VM: usize = 0;
pub const HVC_CONFIG_DELETE_VM: usize = 1;
pub const HVC_CONFIG_CPU: usize = 2;
pub const HVC_CONFIG_MEMORY_REGION: usize = 3;
pub const HVC_CONFIG_EMULATED_DEVICE: usize = 4;
pub const HVC_CONFIG_PASSTHROUGH_DEVICE_REGION: usize = 5;
pub const HVC_CONFIG_PASSTHROUGH_DEVICE_IRQS: usize = 6;
pub const HVC_CONFIG_PASSTHROUGH_DEVICE_STREAMS_IDS: usize = 7;
pub const HVC_CONFIG_DTB_DEVICE: usize = 8;
pub const HVC_CONFIG_UPLOAD_KERNEL_IMAGE: usize = 9;
pub const HVC_CONFIG_MEMORY_COLOR_BUDGET: usize = 10;

#[cfg(feature = "tx2")]
pub const HVC_IRQ: usize = 32 + 0x20;
#[cfg(feature = "pi4")]
pub const HVC_IRQ: usize = 32 + 0x10;
#[cfg(feature = "qemu")]
pub const HVC_IRQ: usize = 32 + 0x20;

#[repr(C)]
pub enum HvcGuestMsg {
    Default(HvcDefaultMsg),
    Manage(HvcManageMsg),
    Migrate(HvcMigrateMsg),
    #[cfg(feature = "unilib")]
    UniLib(HvcUniLibMsg),
}

#[repr(C)]
pub struct HvcDefaultMsg {
    pub fid: usize,
    pub event: usize,
}

#[repr(C)]
pub struct HvcManageMsg {
    pub fid: usize,
    pub event: usize,
    pub vm_id: usize,
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

#[cfg(feature = "unilib")]
#[repr(C)]
pub struct HvcUniLibMsg {
    pub fid: usize,
    pub event: usize,
    pub vm_id: usize,
    pub arg_1: usize,
    pub arg_2: usize,
    pub arg_3: usize,
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
    match hvc_type {
        HVC_SYS => hvc_sys_handler(event, x0),
        HVC_VMM => hvc_vmm_handler(event, x0, x1),
        HVC_IVC => hvc_ivc_handler(event, x0, x1),
        HVC_MEDIATED => hvc_mediated_handler(event, x0, x1),
        HVC_CONFIG => hvc_config_handler(event, x0, x1, x2, x3, x4, x5, x6),
        #[cfg(feature = "unilib")]
        HVC_UNILIB => hvc_unilib_handler(event, x0, x1, x2),
        _ => {
            println!("hvc_guest_handler: unknown hvc type {} event {}", hvc_type, event);
            Err(())
        }
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
    use crate::config;
    match event {
        HVC_CONFIG_ADD_VM => config::add_vm(x0),
        HVC_CONFIG_DELETE_VM => config::del_vm(x0),
        HVC_CONFIG_CPU => config::set_cpu(x0, x1, x2, x3),
        HVC_CONFIG_MEMORY_REGION => config::add_mem_region(x0, x1, x2),
        HVC_CONFIG_EMULATED_DEVICE => config::add_emu_dev(x0, x1, x2, x3, x4, x5, x6),
        HVC_CONFIG_PASSTHROUGH_DEVICE_REGION => config::add_passthrough_device_region(x0, x1, x2, x3),
        HVC_CONFIG_PASSTHROUGH_DEVICE_IRQS => config::add_passthrough_device_irqs(x0, x1, x2),
        HVC_CONFIG_PASSTHROUGH_DEVICE_STREAMS_IDS => config::add_passthrough_device_streams_ids(x0, x1, x2),
        HVC_CONFIG_DTB_DEVICE => config::add_dtb_dev(x0, x1, x2, x3, x4, x5, x6),
        HVC_CONFIG_UPLOAD_KERNEL_IMAGE => config::upload_kernel_image(x0, x1, x2, x3, x4),
        HVC_CONFIG_MEMORY_COLOR_BUDGET => config::set_memory_color_budget(x0, x1, x2, x3),
        _ => {
            println!("hvc_config_handler unknown event {}", event);
            Err(())
        }
    }
}

fn hvc_sys_handler(event: usize, _x0: usize) -> Result<usize, ()> {
    match event {
        HVC_SYS_UPDATE => {
            todo!()
        }
        HVC_SYS_TEST => {
            let vm = active_vm().unwrap();
            crate::device::virtio_net_announce(vm);
            Ok(0)
        }
        _ => Err(()),
    }
}

fn hvc_vmm_handler(event: usize, x0: usize, _x1: usize) -> Result<usize, ()> {
    match event {
        HVC_VMM_LIST_VM => vmm_list_vm(x0),
        HVC_VMM_GET_VM_STATE => {
            error!("unimplemented");
            Err(())
        }
        HVC_VMM_BOOT_VM => {
            vmm_boot_vm(x0);
            Ok(HVC_FINISH)
        }
        HVC_VMM_SHUTDOWN_VM => {
            error!("unimplemented");
            Err(())
        }
        HVC_VMM_REBOOT_VM => {
            vmm_reboot_vm(x0);
            Ok(HVC_FINISH)
        }
        HVC_VMM_GET_VM_ID => {
            get_vm_id(x0);
            Ok(HVC_FINISH)
        }
        HVC_VMM_MIGRATE_START
        | HVC_VMM_MIGRATE_READY
        | HVC_VMM_MIGRATE_MEMCPY
        | HVC_VMM_MIGRATE_INIT_VM
        | HVC_VMM_MIGRATE_VM_BOOT
        | HVC_VMM_MIGRATE_FINISH => {
            error!("unimplemented");
            Ok(HVC_FINISH)
        }
        HVC_VMM_VM_REMOVE => {
            vmm_remove_vm(x0);
            Ok(HVC_FINISH)
        }
        _ => {
            println!("hvc_vmm unknown event {}", event);
            Err(())
        }
    }
}

fn hvc_ivc_handler(event: usize, x0: usize, x1: usize) -> Result<usize, ()> {
    match event {
        HVC_IVC_UPDATE_MQ => {
            if ivc_update_mq(x0, x1) {
                Ok(HVC_FINISH)
            } else {
                Err(())
            }
        }
        HVC_IVC_SHARE_MEM => {
            error!("not support vm migration and live update");
            Ok(HVC_FINISH)
        }
        _ => {
            error!("hvc_ivc_handler: unknown event {}", event);
            Err(())
        }
    }
}

fn hvc_mediated_handler(event: usize, x0: usize, x1: usize) -> Result<usize, ()> {
    match event {
        HVC_MEDIATED_DEV_APPEND => mediated_dev_append(x0, x1),
        HVC_MEDIATED_DEV_NOTIFY => mediated_blk_notify_handler(x0),
        _ => {
            println!("unknown mediated event {}", event);
            Err(())
        }
    }
}

#[cfg(feature = "unilib")]
fn hvc_unilib_handler(event: usize, x0: usize, x1: usize, x2: usize) -> Result<usize, ()> {
    use crate::util::unilib::*;
    match event {
        HVC_UNILIB_FS_INIT => unilib_fs_init(),
        HVC_UNILIB_FS_OPEN => unilib_fs_open(x0, x1, x2),
        HVC_UNILIB_FS_CLOSE => unilib_fs_close(x0),
        HVC_UNILIB_FS_READ => unilib_fs_read(x0, x1, x2),
        HVC_UNILIB_FS_WRITE => unilib_fs_write(x0, x1, x2),
        HVC_UNILIB_FS_LSEEK => unilib_fs_lseek(x0, x1, x2),
        HVC_UNILIB_FS_STAT => unilib_fs_stat(),
        HVC_UNILIB_FS_APPEND => unilib_fs_append(x0),
        HVC_UNILIB_FS_FINISHED => unilib_fs_finished(x0),
        _ => {
            println!("unknown mediated event {}", event);
            Err(())
        }
    }
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

    if target_addr < 0x1000 || (guest_msg as *const _ as usize) < 0x1000 {
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
        HvcGuestMsg::Manage(msg) => {
            memcpy_safe(
                target_addr as *const u8,
                msg as *const _ as *const u8,
                size_of::<HvcManageMsg>(),
            );
            (msg.fid, msg.event)
        }
        #[cfg(feature = "unilib")]
        HvcGuestMsg::UniLib(msg) => {
            memcpy_safe(
                target_addr as *const u8,
                msg as *const _ as *const u8,
                size_of::<HvcUniLibMsg>(),
            );
            (msg.fid, msg.event)
        }
    };

    let cpu_trgt = vm_if_get_cpu_id(vm_id).unwrap();
    if cpu_trgt != current_cpu().id {
        // println!("cpu {} send hvc msg to cpu {}", current_cpu().id, cpu_trgt);
        let ipi_msg = IpiHvcMsg {
            src_vmid: 0,
            trgt_vmid: vm_id,
            fid,
            event,
        };
        if !ipi_send_msg(cpu_trgt, IpiType::Hvc, IpiInnerMsg::HvcMsg(ipi_msg)) {
            error!(
                "hvc_send_msg_to_vm: Failed to send ipi message, target {} type {:#?}",
                cpu_trgt,
                IpiType::Hvc
            );
        }
    } else {
        hvc_guest_notify(vm_id);
    }

    true
}

// notify current cpu's vcpu
pub fn hvc_guest_notify(vm_id: usize) {
    let vm = vm_by_id(vm_id).unwrap();
    match current_cpu().vcpu_array.pop_vcpu_through_vmid(vm_id) {
        None => {
            println!(
                "hvc_guest_notify: Core {} failed to find vcpu of VM {}",
                current_cpu().id,
                vm_id
            );
        }
        Some(vcpu) => {
            interrupt_vm_inject(&vm, vcpu, HVC_IRQ);
        }
    };
}

pub fn hvc_ipi_handler(msg: IpiMessage) {
    match msg.ipi_message {
        IpiInnerMsg::HvcMsg(msg) => {
            if current_cpu().vcpu_array.pop_vcpu_through_vmid(msg.trgt_vmid).is_none() {
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
                    HVC_VMM_MIGRATE_START => {
                        // in mvm
                        hvc_guest_notify(msg.trgt_vmid);
                    }
                    HVC_VMM_MIGRATE_FINISH => {
                        error!("unimplemented");
                    }
                    HVC_VMM_MIGRATE_VM_BOOT => {
                        error!("unimplemented");
                    }
                    _ => {}
                },
                HVC_CONFIG => match msg.event {
                    HVC_CONFIG_UPLOAD_KERNEL_IMAGE => {
                        hvc_guest_notify(msg.trgt_vmid);
                    }
                    _ => {
                        todo!();
                    }
                },
                #[cfg(feature = "unilib")]
                HVC_UNILIB => {
                    hvc_guest_notify(msg.trgt_vmid);
                }
                _ => {
                    todo!();
                }
            }
        }
        _ => {
            println!("vgic_ipi_handler: illegal ipi");
        }
    }
}
