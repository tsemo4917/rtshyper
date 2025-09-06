use alloc::vec::Vec;

use vm_fdt::{Error, FdtWriter, FdtWriterResult};

use crate::board::{PlatOperation, Platform};
use crate::config::VmConfigEntry;
use crate::config::{DtbDevType, VmDtbDevConfig};
use crate::device::EmuDeviceType;
use crate::vmm::CPIO_RAMDISK;

pub static SYSTEM_FDT: spin::Once<alloc::vec::Vec<u8>> = spin::Once::new();

pub unsafe fn setup_fdt_vm0(config: &VmConfigEntry, dtb: *mut core::ffi::c_void) -> usize {
    use fdt::*;
    let mut mr = Vec::new();
    for r in config.memory_region() {
        mr.push(region {
            ipa_start: r.ipa_start as u64,
            length: r.length as u64,
        });
    }
    fdt_set_memory(
        dtb,
        mr.len() as u64,
        mr.as_ptr(),
        format!("memory@{:x}\0", mr[0].ipa_start).as_ptr(),
    );
    // FDT+TIMER
    fdt_add_timer(dtb, 0x8);
    // FDT+BOOTCMD
    fdt_set_bootcmd(dtb, config.cmdline.as_ptr());
    #[cfg(feature = "tx2")]
    fdt_set_stdout_path(dtb, "/serial@3100000\0".as_ptr());
    // #[cfg(feature = "pi4")]
    // fdt_set_stdout_path(dtb, "/serial@fe340000\0".as_ptr());

    for emu_cfg in config.emulated_device_list() {
        match emu_cfg.emu_type {
            EmuDeviceType::EmuDeviceTGicd => {
                #[cfg(any(feature = "tx2", feature = "qemu"))]
                fdt_setup_gic(
                    dtb,
                    Platform::GICD_BASE as u64,
                    Platform::GICC_BASE as u64,
                    emu_cfg.name.as_ptr(),
                );
                #[cfg(feature = "pi4")]
                fdt_setup_gic(
                    dtb,
                    (Platform::GICD_BASE | 0xF_0000_0000) as u64,
                    (Platform::GICC_BASE | 0xF_0000_0000) as u64,
                    emu_cfg.name.as_ptr(),
                );
            }
            EmuDeviceType::EmuDeviceTVirtioNet
            | EmuDeviceType::EmuDeviceTVirtioConsole
            | EmuDeviceType::VirtioBalloon => {
                #[cfg(any(feature = "tx2", feature = "qemu"))]
                fdt_add_virtio(
                    dtb,
                    emu_cfg.name.as_ptr(),
                    emu_cfg.irq_id as u32 - 0x20,
                    emu_cfg.base_ipa as u64,
                );
            }
            EmuDeviceType::EmuDeviceTShyper => {
                #[cfg(any(feature = "tx2", feature = "qemu"))]
                fdt_add_vm_service(
                    dtb,
                    emu_cfg.irq_id as u32 - 0x20,
                    emu_cfg.base_ipa as u64,
                    emu_cfg.length as u64,
                );
            }
            EmuDeviceType::EmuDeviceTIOMMU => {
                #[cfg(feature = "tx2")]
                trace!("EmuDeviceTIOMMU");
            }
            _ => {
                todo!();
            }
        }
    }
    let size = fdt_size(dtb) as usize;
    trace!("after dtb size {:#x}", size);
    size
}

pub fn init_vm0_dtb(dtb: *mut core::ffi::c_void) {
    if dtb.is_null() {
        error!("dtb is null");
        return;
    }
    #[cfg(feature = "tx2")]
    unsafe {
        use fdt::*;
        info!("fdt orignal size {}", fdt_size(dtb));
        fdt_pack(dtb);
        fdt_enlarge(dtb);
        let r = fdt_del_mem_rsv(dtb, 0);
        assert_eq!(r, 0);
        // fdt_add_mem_rsv(fdt, 0x80000000, 0x10000000);
        fdt_clear_initrd(dtb);
        let r = fdt_remove_node(dtb, "/cpus/cpu-map/cluster0/core0\0".as_ptr());
        assert_eq!(r, 0);
        let r = fdt_remove_node(dtb, "/cpus/cpu-map/cluster0/core1\0".as_ptr());
        assert_eq!(r, 0);
        let r = fdt_disable_node(dtb, "/cpus/cpu@0\0".as_ptr());
        assert_eq!(r, 0);
        let r = fdt_disable_node(dtb, "/cpus/cpu@1\0".as_ptr());
        assert_eq!(r, 0);
        let r = fdt_disable_node(dtb, "/sdhci@3460000\0".as_ptr());
        assert_eq!(r, 0);
        // fdt_disable_node(dtb, "iommu@12000000\0".as_ptr());
        // assert_eq!(r, 0);
        let r = fdt_disable_node(dtb, "/sdhci@3440000\0".as_ptr());
        assert_eq!(r, 0);
        let r = fdt_disable_node(dtb, "/serial@c280000\0".as_ptr());
        assert_eq!(r, 0);
        let r = fdt_disable_node(dtb, "/serial@3110000\0".as_ptr());
        assert_eq!(r, 0);
        let r = fdt_disable_node(dtb, "/serial@3130000\0".as_ptr());
        assert_eq!(r, 0);
        let r = fdt_disable_node(dtb, "/combined-uart\0".as_ptr());
        assert_eq!(r, 0);
        let r = fdt_disable_node(dtb, "/trusty\0".as_ptr());
        assert_eq!(r, 0);
        let r = fdt_disable_node(dtb, "/host1x/nvdisplay@15210000\0".as_ptr());
        assert_eq!(r, 0);
        let r = fdt_disable_node(dtb, "/reserved-memory/ramoops_carveout\0".as_ptr());
        assert_eq!(r, 0);
        let r = fdt_disable_node(dtb, "/watchdog@30c0000\0".as_ptr());
        assert_eq!(r, 0);
        // disable denver pmu
        let r = fdt_disable_node(dtb, "/denver-pmu\0".as_ptr());
        assert_eq!(r, 0);
        // modify arm pmu
        // Hardcode: here, irq and affi are associated with clurster 1, cpu 0
        cfg_if::cfg_if! {
            if #[cfg(feature = "memory-reservation")] {
                let r = fdt_disable_node(dtb, "/arm-pmu\0".as_ptr());
                assert_eq!(r, 0);
            } else {
                let irq: [u32; 1] = [0x128];
                let affi: [u32; 1] = [0x4];
                let r = fdt_setup_pmu(
                    dtb,
                    "arm,armv8-pmuv3\0".as_ptr(),
                    irq.as_ptr(),
                    irq.len() as u32,
                    affi.as_ptr(),
                    affi.len() as u32,
                );
                assert_eq!(r, 0);
            }
        }
        let len = fdt_size(dtb);
        info!("fdt after patched size {}", len);
        let slice = core::slice::from_raw_parts(dtb as *const u8, len as usize);

        SYSTEM_FDT.call_once(|| slice.to_vec());
    }
    #[cfg(feature = "pi4")]
    unsafe {
        const PI4_DTB_ADDR: usize = 0xf0000000;
        use crate::arch::PAGE_SIZE;
        use crate::util::round_up;
        use fdt::*;
        let pi_fdt = PI4_DTB_ADDR as *mut core::ffi::c_void;
        let len = round_up(fdt_size(pi_fdt) as usize, PAGE_SIZE) + PAGE_SIZE;
        info!("fdt orignal size {}", len);
        let slice = core::slice::from_raw_parts(pi_fdt as *const u8, len as usize);
        SYSTEM_FDT.call_once(|| slice.to_vec());
    }
    #[cfg(feature = "qemu")]
    unsafe {
        use fdt::*;
        info!("fdt orignal size {}, ptr {dtb:#p}", fdt_size(dtb));
        fdt_pack(dtb);
        fdt_enlarge(dtb);
        fdt_clear_initrd(dtb);
        assert_eq!(fdt_disable_node(dtb, "/platform@c000000\0".as_ptr()), 0);

        let remove_node_list = [
            "/fw-cfg@9020000\0",
            "/memory@40000000\0",
            "/virtio_mmio@a000000\0",
            "/virtio_mmio@a000200\0",
            "/virtio_mmio@a000400\0",
            "/virtio_mmio@a000600\0",
            "/virtio_mmio@a000800\0",
            "/virtio_mmio@a000a00\0",
            "/virtio_mmio@a000c00\0",
            "/virtio_mmio@a000e00\0",
            "/virtio_mmio@a001000\0",
            "/virtio_mmio@a001200\0",
            "/virtio_mmio@a001400\0",
            "/virtio_mmio@a001600\0",
            "/virtio_mmio@a001800\0",
            "/virtio_mmio@a001a00\0",
            "/virtio_mmio@a001c00\0",
            "/virtio_mmio@a001e00\0",
            "/virtio_mmio@a002000\0",
            "/virtio_mmio@a002200\0",
            "/virtio_mmio@a002400\0",
            "/virtio_mmio@a002600\0",
            "/virtio_mmio@a002800\0",
            "/virtio_mmio@a002a00\0",
            "/virtio_mmio@a002c00\0",
            "/virtio_mmio@a002e00\0",
            // keep a003000 & a003200 for passthrough blk/net
            // "/virtio_mmio@a003000\0",
            // "/virtio_mmio@a003200\0",
            "/virtio_mmio@a003400\0",
            "/virtio_mmio@a003600\0",
            "/virtio_mmio@a003800\0",
            "/virtio_mmio@a003a00\0",
            "/virtio_mmio@a003c00\0",
            "/virtio_mmio@a003e00\0",
            "/gpio-keys\0",
            "/pl061@9030000\0",
            "/pcie@10000000\0",
            "/pl031@9010000\0",
            // pass through the only one uart on qemu-system-aarch64
            // "/pl011@9000000\0",
            "/intc@8000000/v2m@8020000\0",
            "/flash@0\0",
        ];

        for remove_node in remove_node_list {
            assert_eq!(fdt_remove_node(dtb, remove_node.as_ptr()), 0);
        }

        cfg_if::cfg_if! {
            if #[cfg(feature = "memory-reservation")] {
                let r = fdt_disable_node(dtb, "/pmu\0".as_ptr());
                assert_eq!(r, 0);
            }
        }

        let len = fdt_size(dtb) as usize;
        info!("fdt patched size {}", len);
        let slice = core::slice::from_raw_parts(dtb as *const u8, len);
        SYSTEM_FDT.call_once(|| slice.to_vec());
    }
}

// create vm1 fdt demo
pub fn create_fdt(config: &VmConfigEntry) -> Result<Vec<u8>, Error> {
    let mut fdt = FdtWriter::new()?;

    let root_node = fdt.begin_node("root")?;
    fdt.property_string("compatible", "linux,dummy-virt")?;
    fdt.property_u32("#address-cells", 0x2)?;
    fdt.property_u32("#size-cells", 0x2)?;
    fdt.property_u32("interrupt-parent", 0x8001)?;

    let psci = fdt.begin_node("psci")?;
    fdt.property_string("compatible", "arm,psci-1.0")?;
    fdt.property_string("method", "smc")?;
    fdt.property_array_u32("interrupts", &[0x1, 0x7, 0x4])?;
    fdt.end_node(psci)?;

    create_memory_node(&mut fdt, config)?;
    create_timer_node(&mut fdt, 0x8)?;
    // todo: fix create_chosen_node size
    create_chosen_node(&mut fdt, &config.cmdline, config.ramdisk_load_ipa(), CPIO_RAMDISK.len())?;
    create_cpu_node(&mut fdt, config)?;
    for dev in config.dtb_device_list().iter() {
        if dev.dev_type == DtbDevType::Serial {
            create_serial_node(&mut fdt, dev)?;
        }
    }
    create_gic_node(&mut fdt, config.gicc_addr(), config.gicd_addr())?;

    for emu_cfg in config.emulated_device_list() {
        match emu_cfg.emu_type {
            EmuDeviceType::EmuDeviceTVirtioBlk
            | EmuDeviceType::EmuDeviceTVirtioNet
            | EmuDeviceType::EmuDeviceTVirtioConsole => {
                debug!("virtio fdt node init {} {:x}", emu_cfg.name, emu_cfg.base_ipa);
                create_virtio_node(&mut fdt, &emu_cfg.name, emu_cfg.irq_id, emu_cfg.base_ipa)?;
            }
            EmuDeviceType::EmuDeviceTShyper => {
                debug!("shyper fdt node init {:x}", emu_cfg.base_ipa);
                create_shyper_node(
                    &mut fdt,
                    &emu_cfg.name,
                    emu_cfg.irq_id,
                    emu_cfg.base_ipa,
                    emu_cfg.length,
                )?;
            }
            _ => {}
        }
    }

    fdt.end_node(root_node)?;
    fdt.finish()
}

// hard code for tx2 vm1
fn create_memory_node(fdt: &mut FdtWriter, config: &VmConfigEntry) -> FdtWriterResult<()> {
    if config.memory_region().is_empty() {
        panic!("create_memory_node memory region num 0");
    }
    let memory_name = format!("memory@{:x}", config.memory_region()[0].ipa_start);
    let memory = fdt.begin_node(&memory_name)?;
    fdt.property_string("device_type", "memory")?;
    let mut addr = vec![];
    for region in config.memory_region() {
        addr.push(region.ipa_start as u64);
        addr.push(region.length as u64);
    }
    fdt.property_array_u64("reg", addr.as_slice())?;
    fdt.end_node(memory)?;
    Ok(())
}

fn create_timer_node(fdt: &mut FdtWriter, trigger_lvl: u32) -> FdtWriterResult<()> {
    let timer = fdt.begin_node("timer")?;
    fdt.property_string("compatible", "arm,armv8-timer")?;
    fdt.property_array_u32(
        "interrupts",
        &[
            0x1,
            0xd,
            trigger_lvl,
            0x1,
            0xe,
            trigger_lvl,
            0x1,
            0xb,
            trigger_lvl,
            0x1,
            0xa,
            trigger_lvl,
        ],
    )?;
    fdt.end_node(timer)?;
    Ok(())
}

fn create_cpu_node(fdt: &mut FdtWriter, config: &VmConfigEntry) -> FdtWriterResult<()> {
    let cpus = fdt.begin_node("cpus")?;
    fdt.property_u32("#size-cells", 0)?;
    fdt.property_u32("#address-cells", 0x2)?;

    let cpu_num = config.cpu_allocated_bitmap().count_ones();
    for cpu_id in 0..cpu_num {
        let cpu_name = format!("cpu@{:x}", cpu_id);
        let cpu_node = fdt.begin_node(&cpu_name)?;
        fdt.property_string("compatible", "arm,cortex-a57")?;
        fdt.property_string("device_type", "cpu")?;
        fdt.property_string("enable-method", "psci")?;
        fdt.property_array_u32("reg", &[0, cpu_id])?;
        fdt.end_node(cpu_node)?;
    }

    fdt.end_node(cpus)?;

    Ok(())
}

fn create_serial_node(fdt: &mut FdtWriter, dev: &VmDtbDevConfig) -> FdtWriterResult<()> {
    if dev.dev_type == DtbDevType::Serial {
        let serial_name = format!("serial@{:x}", dev.addr_region.ipa_start);
        let serial = fdt.begin_node(&serial_name)?;
        fdt.property_string("compatible", "ns16550")?;
        fdt.property_array_u64("reg", &[dev.addr_region.ipa_start as u64, 0x1000])?;
        fdt.property_u32("reg-shift", 0x2)?;
        fdt.property_array_u32("interrupts", &[0x0, (dev.irqs[0] - 32) as u32, 0x4])?;
        fdt.property_u32("clock-frequency", 408000000)?;
        // fdt.property_string("status", "disabled")?;
        fdt.end_node(serial)?;
    }
    Ok(())
}

fn create_chosen_node(fdt: &mut FdtWriter, cmdline: &str, ipa: usize, size: usize) -> FdtWriterResult<()> {
    let chosen = fdt.begin_node("chosen")?;
    fdt.property_string("bootargs", cmdline)?;
    fdt.property_u32("linux,initrd-start", ipa as u32)?;
    fdt.property_u32("linux,initrd-end", (ipa + size) as u32)?;
    fdt.end_node(chosen)?;
    Ok(())
}

fn create_gic_node(fdt: &mut FdtWriter, gicc_addr: usize, gicd_addr: usize) -> FdtWriterResult<()> {
    let gic_name = format!("interrupt-controller@{:x}", gicd_addr);
    let gic = fdt.begin_node(&gic_name)?;

    fdt.property_u32("phandle", 0x8001)?;
    fdt.property_array_u64("reg", &[gicd_addr as u64, 0x1000, gicc_addr as u64, 0x2000])?;
    fdt.property_string("compatible", "arm,gic-400")?;
    fdt.property_u32("#interrupt-cells", 0x03)?;
    fdt.property_null("interrupt-controller")?;
    fdt.end_node(gic)?;

    Ok(())
}

fn create_virtio_node(fdt: &mut FdtWriter, name: &str, irq: usize, address: usize) -> FdtWriterResult<()> {
    let virtio = fdt.begin_node(name)?;
    fdt.property_null("dma-coherent")?;
    fdt.property_string("compatible", "virtio,mmio")?;
    fdt.property_array_u32("interrupts", &[0, irq as u32 - 32, 0x1])?;
    fdt.property_array_u64("reg", &[address as u64, 0x400])?;
    fdt.end_node(virtio)?;

    Ok(())
}

fn create_shyper_node(fdt: &mut FdtWriter, name: &str, irq: usize, address: usize, len: usize) -> FdtWriterResult<()> {
    let shyper = fdt.begin_node(name)?;
    fdt.property_string("compatible", "shyper")?;
    fdt.property_array_u32("interrupts", &[0, irq as u32 - 32, 0x1])?;
    if address != 0 && len != 0 {
        fdt.property_array_u64("reg", &[address as u64, len as u64])?;
    }
    fdt.end_node(shyper)?;

    Ok(())
}
