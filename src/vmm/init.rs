use alloc::sync::Arc;

use crate::arch::PAGE_SIZE;
use crate::arch::{PTE_S2_DEVICE, PTE_S2_NORMAL};
use crate::config::VmRegion;
use crate::device::EmuDeviceType::*;
use crate::dtb::{create_fdt, setup_fdt_vm0};
use crate::kernel::access::copy_segment_to_vm;
use crate::kernel::interrupt_vm_register;
use crate::kernel::{
    count_missing_num, current_cpu, iommmu_vm_init, iommu_add_device, ipi_send_msg, mem_region_alloc_colors,
    ColorMemRegion, IpiInnerMsg, IpiType, IpiVmmPercoreMsg, Vm,
};
use crate::vmm::address::vmm_setup_ipa2hva;
use crate::vmm::VmmPercoreEvent;

cfg_if::cfg_if! {
    if #[cfg(feature = "ramdisk")] {
        pub static CPIO_RAMDISK: & [u8] = include_bytes!("../../image/net_rootfs.cpio");
    } else {
        pub static CPIO_RAMDISK: &[u8] = &[];
    }
}

fn vm_map_ipa2color_regions(vm: &Vm, vm_region: &VmRegion, color_regions: &[ColorMemRegion]) {
    // NOTE: continuous ipa should across colors, and the color_regions must be sorted by count
    let missing_list = count_missing_num(color_regions);
    for (i, region) in color_regions.iter().enumerate() {
        for j in 0..region.count {
            let missing_num = missing_list.get(j).unwrap();
            let page_idx = i + j * color_regions.len() - missing_num;
            let ipa = vm_region.ipa_start + page_idx * PAGE_SIZE;
            let pa = region.base + j * region.step;
            vm.pt_map_range(ipa, PAGE_SIZE, pa, PTE_S2_NORMAL, false);
        }
    }
}

fn color_array2bitmap(colors: &[usize]) -> usize {
    if colors.is_empty() {
        usize::MAX
    } else {
        let mut color_bitmap = 0;
        for color in colors {
            color_bitmap |= 1 << *color;
        }
        color_bitmap
    }
}

fn vmm_init_memory(vm: Arc<Vm>) -> bool {
    let config = vm.config();
    // passthrough regions
    for region in vm.config().passthrough_device_regions() {
        if region.dev_property {
            vm.pt_map_range(region.ipa, region.length, region.pa, PTE_S2_DEVICE, true);
        } else {
            vm.pt_map_range(region.ipa, region.length, region.pa, PTE_S2_NORMAL, true);
        }
        debug!(
            "VM {} registers passthrough device: ipa=<{:#x}>, pa=<{:#x}>, size=<{:#x}>, {}",
            vm.id(),
            region.ipa,
            region.pa,
            region.length,
            if region.dev_property { "device" } else { "normal" }
        );
    }
    // normal memory regions
    let vm_memory_regions = config.memory_region();
    let color_bitmap = color_array2bitmap(&config.memory.colors);
    for vm_region in vm_memory_regions.iter() {
        match mem_region_alloc_colors(vm_region.length, color_bitmap) {
            Ok(vm_color_regions) => {
                assert!(!vm_color_regions.is_empty());
                debug!("{:x?}", vm_color_regions);
                vm_map_ipa2color_regions(&vm, vm_region, &vm_color_regions);
                vm.append_color_regions(vm_color_regions);
            }
            Err(_) => {
                error!(
                    "vmm_init_memory: mem_vm_region_alloc_by_colors failed, length {}, color bitmap {:#x}",
                    vm_region.length, color_bitmap
                );
                return false;
            }
        }
    }
    vmm_setup_ipa2hva(vm);

    true
}

fn vmm_load_image(vm: &Vm, bin: &[u8]) {
    copy_segment_to_vm(vm, vm.config().kernel_load_ipa(), bin);
}

struct BinaryInfo<'a> {
    name: &'a str,
    bin: &'a [u8],
}

static IMAGE_LIST: &[BinaryInfo] = &[
    BinaryInfo {
        name: env!("VM0_IMAGE_PATH"),
        bin: include_bytes!(env!("VM0_IMAGE_PATH")),
    },
    #[cfg(feature = "static-config")]
    BinaryInfo {
        name: "Image_vanilla",
        bin: include_bytes!("../../image/Image_vanilla"),
    },
    #[cfg(feature = "unishyper")]
    BinaryInfo {
        name: "Image_Unishyper",
        bin: include_bytes!("../../image/Image_Unishyper"),
    },
];

pub(super) fn vmm_init_image(vm: &Vm) -> bool {
    let vm_id = vm.id();
    let config = vm.config();

    // Only load MVM kernel image "L4T" from binding.
    // Load GVM kernel image from shyper-cli, you may check it for more information.
    match vm.config().kernel_img_name() {
        Some(name) => {
            if let Some(image) = IMAGE_LIST.iter().find(|image| name == image.name) {
                trace!("VM {} loading {}", vm.id(), name);
                vmm_load_image(vm, image.bin);
            } else {
                panic!("Image {} is not supported", name);
            }
        }
        None => {
            info!("VM[{}] is a dynamic configuration", vm_id);
        }
    }

    if config.device_tree_load_ipa() != 0 {
        // Init dtb for Linux.
        if vm_id == 0 {
            // Init dtb for MVM.
            let mut dtb = crate::dtb::SYSTEM_FDT.get().unwrap().clone();
            // enlarge the size of dtb, because vmm_setup_fdt_vm0 will enlarge it unsafely!
            dtb.resize(dtb.len() << 1, 0);
            let size = unsafe { setup_fdt_vm0(vm.config(), dtb.as_ptr() as *mut _) };
            if size >= dtb.len() {
                panic!("unsafe dtb editing!!");
            }
            dtb.resize(size, 0);
            copy_segment_to_vm(vm, config.device_tree_load_ipa(), dtb.as_slice());
        } else {
            // Init dtb for GVM.
            match create_fdt(config) {
                Ok(dtb) => {
                    copy_segment_to_vm(vm, config.device_tree_load_ipa(), dtb.as_slice());
                }
                _ => {
                    panic!("vmm_setup_config: create fdt for vm{} fail", vm.id());
                }
            }
        }
    } else {
        warn!("VM {} id {} device tree load ipa is not set", vm_id, vm.config().name);
    }

    // ...
    // Todo: support loading ramdisk from MVM shyper-cli.
    // ...
    #[cfg(feature = "ramdisk")]
    if config.ramdisk_load_ipa() != 0 {
        info!("VM {} use ramdisk CPIO_RAMDISK", vm_id);
        copy_segment_to_vm(vm, config.ramdisk_load_ipa(), CPIO_RAMDISK);
    }

    true
}

fn vmm_init_hardware(vm: &Vm) -> bool {
    // init passthrough irqs
    for irq in vm.config().passthrough_device_irqs() {
        if !interrupt_vm_register(vm, *irq, true) {
            return false;
        }
    }
    // init iommu
    for emu_cfg in vm.config().emulated_device_list().iter() {
        if emu_cfg.emu_type == EmuDeviceTIOMMU {
            if !iommmu_vm_init(vm) {
                return false;
            } else {
                break;
            }
        }
    }
    for stream_id in vm.config().passthrough_device_stread_ids() {
        if *stream_id == 0 {
            break;
        }
        if !iommu_add_device(vm, *stream_id) {
            return false;
        }
    }
    true
}

/* Setup VM Configuration before boot.
 * Only VM0 will call this function.
 * This func should run 1 time for each vm.
 *
 * @param[in] vm_id: target VM id to set up config.
 */
pub fn vmm_setup_config(vm: Arc<Vm>) {
    trace!(
        "vmm_setup_config VM[{}] name {:?} current core {}",
        vm.id(),
        vm.config().name,
        current_cpu().id
    );
    // need ipi, must after push to global list
    vmm_init_cpu(vm.clone());
    // need ipi, must after push to global list
    if !vmm_init_memory(vm.clone()) {
        panic!("vmm_setup_config: vmm_init_memory failed");
    }
    // need memory, must after init memory
    if !vmm_init_image(&vm) {
        panic!("vmm_setup_config: vmm_init_image failed");
    }
    if !vmm_init_hardware(&vm) {
        panic!("vmm_setup_config: vmm_init_hardware failed");
    }

    info!("VM {} id {} init ok", vm.id(), vm.config().name);
}

fn vmm_init_cpu(vm: Arc<Vm>) {
    let vm_id = vm.id();
    trace!("vmm_init_cpu: set up vm {} on cpu {}", vm_id, current_cpu().id);
    info!(
        "VM {} init cpu: cores=<{}>, allocat_bits=<{:#b}>",
        vm.id(),
        vm.config().cpu_num(),
        vm.config().cpu_allocated_bitmap()
    );

    for vcpu in vm.vcpu_list() {
        let target_cpu_id = vcpu.phys_id();
        if target_cpu_id != current_cpu().id {
            let m = IpiVmmPercoreMsg {
                vm: vm.clone(),
                event: VmmPercoreEvent::AssignCpu,
            };
            if !ipi_send_msg(target_cpu_id, IpiType::Vmm, IpiInnerMsg::VmmPercoreMsg(m)) {
                error!("vmm_init_cpu: failed to send ipi to Core {}", target_cpu_id);
            }
        } else {
            vmm_assign_vcpu_percore(&vm);
        }
    }
    info!("vmm_init_cpu: VM [{}] is ready", vm_id);
}

pub fn vmm_assign_vcpu_percore(vm: &Vm) {
    let cpu_id = current_cpu().id;
    if current_cpu().assigned() {
        trace!("vmm_cpu_assign_vcpu vm[{}] cpu {} is assigned", vm.id(), cpu_id);
    }

    for vcpu in vm.vcpu_list() {
        if vcpu.phys_id() == current_cpu().id {
            if vcpu.id() == 0 {
                info!("* Core {} is assigned => vm {}, vcpu {}", cpu_id, vm.id(), vcpu.id());
            } else {
                info!("Core {} is assigned => vm {}, vcpu {}", cpu_id, vm.id(), vcpu.id());
            }
            current_cpu().vcpu_array.append_vcpu(vcpu.clone());
            break;
        }
    }
}

pub fn vm_init() {
    if current_cpu().id == 0 {
        // Set up basic config.
        if cfg!(feature = "unishyper") {
            #[cfg(feature = "tx2")]
            crate::config::unishyper_config_init();
        } else {
            crate::config::mvm_config_init();
        }
        // Add VM 0
        super::vmm_init_gvm(0);
        #[cfg(feature = "static-config")]
        {
            crate::config::init_tmp_config_for_vm1();
            crate::config::init_tmp_config_for_vm2();
            super::vmm_init_gvm(1);
            super::vmm_init_gvm(2);
        }
    }
}
