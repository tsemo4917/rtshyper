use crate::lib::in_range;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

pub const EMU_DEV_NUM_MAX: usize = 32;
pub static EMU_DEVS_LIST: Mutex<Vec<EmuDevEntry>> = Mutex::new(Vec::new());

use crate::arch::Vgic;
pub enum EmuDevs {
    Vgic(Arc<Vgic>),
    VirtioBlk,
    None,
}

pub struct EmuContext {
    address: usize,
    width: usize,
    write: bool,
    sign_ext: bool,
    reg: usize,
    reg_width: usize,
}

pub struct EmuDevEntry {
    vm_id: usize,
    id: usize,
    ipa: usize,
    size: usize,
    handler: emu_dev_handler,
}

pub enum EmuDeviceType {
    EmuDeviceTConsole,
    EmuDeviceTGicd,
    EmuDeviceTVirtioBlk,
    EmuDeviceTVirtioNet,
    EmuDeviceTShyper,
}

pub type emu_dev_handler = fn(usize, &EmuContext) -> bool;

// TO CHECK
pub fn emu_handler(emu_ctx: &EmuContext) -> bool {
    let ipa = emu_ctx.address;
    let emu_devs_list = EMU_DEVS_LIST.lock();

    for emu_dev in &*emu_devs_list {
        let active_vcpu_lock = crate::kernel::active_vcpu();
        let active_vcpu = active_vcpu_lock.lock();
        if active_vcpu.vm_id() == emu_dev.vm_id && in_range(ipa, emu_dev.ipa, emu_dev.size - 1) {
            return (emu_dev.handler)(emu_dev.id, emu_ctx);
        }
    }
    println!(
        "emu_handler: no emul handler for data abort ipa 0x{:x}",
        ipa
    );
    return false;
}

pub fn emu_register_dev(
    vm_id: usize,
    dev_id: usize,
    address: usize,
    size: usize,
    handler: emu_dev_handler,
) {
    let mut emu_devs_list = EMU_DEVS_LIST.lock();
    if emu_devs_list.len() >= EMU_DEV_NUM_MAX {
        panic!("emu_register_dev: can't register more devs");
    }

    for emu_dev in &*emu_devs_list {
        if vm_id != emu_dev.vm_id {
            continue;
        }
        use crate::lib::in_range;
        if in_range(address, emu_dev.ipa, emu_dev.size - 1)
            || in_range(emu_dev.ipa, address, size - 1)
        {
            panic!("emu_register_dev: duplicated emul address region: prev address 0x{:x} size 0x{:x}, next address 0x{:x} size 0x{:x}", emu_dev.ipa, emu_dev.size, address, size);
        }
    }

    emu_devs_list.push(EmuDevEntry {
        vm_id,
        id: dev_id,
        ipa: address,
        size,
        handler,
    })
}