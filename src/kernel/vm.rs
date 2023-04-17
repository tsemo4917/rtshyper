use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::once::Once;

use spin::Mutex;

use crate::arch::{PAGE_SIZE, PTE_S2_FIELD_AP_RO, timer_arch_get_counter, HYP_VA_SIZE, VM_IPA_SIZE};
use crate::arch::{GICC_CTLR_EN_BIT, GICC_CTLR_EOIMODENS_BIT};
use crate::arch::PageTable;
use crate::arch::Vgic;
use crate::board::{PlatOperation, Platform};
use crate::config::VmConfigEntry;
use crate::device::EmuDevs;
use crate::kernel::mem_color_region_free;
use crate::util::*;
use crate::mm::PageFrame;

use super::ColorMemRegion;
use super::vcpu::Vcpu;

macro_rules! min {
    ($a: expr, $b: expr) => {
        if $a < $b {
            $a
        } else {
            $b
        }
    };
}
// make sure that the CONFIG_VM_NUM_MAX is not greater than (1 << (HYP_VA_SIZE - VM_IPA_SIZE)) - 1
pub const CONFIG_VM_NUM_MAX: usize = min!(shyper::VM_NUM_MAX, (1 << (HYP_VA_SIZE - VM_IPA_SIZE)) - 1);
pub static VM_IF_LIST: [Mutex<VmInterface>; CONFIG_VM_NUM_MAX] =
    [const { Mutex::new(VmInterface::default()) }; CONFIG_VM_NUM_MAX];

pub fn vm_if_reset(vm_id: usize) {
    let mut vm_if = VM_IF_LIST[vm_id].lock();
    vm_if.reset();
}

pub fn vm_if_set_state(vm_id: usize, vm_state: VmState) {
    let mut vm_if = VM_IF_LIST[vm_id].lock();
    vm_if.state = vm_state;
}

pub fn vm_if_get_state(vm_id: usize) -> VmState {
    let vm_if = VM_IF_LIST[vm_id].lock();
    vm_if.state
}

pub fn vm_if_set_type(vm_id: usize, vm_type: VmType) {
    let mut vm_if = VM_IF_LIST[vm_id].lock();
    vm_if.vm_type = vm_type;
}

// pub fn vm_if_get_type(vm_id: usize) -> VmType {
//     let vm_if = VM_IF_LIST[vm_id].lock();
//     vm_if.vm_type
// }

fn vm_if_set_cpu_id(vm_id: usize, master_cpu_id: usize) {
    let vm_if = VM_IF_LIST[vm_id].lock();
    vm_if.master_cpu_id.call_once(|| master_cpu_id);
    debug!(
        "vm_if_list_set_cpu_id vm [{}] set master_cpu_id {}",
        vm_id, master_cpu_id
    );
}

pub fn vm_if_get_cpu_id(vm_id: usize) -> Option<usize> {
    let vm_if = VM_IF_LIST[vm_id].lock();
    vm_if.master_cpu_id.get().cloned()
}

pub fn vm_if_cmp_mac(vm_id: usize, frame: &[u8]) -> bool {
    let vm_if = VM_IF_LIST[vm_id].lock();
    vm_if.mac == frame[0..6]
}

pub fn vm_if_set_ivc_arg(vm_id: usize, ivc_arg: usize) {
    let mut vm_if = VM_IF_LIST[vm_id].lock();
    vm_if.ivc_arg = ivc_arg;
}

pub fn vm_if_ivc_arg(vm_id: usize) -> usize {
    let vm_if = VM_IF_LIST[vm_id].lock();
    vm_if.ivc_arg
}

pub fn vm_if_set_ivc_arg_ptr(vm_id: usize, ivc_arg_ptr: usize) {
    let mut vm_if = VM_IF_LIST[vm_id].lock();
    vm_if.ivc_arg_ptr = ivc_arg_ptr;
}

pub fn vm_if_ivc_arg_ptr(vm_id: usize) -> usize {
    let vm_if = VM_IF_LIST[vm_id].lock();
    vm_if.ivc_arg_ptr
}

// new if for vm migration
pub fn vm_if_init_mem_map(vm_id: usize, len: usize) {
    let mut vm_if = VM_IF_LIST[vm_id].lock();
    vm_if.mem_map = Some(FlexBitmap::new(len));
}

pub fn vm_if_set_mem_map_bit(vm: &Vm, ipa: usize) {
    let mut vm_if = VM_IF_LIST[vm.id()].lock();
    let mut bit = 0;
    for region in vm.config().memory_region().iter() {
        let range = region.as_range();
        if range.contains(&ipa) {
            bit += (ipa - range.start) / PAGE_SIZE;
            // if vm_if.mem_map.as_mut().unwrap().get(bit) == 0 {
            //     println!("vm_if_set_mem_map_bit: set pa {:#x}", pa);
            // }
            vm_if.mem_map.as_mut().unwrap().set(bit, true);
            return;
        } else {
            bit += range.len() / PAGE_SIZE;
        }
    }
    error!("vm_if_set_mem_map_bit: illegal ipa {:#x}", ipa);
}
// End vm interface func implementation

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub enum VmState {
    Inv = 0,
    Pending = 1,
    Active = 2,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VmType {
    VmTOs = 0,
    VmTBma = 1,
}

impl From<usize> for VmType {
    fn from(value: usize) -> Self {
        match value {
            0 => Self::VmTOs,
            1 => Self::VmTBma,
            _ => panic!("Unknown VmType value: {}", value),
        }
    }
}

pub struct VmInterface {
    pub master_cpu_id: Once<usize>,
    pub state: VmState,
    pub vm_type: VmType,
    pub mac: [u8; 6],
    pub ivc_arg: usize,
    pub ivc_arg_ptr: usize,
    pub mem_map: Option<FlexBitmap>,
    pub mem_map_cache: Option<Arc<PageFrame>>,
}

impl VmInterface {
    const fn default() -> VmInterface {
        VmInterface {
            master_cpu_id: Once::new(),
            state: VmState::Pending,
            vm_type: VmType::VmTBma,
            mac: [0; 6],
            ivc_arg: 0,
            ivc_arg_ptr: 0,
            mem_map: None,
            mem_map_cache: None,
        }
    }

    fn reset(&mut self) {
        self.master_cpu_id = Once::new();
        self.state = VmState::Pending;
        self.vm_type = VmType::VmTBma;
        self.mac = [0; 6];
        self.ivc_arg = 0;
        self.ivc_arg_ptr = 0;
        self.mem_map = None;
        self.mem_map_cache = None;
    }
}

/* HCR_EL2 init value
 *  - VM
 *  - RW
 *  - IMO
 *  - FMO
 *  - TSC
 */
// const HCR_EL2_INIT_VAL: u64 = 0x80080019;

#[derive(Clone)]
pub struct Vm {
    inner: Arc<Mutex<VmInner>>,
}

#[derive(Clone)]
pub struct WeakVm {
    inner: Weak<Mutex<VmInner>>,
}

impl WeakVm {
    pub fn get_vm(&self) -> Option<Vm> {
        Weak::upgrade(&self.inner).map(|inner| Vm { inner })
    }
}

#[allow(dead_code)]
impl Vm {
    pub fn get_weak(&self) -> WeakVm {
        WeakVm {
            inner: Arc::downgrade(&self.inner),
        }
    }

    pub fn new(id: usize) -> Vm {
        Vm {
            inner: Arc::new(Mutex::new(VmInner::new(id))),
        }
    }

    pub fn init_intc_mode(&self, emu: bool) {
        let vm_inner = self.inner.lock();
        for vcpu in &vm_inner.vcpu_list {
            info!(
                "vm {} vcpu {} set {} hcr",
                vm_inner.id,
                vcpu.id(),
                if emu { "emu" } else { "partial passthrough" }
            );
            if !emu {
                vcpu.set_gich_ctlr((GICC_CTLR_EN_BIT) as u32);
                vcpu.set_hcr(0x80080001); // HCR_EL2_GIC_PASSTHROUGH_VAL
            } else {
                vcpu.set_gich_ctlr((GICC_CTLR_EN_BIT | GICC_CTLR_EOIMODENS_BIT) as u32);
                vcpu.set_hcr(0x80080019);
            }
            // hcr |= 1 << 17; // set HCR_EL2.TID2=1, trap for cache id sysregs
        }
    }

    pub fn set_iommu_ctx_id(&self, id: usize) {
        let mut vm_inner = self.inner.lock();
        vm_inner.iommu_ctx_id = Some(id);
    }

    pub fn iommu_ctx_id(&self) -> usize {
        let vm_inner = self.inner.lock();
        match vm_inner.iommu_ctx_id {
            None => {
                panic!("vm {} do not have iommu context bank", vm_inner.id);
            }
            Some(id) => id,
        }
    }

    pub fn med_blk_id(&self) -> usize {
        let vm_inner = self.inner.lock();
        match vm_inner.config.as_ref().unwrap().mediated_block_index() {
            None => {
                panic!("vm {} do not have mediated blk", vm_inner.id);
            }
            Some(idx) => idx,
        }
    }

    pub fn vcpu(&self, index: usize) -> Option<Vcpu> {
        let vm_inner = self.inner.lock();
        match vm_inner.vcpu_list.get(index).cloned() {
            Some(vcpu) => {
                assert_eq!(index, vcpu.id());
                Some(vcpu)
            }
            None => {
                println!(
                    "vcpu idx {} is to large than vcpu_list len {}",
                    index,
                    vm_inner.vcpu_list.len()
                );
                None
            }
        }
    }

    pub fn push_vcpu(&self, vcpu: Vcpu) {
        let mut vm_inner = self.inner.lock();
        if vcpu.id() >= vm_inner.vcpu_list.len() {
            vm_inner.vcpu_list.push(vcpu);
        } else {
            println!("VM[{}] insert VCPU {}", vm_inner.id, vcpu.id());
            vm_inner.vcpu_list.insert(vcpu.id(), vcpu);
        }
    }

    pub fn select_vcpu2assign(&self, cpu_id: usize) -> Option<Vcpu> {
        let cfg_master = self.config().cpu_master();
        let cfg_cpu_num = self.config().cpu_num();
        let cfg_cpu_allocate_bitmap = self.config().cpu_allocated_bitmap();
        // make sure that vcpu assign is executed sequentially, otherwise
        // the PCPUs may found that vm.cpu_num() == 0 at the same time and
        // if cfg_master is not setted, they will not set master vcpu for VM
        let mut vm_inner = self.inner.lock();
        if (cfg_cpu_allocate_bitmap & (1 << cpu_id)) != 0 && vm_inner.cpu_num < cfg_cpu_num {
            // vm.vcpu(0) must be the VM's master vcpu
            let trgt_id = if cpu_id == cfg_master
                || (vm_if_get_cpu_id(vm_inner.id).is_none() && vm_inner.cpu_num == cfg_cpu_num - 1)
            {
                0
            } else if vm_if_get_cpu_id(vm_inner.id).is_some() {
                // VM has master
                cfg_cpu_num - vm_inner.cpu_num
            } else {
                // if master vcpu is not assigned, retain id 0 for it
                cfg_cpu_num - vm_inner.cpu_num - 1
            };
            match vm_inner.vcpu_list.get(trgt_id).cloned() {
                None => None,
                Some(vcpu) => {
                    if vcpu.id() == 0 {
                        vm_if_set_cpu_id(vm_inner.id, cpu_id);
                    }
                    vm_inner.cpu_num += 1;
                    vm_inner.ncpu |= 1 << cpu_id;
                    Some(vcpu)
                }
            }
        } else {
            None
        }
    }

    pub fn set_emu_devs(&self, idx: usize, emu: EmuDevs) {
        let mut vm_inner = self.inner.lock();
        if idx < vm_inner.emu_devs.len() {
            if let EmuDevs::None = vm_inner.emu_devs[idx] {
                // println!("set_emu_devs: cover a None emu dev");
                vm_inner.emu_devs[idx] = emu;
                return;
            } else {
                panic!("set_emu_devs: set an exsit emu dev");
            }
        }
        while idx > vm_inner.emu_devs.len() {
            println!("set_emu_devs: push a None emu dev");
            vm_inner.emu_devs.push(EmuDevs::None);
        }
        vm_inner.emu_devs.push(emu);
    }

    pub fn set_intc_dev_id(&self, intc_dev_id: usize) {
        let mut vm_inner = self.inner.lock();
        vm_inner.intc_dev_id = intc_dev_id;
    }

    pub fn set_int_bit_map(&self, int_id: usize) {
        let mut vm_inner = self.inner.lock();
        vm_inner.int_bitmap.as_mut().unwrap().set(int_id);
    }

    pub fn set_config_entry(&self, config: Option<VmConfigEntry>) {
        let mut vm_inner = self.inner.lock();
        vm_inner.config = config;
    }

    pub fn intc_dev_id(&self) -> usize {
        let vm_inner = self.inner.lock();
        vm_inner.intc_dev_id
    }

    pub fn pt_map_range(&self, ipa: usize, len: usize, pa: usize, pte: usize, map_block: bool) {
        let vm_inner = self.inner.lock();
        match &vm_inner.pt {
            Some(pt) => pt.pt_map_range(ipa, len, pa, pte, map_block),
            None => {
                panic!("Vm::pt_map_range: vm{} pt is empty", vm_inner.id);
            }
        }
    }

    pub fn pt_unmap_range(&self, ipa: usize, len: usize, map_block: bool) {
        let vm_inner = self.inner.lock();
        match &vm_inner.pt {
            Some(pt) => pt.pt_unmap_range(ipa, len, map_block),
            None => {
                panic!("Vm::pt_umnmap_range: vm{} pt is empty", vm_inner.id);
            }
        }
    }

    // ap: access permission
    pub fn pt_set_access_permission(&self, ipa: usize, ap: usize) -> (usize, usize) {
        let vm_inner = self.inner.lock();
        match &vm_inner.pt {
            Some(pt) => pt.access_permission(ipa, PAGE_SIZE, ap),
            None => {
                panic!("pt_set_access_permission: vm{} pt is empty", vm_inner.id);
            }
        }
    }

    pub fn pt_read_only(&self) {
        let config = self.config();
        let vm_inner = self.inner.lock();
        match &vm_inner.pt {
            Some(pt) => {
                for region in config.memory_region().iter() {
                    pt.access_permission(region.ipa_start, region.length, PTE_S2_FIELD_AP_RO);
                }
            }
            None => {
                panic!("Vm::read_only: vm{} pt is empty", vm_inner.id);
            }
        }
    }

    pub fn set_pt(&self, pt_dir_frame: PageFrame) {
        let mut vm_inner = self.inner.lock();
        vm_inner.pt = Some(PageTable::new(pt_dir_frame, true))
    }

    pub fn pt_dir(&self) -> usize {
        let vm_inner = self.inner.lock();
        match &vm_inner.pt {
            Some(pt) => pt.base_pa(),
            None => {
                panic!("Vm::pt_dir: vm{} pt is empty", vm_inner.id);
            }
        }
    }

    pub fn ipa2pa(&self, ipa: usize) -> Option<usize> {
        let vm_inner = self.inner.lock();
        match &vm_inner.pt {
            Some(pt) => pt.ipa2pa(ipa),
            None => panic!("Vm::ipa2pa: vm{} pt is empty", vm_inner.id),
        }
    }

    pub fn cpu_num(&self) -> usize {
        let vm_inner = self.inner.lock();
        vm_inner.cpu_num
    }

    pub fn id(&self) -> usize {
        let vm_inner = self.inner.lock();
        vm_inner.id
    }

    pub fn config(&self) -> VmConfigEntry {
        let vm_inner = self.inner.lock();
        match &vm_inner.config {
            None => {
                panic!("VM[{}] do not have vm config entry", vm_inner.id);
            }
            Some(config) => config.clone(),
        }
    }

    pub fn reset_mem_regions(&self) {
        let config = self.config();
        for region in config.memory_region().iter() {
            let hva = vm_ipa2hva(self, region.ipa_start);
            memset_safe(hva as *mut _, 0, region.length);
        }
    }

    pub fn append_color_regions(&self, mut regions: Vec<ColorMemRegion>) {
        let mut vm_inner = self.inner.lock();
        vm_inner.color_pa_info.color_pa_region.append(&mut regions);
    }

    pub fn vgic(&self) -> Arc<Vgic> {
        let vm_inner = self.inner.lock();
        match &vm_inner.emu_devs[vm_inner.intc_dev_id] {
            EmuDevs::Vgic(vgic) => vgic.clone(),
            _ => {
                panic!("vm{} cannot find vgic", vm_inner.id);
            }
        }
    }

    pub fn has_vgic(&self) -> bool {
        let vm_inner = self.inner.lock();
        if vm_inner.intc_dev_id >= vm_inner.emu_devs.len() {
            return false;
        }
        matches!(&vm_inner.emu_devs[vm_inner.intc_dev_id], EmuDevs::Vgic(_))
    }

    pub fn emu_dev(&self, dev_id: usize) -> EmuDevs {
        let vm_inner = self.inner.lock();
        vm_inner.emu_devs[dev_id].clone()
    }

    pub fn emu_net_dev(&self, id: usize) -> EmuDevs {
        let vm_inner = self.inner.lock();
        let mut dev_num = 0;

        for dev in vm_inner.emu_devs.iter() {
            if let EmuDevs::VirtioNet(_) = dev {
                if dev_num == id {
                    return dev.clone();
                }
                dev_num += 1;
            }
        }
        EmuDevs::None
    }

    pub fn emu_blk_dev(&self) -> EmuDevs {
        for emu in &self.inner.lock().emu_devs {
            if let EmuDevs::VirtioBlk(_) = emu {
                return emu.clone();
            }
        }
        EmuDevs::None
    }

    // Get console dev by ipa.
    pub fn emu_console_dev(&self, ipa: usize) -> EmuDevs {
        for (idx, emu_dev_cfg) in self.config().emulated_device_list().iter().enumerate() {
            if emu_dev_cfg.base_ipa == ipa {
                return self.inner.lock().emu_devs[idx].clone();
            }
        }
        // println!("emu_console_dev ipa {:x}", ipa);
        // for (idx, emu_dev_cfg) in self.config().emulated_device_list().iter().enumerate() {
        //     println!("emu dev[{}], ipa {:#x}", idx, emu_dev_cfg.base_ipa);
        // }
        EmuDevs::None
    }

    pub fn ncpu(&self) -> usize {
        let vm_inner = self.inner.lock();
        vm_inner.ncpu
    }

    pub fn has_interrupt(&self, int_id: usize) -> bool {
        let mut vm_inner = self.inner.lock();
        vm_inner.int_bitmap.as_mut().unwrap().get(int_id) != 0
    }

    pub fn emu_has_interrupt(&self, int_id: usize) -> bool {
        for emu_dev in self.config().emulated_device_list() {
            if int_id == emu_dev.irq_id {
                return true;
            }
        }
        false
    }

    pub fn vcpuid_to_pcpuid(&self, vcpuid: usize) -> Result<usize, ()> {
        // println!("vcpuid_to_pcpuid");
        let vm_inner = self.inner.lock();
        if vcpuid < vm_inner.cpu_num {
            let vcpu = vm_inner.vcpu_list[vcpuid].clone();
            drop(vm_inner);
            Ok(vcpu.phys_id())
        } else {
            Err(())
        }
    }

    pub fn pcpuid_to_vcpuid(&self, pcpuid: usize) -> Result<usize, ()> {
        let vm_inner = self.inner.lock();
        for vcpuid in 0..vm_inner.cpu_num {
            if vm_inner.vcpu_list[vcpuid].phys_id() == pcpuid {
                return Ok(vcpuid);
            }
        }
        Err(())
    }

    pub fn vcpu_to_pcpu_mask(&self, mask: usize, len: usize) -> usize {
        let mut pmask = 0;
        for i in 0..len {
            let shift = self.vcpuid_to_pcpuid(i);
            if mask & (1 << i) != 0 {
                if let Ok(shift) = shift {
                    pmask |= 1 << shift;
                }
            }
        }
        pmask
    }

    pub fn pcpu_to_vcpu_mask(&self, mask: usize, len: usize) -> usize {
        let mut pmask = 0;
        for i in 0..len {
            let shift = self.pcpuid_to_vcpuid(i);
            if mask & (1 << i) != 0 && shift.is_ok() {
                if let Ok(shift) = shift {
                    pmask |= 1 << shift;
                }
            }
        }
        pmask
    }

    pub fn show_pagetable(&self, ipa: usize) {
        let vm_inner = self.inner.lock();
        vm_inner.pt.as_ref().unwrap().show_pt(ipa);
    }

    pub fn ready(&self) -> bool {
        let vm_inner = self.inner.lock();
        vm_inner.ready
    }

    pub fn set_ready(&self, ready: bool) {
        let mut vm_inner = self.inner.lock();
        vm_inner.ready = ready;
    }

    pub fn share_mem_base(&self) -> usize {
        let inner = self.inner.lock();
        inner.share_mem_base
    }

    pub fn add_share_mem_base(&self, len: usize) {
        let mut inner = self.inner.lock();
        inner.share_mem_base += len;
    }

    // Formula: Virtual Count = Physical Count - <offset>
    //          (from ARM: Learn the architecture - Generic Timer)
    // So, <offset> = Physical Count - Virtual Count
    // in this case, Physical Count is `timer_arch_get_counter()`;
    // virtual count is recorded when the VM is pending (runnning vcpu = 0)
    // Only used in Vcpu::context_vm_store
    pub(super) fn update_vtimer(&self) {
        let mut inner = self.inner.lock();
        // println!(">>> update_vtimer: VM[{}] running {}", inner.id, inner.running);
        inner.running -= 1;
        if inner.running == 0 {
            inner.vtimer = timer_arch_get_counter() - inner.vtimer_offset;
            // info!("VM[{}] set vtimer {:#x}", inner.id, inner.vtimer);
        }
    }

    // Only used in Vcpu::context_vm_restore
    pub(super) fn update_vtimer_offset(&self) -> usize {
        let mut inner = self.inner.lock();
        // println!(">>> update_vtimer_offset: VM[{}] running {}", inner.id, inner.running);
        if inner.running == 0 {
            inner.vtimer_offset = timer_arch_get_counter() - inner.vtimer;
            // info!("VM[{}] set offset {:#x}", inner.id, inner.vtimer_offset);
        }
        inner.running += 1;
        inner.vtimer_offset
    }
}

#[derive(Default)]
struct VmColorPaInfo {
    pub color_pa_region: Vec<ColorMemRegion>,
}

impl Drop for VmColorPaInfo {
    fn drop(&mut self) {
        for region in self.color_pa_region.iter() {
            mem_color_region_free(region);
        }
    }
}

struct VmInner {
    pub id: usize,
    pub ready: bool,
    pub config: Option<VmConfigEntry>,
    // memory config
    pub pt: Option<PageTable>,
    pub color_pa_info: VmColorPaInfo,

    // vcpu config
    pub vcpu_list: Vec<Vcpu>,
    pub cpu_num: usize,
    pub ncpu: usize,

    // interrupt
    pub intc_dev_id: usize,
    pub int_bitmap: Option<BitMap<BitAlloc256>>,

    // migration
    pub share_mem_base: usize,

    // iommu
    pub iommu_ctx_id: Option<usize>,

    // emul devs
    pub emu_devs: Vec<EmuDevs>,

    // VM timer
    running: usize,
    vtimer_offset: usize,
    vtimer: usize,
}

impl VmInner {
    pub fn new(id: usize) -> VmInner {
        VmInner {
            id,
            ready: false,
            config: None,
            pt: None,
            color_pa_info: VmColorPaInfo::default(),

            vcpu_list: Vec::new(),
            cpu_num: 0,
            ncpu: 0,

            intc_dev_id: 0,
            int_bitmap: Some(BitAlloc4K::default()),
            share_mem_base: Platform::SHARE_MEM_BASE, // hard code
            iommu_ctx_id: None,
            emu_devs: Vec::new(),
            running: 0,
            vtimer_offset: timer_arch_get_counter(),
            vtimer: 0,
        }
    }
}

pub static VM_LIST: Mutex<Vec<Vm>> = Mutex::new(Vec::new());

pub fn push_vm(id: usize) -> Result<(), ()> {
    let mut vm_list = VM_LIST.lock();
    if vm_list.iter().any(|x| x.id() == id) {
        println!("push_vm: vm {} already exists", id);
        Err(())
    } else {
        vm_list.push(Vm::new(id));
        Ok(())
    }
}

pub fn remove_vm(id: usize) -> Vm {
    let mut vm_list = VM_LIST.lock();
    match vm_list.iter().position(|x| x.id() == id) {
        None => {
            panic!("VM[{}] not exist in VM LIST", id);
        }
        Some(idx) => vm_list.remove(idx),
    }
}

pub fn vm(id: usize) -> Option<Vm> {
    let vm_list = VM_LIST.lock();
    vm_list.iter().find(|&x| x.id() == id).cloned()
}

// TODO: rename the function
pub fn vm_ipa2hva(vm: &Vm, ipa: usize) -> usize {
    let mask = (1 << (HYP_VA_SIZE - VM_IPA_SIZE)) - 1;
    let prefix = mask << VM_IPA_SIZE;
    if ipa == 0 || ipa & prefix != 0 {
        println!("vm_ipa2hva: VM {} access invalid ipa {:x}", vm.id(), ipa);
        return 0;
    }
    let prefix = prefix - ((vm.id() & mask) << VM_IPA_SIZE);
    prefix | ipa
}
