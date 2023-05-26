use core::ffi::CStr;
use core::ops::Range;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use spin::Mutex;

// use crate::board::*;
use crate::device::{EmuDeviceType, mediated_blk_free, mediated_blk_request};
use crate::kernel::{active_vm, vm, Vm, VmType, CONFIG_VM_NUM_MAX};
use crate::util::{BitAlloc, BitAlloc16};
use crate::vmm::vmm_init_gvm;
use crate::kernel::access::{copy_segment_from_vm, copy_between_vm};

const CFG_MAX_NUM: usize = 0x10;
// const IRQ_MAX_NUM: usize = 0x40;
// const PASSTHROUGH_DEV_MAX_NUM: usize = 128;
// const EMULATED_DEV_MAX_NUM: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DtbDevType {
    Serial = 0,
    Gicd = 1,
    Gicc = 2,
}

impl From<usize> for DtbDevType {
    fn from(value: usize) -> Self {
        match value {
            0 => Self::Serial,
            1 => Self::Gicd,
            2 => Self::Gicc,
            _ => panic!("Unknown DtbDevType value: {}", value),
        }
    }
}

#[derive(Clone)]
pub struct VmEmulatedDeviceConfig {
    pub name: String,
    pub base_ipa: usize,
    pub length: usize,
    pub irq_id: usize,
    pub cfg_list: Vec<usize>,
    pub emu_type: EmuDeviceType,
    pub mediated: bool,
}

#[derive(Clone, Default)]
pub struct VmEmulatedDeviceConfigList {
    pub emu_dev_list: Vec<VmEmulatedDeviceConfig>,
}

#[derive(Clone, Debug)]
pub struct PassthroughRegion {
    pub ipa: usize,
    pub pa: usize,
    pub length: usize,
    pub dev_property: bool,
}

#[derive(Default, Clone)]
pub struct VmPassthroughDeviceConfig {
    pub regions: Vec<PassthroughRegion>,
    pub irqs: Vec<usize>,
    pub streams_ids: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct VmRegion {
    pub ipa_start: usize,
    pub length: usize,
}

impl VmRegion {
    pub fn as_range(&self) -> Range<usize> {
        self.ipa_start..(self.ipa_start + self.length)
    }
}

#[derive(Clone, Default)]
pub struct VmMemoryConfig {
    pub region: Vec<VmRegion>,
    pub colors: Vec<usize>,
}

#[derive(Clone)]
pub struct VmImageConfig {
    pub kernel_img_name: Option<&'static str>,
    pub kernel_load_ipa: usize,
    pub kernel_entry_point: usize,
    // pub device_tree_filename: Option<&'static str>,
    pub device_tree_load_ipa: usize,
    // pub ramdisk_filename: Option<&'static str>,
    pub ramdisk_load_ipa: usize,
}

impl VmImageConfig {
    pub fn new(kernel_load_ipa: usize, device_tree_load_ipa: usize, ramdisk_load_ipa: usize) -> VmImageConfig {
        VmImageConfig {
            kernel_img_name: None,
            kernel_load_ipa,
            kernel_entry_point: kernel_load_ipa,
            // device_tree_filename: None,
            device_tree_load_ipa,
            // ramdisk_filename: None,
            ramdisk_load_ipa,
        }
    }
}

#[derive(Clone, Default)]
pub struct VmCpuConfig {
    pub num: usize,
    pub allocate_bitmap: u32,
    pub master: Option<usize>,
}

#[derive(Clone)]
pub struct VmDtbDevConfig {
    pub name: String,
    pub dev_type: DtbDevType,
    pub irqs: Vec<usize>,
    pub addr_region: VmRegion,
}

#[derive(Clone, Default)]
pub struct VMDtbDevConfigList {
    pub dtb_device_list: Vec<VmDtbDevConfig>,
}

#[derive(Clone)]
pub struct VmConfigEntry {
    // VM id, generate inside hypervisor.
    pub id: usize,
    // Following configs are not intended to be modified during configuration.
    pub name: String,
    pub os_type: VmType,
    pub cmdline: String,
    pub image: VmImageConfig,
    // Following config can be modified during configuration.
    pub memory: VmMemoryConfig,
    pub cpu: VmCpuConfig,
    pub vm_emu_dev_confg: VmEmulatedDeviceConfigList,
    pub vm_pt_dev_confg: VmPassthroughDeviceConfig,
    pub vm_dtb_devs: VMDtbDevConfigList,
    pub mediated_block_index: Option<usize>,
}

impl VmConfigEntry {
    pub fn new(
        name: String,
        cmdline: String,
        vm_type: usize,
        kernel_load_ipa: usize,
        device_tree_load_ipa: usize,
        ramdisk_load_ipa: usize,
    ) -> VmConfigEntry {
        VmConfigEntry {
            id: 0,
            name,
            os_type: VmType::from(vm_type),
            cmdline,
            image: VmImageConfig::new(kernel_load_ipa, device_tree_load_ipa, ramdisk_load_ipa),
            memory: VmMemoryConfig::default(),
            cpu: VmCpuConfig::default(),
            vm_emu_dev_confg: VmEmulatedDeviceConfigList::default(),
            vm_pt_dev_confg: VmPassthroughDeviceConfig::default(),
            vm_dtb_devs: VMDtbDevConfigList::default(),
            mediated_block_index: None,
        }
    }

    pub fn mediated_block_index(&self) -> Option<usize> {
        self.mediated_block_index
    }

    fn set_mediated_block_index(&mut self, med_blk_id: usize) {
        self.mediated_block_index = Some(med_blk_id);
    }

    pub fn kernel_img_name(&self) -> Option<&'static str> {
        self.image.kernel_img_name
    }

    pub fn kernel_load_ipa(&self) -> usize {
        self.image.kernel_load_ipa
    }

    pub fn kernel_entry_point(&self) -> usize {
        self.image.kernel_entry_point
    }

    pub fn device_tree_load_ipa(&self) -> usize {
        self.image.device_tree_load_ipa
    }

    pub fn ramdisk_load_ipa(&self) -> usize {
        self.image.ramdisk_load_ipa
    }

    pub fn memory_region(&self) -> &[VmRegion] {
        &self.memory.region
    }

    pub fn memory_color_bitmap(&self) -> usize {
        if self.memory.colors.is_empty() {
            usize::MAX
        } else {
            let mut color_bitmap = 0;
            for color in &self.memory.colors {
                color_bitmap |= 1 << *color;
            }
            color_bitmap
        }
    }

    fn add_memory_cfg(&mut self, ipa_start: usize, length: usize) {
        self.memory.region.push(VmRegion { ipa_start, length });
    }

    pub fn cpu_num(&self) -> usize {
        self.cpu.num
    }

    pub fn cpu_allocated_bitmap(&self) -> u32 {
        self.cpu.allocate_bitmap
    }

    pub fn cpu_master(&self) -> Option<usize> {
        self.cpu.master
    }

    fn set_cpu_cfg(&mut self, num: usize, allocate_bitmap: usize, master: usize) {
        self.cpu.num = usize::min(num, allocate_bitmap.count_ones() as usize);
        self.cpu.allocate_bitmap = allocate_bitmap as u32;
        self.cpu.master = if allocate_bitmap & (1 << master) != 0 {
            Some(master)
        } else {
            None
        };
    }

    pub fn emulated_device_list(&self) -> &[VmEmulatedDeviceConfig] {
        &self.vm_emu_dev_confg.emu_dev_list
    }

    fn add_emulated_device_cfg(&mut self, cfg: VmEmulatedDeviceConfig) {
        self.vm_emu_dev_confg.emu_dev_list.push(cfg);
    }

    pub fn passthrough_device_regions(&self) -> &[PassthroughRegion] {
        &self.vm_pt_dev_confg.regions
    }

    pub fn passthrough_device_irqs(&self) -> &[usize] {
        &self.vm_pt_dev_confg.irqs
    }

    pub fn passthrough_device_stread_ids(&self) -> &[usize] {
        &self.vm_pt_dev_confg.streams_ids
    }

    fn add_passthrough_device_region(&mut self, base_ipa: usize, base_pa: usize, length: usize) {
        let pt_region_cfg = PassthroughRegion {
            ipa: base_ipa,
            pa: base_pa,
            length,
            dev_property: true,
        };
        self.vm_pt_dev_confg.regions.push(pt_region_cfg)
    }

    fn add_passthrough_device_irqs(&mut self, irqs: &mut Vec<usize>) {
        self.vm_pt_dev_confg.irqs.append(irqs);
    }

    fn add_passthrough_device_streams_ids(&mut self, streams_ids: &mut Vec<usize>) {
        self.vm_pt_dev_confg.streams_ids.append(streams_ids);
    }

    pub fn dtb_device_list(&self) -> &[VmDtbDevConfig] {
        &self.vm_dtb_devs.dtb_device_list
    }

    fn add_dtb_device(&mut self, cfg: VmDtbDevConfig) {
        self.vm_dtb_devs.dtb_device_list.push(cfg);
    }

    pub fn gicc_addr(&self) -> usize {
        for dev in &self.vm_dtb_devs.dtb_device_list {
            if dev.dev_type == DtbDevType::Gicc {
                return dev.addr_region.ipa_start;
            }
        }
        0
    }

    pub fn gicd_addr(&self) -> usize {
        for dev in &self.vm_dtb_devs.dtb_device_list {
            if dev.dev_type == DtbDevType::Gicd {
                return dev.addr_region.ipa_start;
            }
        }
        0
    }
}

struct VmConfigTable {
    vm_bitmap: BitAlloc16,
    entries: Vec<VmConfigEntry>,
}

impl VmConfigTable {
    const fn new() -> VmConfigTable {
        VmConfigTable {
            vm_bitmap: BitAlloc16::default(),
            entries: Vec::new(),
        }
    }

    fn generate_vm_id(&mut self) -> Result<usize, ()> {
        for i in 0..CONFIG_VM_NUM_MAX {
            if self.vm_bitmap.get(i) == 0 {
                self.vm_bitmap.set(i);
                return Ok(i);
            }
        }
        Err(())
    }

    fn remove_vm_id(&mut self, vm_id: usize) {
        if vm_id >= CONFIG_VM_NUM_MAX || self.vm_bitmap.get(vm_id) == 0 {
            println!("illegal vm id {}", vm_id);
        }
        self.vm_bitmap.clear(vm_id);
    }
}

static DEF_VM_CONFIG_TABLE: Mutex<VmConfigTable> = Mutex::new(VmConfigTable::new());

pub fn vm_cfg_entry(vmid: usize) -> Option<VmConfigEntry> {
    let vm_config = DEF_VM_CONFIG_TABLE.lock();
    for vm_cfg_entry in vm_config.entries.iter() {
        if vm_cfg_entry.id == vmid {
            return Some(vm_cfg_entry.clone());
        }
    }
    println!("failed to find VM[{}] in vm cfg entry list", vmid);
    None
}

fn vm_cfg_editor<F>(vmid: usize, f: F) -> Result<usize, ()>
where
    F: FnOnce(&mut VmConfigEntry) -> Result<usize, ()>,
{
    let mut vm_config = DEF_VM_CONFIG_TABLE.lock();
    for vm_cfg_entry in vm_config.entries.iter_mut() {
        if vm_cfg_entry.id == vmid {
            return f(vm_cfg_entry);
        }
    }
    error!("failed to find VM[{}] in vm cfg entry list", vmid);
    Err(())
}

/* Add VM config entry to DEF_VM_CONFIG_TABLE */
pub fn vm_cfg_add_vm_entry(mut vm_cfg_entry: VmConfigEntry) -> Result<usize, ()> {
    let mut vm_config = DEF_VM_CONFIG_TABLE.lock();
    match vm_config.generate_vm_id() {
        Ok(vm_id) => {
            if vm_id == 0 && !vm_config.entries.is_empty() {
                panic!("error in mvm config init, the def vm config table is not empty");
            }
            vm_cfg_entry.id = vm_id;
            println!(
                "\nSuccessfully add VM[{}]: {}, currently vm_num {}",
                vm_cfg_entry.id,
                vm_cfg_entry.name,
                vm_config.entries.len() + 1
            );
            vm_config.entries.push(vm_cfg_entry);

            Ok(vm_id)
        }
        Err(_) => {
            println!("vm_cfg_add_vm_entry, vm num reached max value");
            Err(())
        }
    }
}

/* Generate a new VM Config Entry, set basic value */
pub fn vm_cfg_add_vm(config_ipa: usize) -> Result<usize, ()> {
    let vm = active_vm().unwrap();
    let config_pa = vm.ipa2hva(config_ipa);
    let (
        vm_name_ipa,
        _vm_name_length,
        vm_type,
        cmdline_ipa,
        _cmdline_length,
        kernel_load_ipa,
        device_tree_load_ipa,
        ramdisk_load_ipa,
    ): (usize, usize, usize, usize, usize, usize, usize, usize) = unsafe { *(config_pa as *const _) };
    println!("\n\nStart to prepare configuration for new VM");

    // Copy VM name from user ipa.
    let vm_name_pa = vm.ipa2hva(vm_name_ipa);
    if vm_name_pa == 0 {
        println!("illegal vm_name_ipa {:x}", vm_name_ipa);
        return Err(());
    }
    let vm_name_str = unsafe { CStr::from_ptr(vm_name_pa as *const _) }
        .to_string_lossy()
        .to_string();

    // Copy VM cmdline from user ipa.
    let cmdline_pa = vm.ipa2hva(cmdline_ipa);
    if cmdline_pa == 0 {
        println!("illegal cmdline_ipa {:x}", cmdline_ipa);
        return Err(());
    }
    let cmdline_str = unsafe { CStr::from_ptr(cmdline_pa as *const _) }
        .to_string_lossy()
        .to_string();

    // Generate a new VM config entry.
    let new_vm_cfg = VmConfigEntry::new(
        vm_name_str,
        cmdline_str,
        vm_type,
        kernel_load_ipa,
        device_tree_load_ipa,
        ramdisk_load_ipa,
    );

    println!("      VM name is [{:?}]", new_vm_cfg.name);
    println!("      cmdline is [{:?}]", new_vm_cfg.cmdline);
    println!("      ramdisk is [{:#x}]", new_vm_cfg.ramdisk_load_ipa());
    vm_cfg_add_vm_entry(new_vm_cfg)
}

/* Delete a VM config entry */
pub fn vm_cfg_del_vm(vmid: usize) -> Result<usize, ()> {
    let mut vm_config = DEF_VM_CONFIG_TABLE.lock();
    for (idx, vm_cfg_entry) in vm_config.entries.iter().enumerate() {
        if vm_cfg_entry.id == vmid {
            if let Some(block_idx) = vm_cfg_entry.mediated_block_index() {
                mediated_blk_free(block_idx);
            }
            vm_config.remove_vm_id(vmid);
            vm_config.entries.remove(idx);
            println!("delete VM[{}] config entry from vm-config-table", vmid);
            break;
        }
    }
    Ok(0)
}

/* Add VM memory region according to VM id */
pub fn vm_cfg_add_mem_region(vmid: usize, ipa_start: usize, length: usize) -> Result<usize, ()> {
    vm_cfg_editor(vmid, |vm_cfg| {
        vm_cfg.add_memory_cfg(ipa_start, length);
        println!(
            "\nVM[{}] vm_cfg_add_mem_region: add region start_ipa {:x} length {:x}",
            vmid, ipa_start, length
        );
        Ok(0)
    })
}

/* Set VM cpu config according to VM id */
pub fn vm_cfg_set_cpu(vmid: usize, num: usize, allocate_bitmap: usize, master: usize) -> Result<usize, ()> {
    vm_cfg_editor(vmid, |vm_cfg| {
        vm_cfg.set_cpu_cfg(num, allocate_bitmap, master);

        println!(
            "\nVM[{}] vm_cfg_set_cpu: num {} allocate_bitmap {} master {:?}",
            vmid,
            vm_cfg.cpu_num(),
            vm_cfg.cpu_allocated_bitmap(),
            vm_cfg.cpu_master()
        );

        Ok(0)
    })
}

/* Add emulated device config for VM */
pub fn vm_cfg_add_emu_dev(
    vmid: usize,
    name_ipa: usize,
    base_ipa: usize,
    length: usize,
    irq_id: usize,
    cfg_list_ipa: usize,
    emu_type: usize,
) -> Result<usize, ()> {
    vm_cfg_editor(vmid, |vm_cfg| {
        // Copy emu device name from user ipa.
        let name_pa = active_vm().unwrap().ipa2hva(name_ipa);
        if name_pa == 0 {
            println!("illegal emulated device name_ipa {:x}", name_ipa);
            return Err(());
        }
        let name_str = unsafe { CStr::from_ptr(name_pa as *const _) }
            .to_string_lossy()
            .to_string();
        // Copy emu device cfg list from user ipa.
        let mut cfg_list = vec![0_usize; CFG_MAX_NUM];
        copy_segment_from_vm(&active_vm().unwrap(), cfg_list.as_mut_slice(), cfg_list_ipa);

        println!(
            concat!(
                "\nVM[{}] vm_cfg_add_emu_dev:\n",
                "    name {:?}\n",
                "     cfg_list {:?}\n",
                "     base ipa {:x} length {:x} irq_id {} emu_type {}"
            ),
            vmid, name_str, cfg_list, base_ipa, length, irq_id, emu_type
        );

        let emu_dev_type = EmuDeviceType::from(emu_type);
        let emu_dev_cfg = VmEmulatedDeviceConfig {
            name: name_str,
            base_ipa,
            length,
            irq_id,
            cfg_list,
            emu_type: match emu_dev_type {
                EmuDeviceType::EmuDeviceTVirtioBlkMediated => EmuDeviceType::EmuDeviceTVirtioBlk,
                _ => emu_dev_type,
            },
            mediated: matches!(
                EmuDeviceType::from(emu_type),
                EmuDeviceType::EmuDeviceTVirtioBlkMediated
            ),
        };
        vm_cfg.add_emulated_device_cfg(emu_dev_cfg);

        // Set GVM Mediated Blk Index Here.
        if emu_dev_type == EmuDeviceType::EmuDeviceTVirtioBlkMediated {
            let med_blk_index = match mediated_blk_request() {
                Ok(idx) => idx,
                Err(_) => {
                    println!("no more medaited blk for vm {}", vmid);
                    return Err(());
                }
            };
            vm_cfg.set_mediated_block_index(med_blk_index);
        }

        Ok(0)
    })
}

/* Add passthrough device config region for VM */
pub fn vm_cfg_add_passthrough_device_region(
    vmid: usize,
    base_ipa: usize,
    base_pa: usize,
    length: usize,
) -> Result<usize, ()> {
    // Get VM config entry.
    vm_cfg_editor(vmid, |vm_cfg| {
        println!(
            concat!(
                "\nVM[{}] vm_cfg_add_pt_dev:\n",
                "     base_ipa {:x} base_pa {:x} length {:x}"
            ),
            vmid, base_ipa, base_pa, length
        );

        vm_cfg.add_passthrough_device_region(base_ipa, base_pa, length);
        Ok(0)
    })
}

/* Add passthrough device config irqs for VM */
pub fn vm_cfg_add_passthrough_device_irqs(vmid: usize, irqs_base_ipa: usize, irqs_length: usize) -> Result<usize, ()> {
    println!(
        "\nVM[{}] vm_cfg_add_pt_dev irqs:\n     base_ipa {:x} length {:x}",
        vmid, irqs_base_ipa, irqs_length
    );

    let mut irqs = vec![0_usize; irqs_length];
    if irqs_length > 0 {
        copy_segment_from_vm(&active_vm().unwrap(), irqs.as_mut_slice(), irqs_base_ipa);
    }
    println!("      irqs {:?}", irqs);

    vm_cfg_editor(vmid, |vm_cfg| {
        vm_cfg.add_passthrough_device_irqs(&mut irqs);
        Ok(0)
    })
}

/* Add passthrough device config streams ids for VM */
pub fn vm_cfg_add_passthrough_device_streams_ids(
    vmid: usize,
    streams_ids_base_ipa: usize,
    streams_ids_length: usize,
) -> Result<usize, ()> {
    println!(
        "\nVM[{}] vm_cfg_add_pt_dev streams ids:\n     streams_ids_base_ipa {:x} streams_ids_length {:x}",
        vmid, streams_ids_base_ipa, streams_ids_length
    );

    // Copy passthrough device streams ids from user ipa.
    let mut streams_ids = vec![0_usize; streams_ids_length];
    if streams_ids_length > 0 {
        copy_segment_from_vm(&active_vm().unwrap(), streams_ids.as_mut_slice(), streams_ids_base_ipa)
    }
    println!("      get streams_ids {:?}", streams_ids);

    vm_cfg_editor(vmid, |vm_cfg| {
        vm_cfg.add_passthrough_device_streams_ids(&mut streams_ids);
        Ok(0)
    })
}

/* Add device tree device config for VM */
pub fn vm_cfg_add_dtb_dev(
    vmid: usize,
    name_ipa: usize,
    dev_type: usize,
    irq_list_ipa: usize,
    irq_list_length: usize,
    addr_region_ipa: usize,
    addr_region_length: usize,
) -> Result<usize, ()> {
    println!(
        "\nVM[{}] vm_cfg_add_dtb_dev:\n     dev_type {} irq_list_length {} addr_region_ipa {:x} addr_region_length {:x}",
        vmid, dev_type, irq_list_length, addr_region_ipa, addr_region_length
    );

    // Copy DTB device name from user ipa.
    let name_pa = active_vm().unwrap().ipa2hva(name_ipa);
    if name_pa == 0 {
        println!("illegal dtb_dev name ipa {:x}", name_ipa);
        return Err(());
    }
    let dtb_dev_name_str = unsafe { CStr::from_ptr(name_pa as *const _) }
        .to_string_lossy()
        .to_string();
    println!("      get dtb dev name {:?}", dtb_dev_name_str);

    // Copy DTB device irq list from user ipa.
    let mut dtb_irq_list: Vec<usize> = Vec::new();

    if irq_list_length > 0 {
        let mut tmp_dtb_irq_list = vec![0_usize; irq_list_length];
        copy_segment_from_vm(&active_vm().unwrap(), tmp_dtb_irq_list.as_mut_slice(), irq_list_ipa);
        for irq in tmp_dtb_irq_list.into_iter().take(irq_list_length) {
            dtb_irq_list.push(irq);
        }
    }
    println!("      get dtb dev dtb_irq_list {:?}", dtb_irq_list);

    let vm_dtb_dev = VmDtbDevConfig {
        name: dtb_dev_name_str,
        dev_type: DtbDevType::from(dev_type),
        irqs: dtb_irq_list,
        addr_region: VmRegion {
            ipa_start: addr_region_ipa,
            length: addr_region_length,
        },
    };
    vm_cfg_editor(vmid, |vm_cfg| {
        // Get DTB device config list.

        vm_cfg.add_dtb_device(vm_dtb_dev);

        Ok(0)
    })
}

/**
 * Final Step for GVM configuration.
 * Set up GVM configuration;
 * Set VM kernel image load region;
 */
fn vm_cfg_finish_configuration(vmid: usize, _img_size: usize) -> Vm {
    // Set up GVM configuration.
    vmm_init_gvm(vmid);

    // Get VM structure.

    match vm(vmid) {
        None => {
            panic!("vm_cfg_upload_kernel_image:failed to init VM[{}]", vmid);
        }
        Some(vm) => vm,
    }
}

/**
 * Load kernel image file from MVM user space.
 * It's the last step in GVM configuration.
 */
pub fn vm_cfg_upload_kernel_image(
    vmid: usize,
    img_size: usize,
    cache_ipa: usize,
    load_offset: usize,
    load_size: usize,
) -> Result<usize, ()> {
    // Before upload kernel image, set GVM.
    let vm = match vm(vmid) {
        None => {
            println!(
                "\nSuccessfully add configuration file for VM [{}]\nStart to init...",
                vmid
            );
            // This code should only run once.
            vm_cfg_finish_configuration(vmid, img_size)
        }
        Some(vm) => vm,
    };
    let config = vm.config();

    println!(
        "VM[{}] Upload kernel image. cache_ipa:{:x} load_offset:{:x} load_size:{:x}",
        vmid, cache_ipa, load_offset, load_size
    );
    if copy_between_vm(
        (&vm, config.kernel_load_ipa() + load_offset),
        (&active_vm().unwrap(), cache_ipa),
        load_size,
    ) {
        Ok(0)
    } else {
        Err(())
    }
}
