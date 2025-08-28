use alloc::boxed::Box;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use spin::{Mutex, Once};

use crate::arch::PageTable;
use crate::arch::Vgic;
use crate::arch::{emu_intc_init, HYP_VA_SIZE, VM_IPA_SIZE};
use crate::config::VmConfigEntry;
use crate::device::{emu_virtio_mmio_init, EmuDev};
use crate::kernel::{mem_color_region_free, shyper_init};
use crate::util::*;

use super::vcpu::Vcpu;
use super::{mem_page_alloc, ColorMemRegion};

// make sure that the CONFIG_VM_NUM_MAX is not greater than (1 << (HYP_VA_SIZE - VM_IPA_SIZE)) - 1
pub const CONFIG_VM_NUM_MAX: usize = min!(shyper::VM_NUM_MAX, (1 << (HYP_VA_SIZE - VM_IPA_SIZE)) - 1);
static VM_IF_LIST: [Mutex<VmInterface>; CONFIG_VM_NUM_MAX] =
    [const { Mutex::new(VmInterface::default()) }; CONFIG_VM_NUM_MAX];

pub fn vm_if_reset(vm_id: usize) {
    if let Some(vm_if) = VM_IF_LIST.get(vm_id) {
        vm_if.lock().reset();
    }
}

pub fn vm_if_set_state(vm_id: usize, vm_state: VmState) {
    if let Some(vm_if) = VM_IF_LIST.get(vm_id) {
        vm_if.lock().state = vm_state;
    }
}

pub fn vm_if_get_state(vm_id: usize) -> VmState {
    if let Some(vm_if) = VM_IF_LIST.get(vm_id) {
        vm_if.lock().state
    } else {
        VmState::default()
    }
}

fn vm_if_set_cpu_id(vm_id: usize, master_cpu_id: usize) {
    if let Some(vm_if) = VM_IF_LIST.get(vm_id) {
        vm_if.lock().master_cpu_id.call_once(|| master_cpu_id);
        debug!(
            "vm_if_list_set_cpu_id vm [{}] set master_cpu_id {}",
            vm_id, master_cpu_id
        );
    }
}

pub fn vm_if_get_cpu_id(vm_id: usize) -> Option<usize> {
    if let Some(vm_if) = VM_IF_LIST.get(vm_id) {
        vm_if.lock().master_cpu_id.get().cloned()
    } else {
        None
    }
}

pub fn vm_if_set_ivc_arg(vm_id: usize, ivc_arg: usize) {
    if let Some(vm_if) = VM_IF_LIST.get(vm_id) {
        vm_if.lock().ivc_arg = ivc_arg;
    }
}

pub fn vm_if_ivc_arg(vm_id: usize) -> usize {
    if let Some(vm_if) = VM_IF_LIST.get(vm_id) {
        vm_if.lock().ivc_arg
    } else {
        0
    }
}

pub fn vm_if_set_ivc_arg_ptr(vm_id: usize, ivc_arg_ptr: usize) {
    if let Some(vm_if) = VM_IF_LIST.get(vm_id) {
        vm_if.lock().ivc_arg_ptr = ivc_arg_ptr;
    }
}

pub fn vm_if_ivc_arg_ptr(vm_id: usize) -> usize {
    if let Some(vm_if) = VM_IF_LIST.get(vm_id) {
        vm_if.lock().ivc_arg_ptr
    } else {
        0
    }
}
// End vm interface func implementation

#[allow(dead_code)]
#[derive(Clone, Copy, Default)]
pub enum VmState {
    #[default]
    Inv = 0,
    Pending = 1,
    Active = 2,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum VmType {
    #[default]
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
    master_cpu_id: Once<usize>,
    state: VmState,
    ivc_arg: usize,
    ivc_arg_ptr: usize,
}

impl VmInterface {
    const fn default() -> VmInterface {
        VmInterface {
            master_cpu_id: Once::new(),
            state: VmState::Pending,
            ivc_arg: 0,
            ivc_arg_ptr: 0,
        }
    }

    fn reset(&mut self) {
        self.master_cpu_id = Once::new();
        self.state = VmState::Pending;
        self.ivc_arg = 0;
        self.ivc_arg_ptr = 0;
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub enum IntCtrlType {
    #[default]
    Emulated,
    #[cfg(not(feature = "memory-reservation"))]
    Passthrough,
}

pub struct Vm {
    inner_const: VmInnerConst,
    inner_mut: Mutex<VmInnerMut>,
}

struct VmInnerConst {
    id: usize,
    config: VmConfigEntry,
    vcpu_list: Box<[Vcpu]>,
    intc_type: IntCtrlType,
    // TODO: create struct ArchVcpu and move intc_dev into it
    arch_intc_dev: Option<Arc<Vgic>>,
    int_bitmap: BitAlloc4K,
    emu_devs: Vec<Arc<dyn EmuDev>>,
}

fn cal_phys_id_list(config: &VmConfigEntry) -> Vec<usize> {
    // generate the vcpu physical id list
    let mut phys_id_list = vec![];
    let mut cfg_cpu_allocate_bitmap = config.cpu_allocated_bitmap();
    if let Some(master) = config.cpu_master() {
        if cfg_cpu_allocate_bitmap & (1 << master) != 0 {
            phys_id_list.push(master);
        }
        let mut phys_id = 0;
        while cfg_cpu_allocate_bitmap != 0 {
            if cfg_cpu_allocate_bitmap & 1 != 0 && phys_id != master {
                phys_id_list.push(phys_id);
            }
            phys_id += 1;
            cfg_cpu_allocate_bitmap >>= 1;
        }
    } else {
        let mut phys_id = 0;
        while cfg_cpu_allocate_bitmap != 0 {
            if cfg_cpu_allocate_bitmap & 1 != 0 {
                phys_id_list.push(phys_id);
            }
            phys_id += 1;
            cfg_cpu_allocate_bitmap >>= 1;
        }
    }
    phys_id_list
}

impl VmInnerConst {
    fn new(id: usize, config: VmConfigEntry, vm: Weak<Vm>) -> Self {
        let phys_id_list = cal_phys_id_list(&config);
        debug!("VM[{}] vcpu phys_id_list {:?}", id, phys_id_list);

        // cpu total count must equals to physical id list
        assert_eq!(phys_id_list.len(), config.cpu_num());
        // set the master cpu id to VmInterface
        vm_if_set_cpu_id(id, *phys_id_list.first().unwrap());

        let mut vcpu_list = Vec::with_capacity(config.cpu_num());
        for (vcpu_id, phys_id) in phys_id_list.into_iter().enumerate() {
            vcpu_list.push(Vcpu::new(vm.clone(), vcpu_id, phys_id, &config));
        }
        let mut this = Self {
            id,
            config,
            vcpu_list: vcpu_list.into_boxed_slice(),
            arch_intc_dev: None,
            int_bitmap: BitAlloc4K::default(),
            emu_devs: vec![],
            intc_type: IntCtrlType::Emulated,
        };
        this.init_devices(vm);
        this
    }

    fn init_devices(&mut self, vm: Weak<Vm>) -> bool {
        // emulated devices
        use crate::device::EmuDeviceType::*;
        for (idx, emu_cfg) in self.config.emulated_device_list().iter().enumerate() {
            let dev = match emu_cfg.emu_type {
                EmuDeviceTGicd => {
                    self.intc_type = IntCtrlType::Emulated;
                    emu_intc_init(emu_cfg, &self.vcpu_list).map(|vgic| {
                        self.arch_intc_dev = vgic.clone().into_any_arc().downcast::<Vgic>().ok();
                        vgic
                    })
                }
                #[cfg(not(feature = "memory-reservation"))]
                EmuDeviceTGPPT => {
                    self.intc_type = IntCtrlType::Passthrough;
                    crate::arch::partial_passthrough_intc_init(emu_cfg)
                }
                EmuDeviceTVirtioBlk | EmuDeviceTVirtioConsole | EmuDeviceTVirtioNet | VirtioBalloon => {
                    emu_virtio_mmio_init(vm.clone(), emu_cfg)
                }
                #[cfg(feature = "iommu")]
                EmuDeviceTIOMMU => crate::kernel::emu_iommu_init(emu_cfg), // Do IOMMU init later, after add VM to global list
                EmuDeviceTShyper => {
                    if !shyper_init(self.id, emu_cfg.base_ipa, emu_cfg.length) {
                        return false;
                    }
                    Err(())
                }
                _ => {
                    warn!(
                        "vmm_init_emulated_device: unknown emulated device {:?}",
                        emu_cfg.emu_type
                    );
                    return false;
                }
            };
            if let Ok(emu_dev) = dev {
                if self.emu_devs.iter().any(|dev| {
                    emu_dev.address_range().contains(&dev.address_range().start)
                        || dev.address_range().contains(&emu_dev.address_range().start)
                }) {
                    panic!(
                        "duplicated emul address region: prev address {:x?}",
                        emu_dev.address_range(),
                    );
                } else {
                    self.emu_devs.push(emu_dev);
                }
            }
            if emu_cfg.irq_id != 0 {
                self.int_bitmap.set(emu_cfg.irq_id);
            }
            info!(
                "VM {} registers emulated device: id=<{}>, name=\"{:?}\", ipa=<{:#x}>",
                self.id, idx, emu_cfg.emu_type, emu_cfg.base_ipa
            );
        }
        // pass through irqs
        for irq in self.config.passthrough_device_irqs() {
            self.int_bitmap.set(*irq);
        }
        true
    }
}

impl Vm {
    pub fn new(id: usize, config: VmConfigEntry) -> Arc<Self> {
        let this = Arc::new_cyclic(|weak| Vm {
            inner_const: VmInnerConst::new(id, config, weak.clone()),
            inner_mut: Mutex::new(VmInnerMut::new()),
        });
        for vcpu in this.vcpu_list() {
            vcpu.init(this.config());
        }
        this.init_intc_mode(this.inner_const.intc_type);
        this
    }

    #[cfg(feature = "iommu")]
    pub fn set_iommu_ctx_id(&self, id: usize) {
        let mut vm_inner = self.inner_mut.lock();
        vm_inner.iommu_ctx_id = Some(id);
    }

    #[cfg(feature = "iommu")]
    pub fn iommu_ctx_id(&self) -> usize {
        let vm_inner = self.inner_mut.lock();
        match vm_inner.iommu_ctx_id {
            None => {
                panic!("vm {} do not have iommu context bank", self.id());
            }
            Some(id) => id,
        }
    }

    pub fn med_blk_id(&self) -> usize {
        match self.config().mediated_block_index() {
            None => {
                panic!("vm {} do not have mediated blk", self.id());
            }
            Some(idx) => idx,
        }
    }

    #[inline]
    pub fn vcpu(&self, index: usize) -> Option<&Vcpu> {
        self.vcpu_list().get(index)
    }

    #[inline]
    pub fn vcpu_list(&self) -> &[Vcpu] {
        &self.inner_const.vcpu_list
    }

    pub fn find_emu_dev(&self, ipa: usize) -> Option<Arc<dyn EmuDev>> {
        self.inner_const
            .emu_devs
            .iter()
            .find(|&dev| dev.address_range().contains(&ipa))
            .cloned()
    }

    pub fn pt_map_range(&self, ipa: usize, len: usize, pa: usize, pte: usize, map_block: bool) {
        let vm_inner = self.inner_mut.lock();
        vm_inner.pt.pt_map_range(ipa, len, pa, pte, map_block);
    }

    #[allow(dead_code)]
    pub fn pt_unmap_range(&self, ipa: usize, len: usize, map_block: bool) {
        let vm_inner = self.inner_mut.lock();
        vm_inner.pt.pt_unmap_range(ipa, len, map_block);
    }

    pub fn pt_dir(&self) -> usize {
        let vm_inner = self.inner_mut.lock();
        vm_inner.pt.base_pa()
    }

    pub fn ipa2pa(&self, ipa: usize) -> Option<usize> {
        let vm_inner = self.inner_mut.lock();
        vm_inner.pt.ipa2pa(ipa)
    }

    pub fn cpu_num(&self) -> usize {
        self.inner_const.config.cpu_num()
    }

    #[inline]
    pub fn id(&self) -> usize {
        self.inner_const.id
    }

    #[inline]
    pub fn config(&self) -> &VmConfigEntry {
        &self.inner_const.config
    }

    #[inline]
    pub fn vm_type(&self) -> VmType {
        self.config().os_type
    }

    pub fn reset_mem_regions(&self) {
        let config = self.config();
        for region in config.memory_region().iter() {
            let hva = self.ipa2hva(region.ipa_start);
            unsafe { core::slice::from_raw_parts_mut(hva as *mut u8, region.length) }.fill(0);
        }
    }

    pub fn append_color_regions(&self, mut regions: Vec<ColorMemRegion>) {
        let mut vm_inner = self.inner_mut.lock();
        vm_inner.color_pa_info.region_list.append(&mut regions);
    }

    pub fn vgic(&self) -> &Vgic {
        if let Some(vgic) = self.inner_const.arch_intc_dev.as_ref() {
            return vgic;
        }
        panic!("vm{} cannot find vgic", self.id());
    }

    pub fn has_vgic(&self) -> bool {
        self.inner_const.arch_intc_dev.is_some()
    }

    pub fn ncpu(&self) -> usize {
        self.inner_const.config.cpu_allocated_bitmap()
    }

    pub fn has_interrupt(&self, int_id: usize) -> bool {
        self.inner_const.int_bitmap.get(int_id) != 0
    }

    pub fn vcpuid_to_pcpuid(&self, vcpuid: usize) -> Option<usize> {
        self.vcpu_list().get(vcpuid).map(|vcpu| vcpu.phys_id())
    }

    pub fn pcpuid_to_vcpuid(&self, pcpuid: usize) -> Option<usize> {
        for vcpu in self.vcpu_list() {
            if vcpu.phys_id() == pcpuid {
                return Some(vcpu.id());
            }
        }
        None
    }

    pub fn vcpu_to_pcpu_mask(&self, mask: usize, len: usize) -> usize {
        let mut pmask = 0;
        for i in 0..len {
            if let Some(shift) = self.vcpuid_to_pcpuid(i) {
                if mask & (1 << i) != 0 {
                    pmask |= 1 << shift;
                }
            }
        }
        pmask
    }

    pub fn pcpu_to_vcpu_mask(&self, mask: usize, len: usize) -> usize {
        let mut pmask = 0;
        for i in 0..len {
            if let Some(shift) = self.pcpuid_to_vcpuid(i) {
                if mask & (1 << i) != 0 {
                    pmask |= 1 << shift;
                }
            }
        }
        pmask
    }

    pub fn show_pagetable(&self, ipa: usize) {
        let vm_inner = self.inner_mut.lock();
        vm_inner.pt.show_pt(ipa);
    }

    // Formula: Virtual Count = Physical Count - <offset>
    //          (from ARM: Learn the architecture - Generic Timer)
    // So, <offset> = Physical Count - Virtual Count
    // in this case, Physical Count is `timer::get_counter()`;
    // virtual count is recorded when the VM is pending (runnning vcpu = 0)
    // Only used in Vcpu::context_vm_store
    #[cfg(feature = "vtimer")]
    pub(super) fn update_vtimer(&self) {
        let mut inner = self.inner_mut.lock();
        trace!(">>> update_vtimer: VM[{}] running {}", self.id(), inner.running);
        inner.running -= 1;
        if inner.running == 0 {
            inner.vtimer = super::timer::get_counter() - inner.vtimer_offset;
            trace!("VM[{}] set vtimer {:#x}", self.id(), inner.vtimer);
        }
    }

    // Only used in Vcpu::context_vm_restore
    #[cfg(feature = "vtimer")]
    pub(super) fn update_vtimer_offset(&self) -> usize {
        let mut inner = self.inner_mut.lock();
        trace!(">>> update_vtimer_offset: VM[{}] running {}", self.id(), inner.running);
        if inner.running == 0 {
            inner.vtimer_offset = super::timer::get_counter() - inner.vtimer;
            trace!("VM[{}] set offset {:#x}", self.id(), inner.vtimer_offset);
        }
        inner.running += 1;
        inner.vtimer_offset
    }

    pub fn ipa2hva(&self, ipa: usize) -> usize {
        let mask = (1 << (HYP_VA_SIZE - VM_IPA_SIZE)) - 1;
        let prefix = mask << VM_IPA_SIZE;
        if ipa & prefix != 0 {
            error!("ipa2hva: VM {} access invalid ipa {:x}", self.id(), ipa);
            return 0;
        }
        let prefix = prefix - ((self.id() & mask) << VM_IPA_SIZE);
        prefix | ipa
    }

    #[cfg(feature = "balloon")]
    pub fn inflate_balloon(&self, guest_addr: usize, len: usize) {
        use crate::arch::PAGE_SIZE;
        if len != PAGE_SIZE {
            error!("len {:#x} not handable", len);
            return;
        }
        let pa = self.ipa2pa(guest_addr).unwrap();
        debug!("inflate_balloon: remove guest_addr {guest_addr:#x} -> pa {pa:#x}");
        let mut inner = self.inner_mut.lock();
        let mut tmp = vec![];
        for region in inner.color_pa_info.region_list.iter_mut() {
            if region.contains(&pa) {
                if let Some(new_region) = region.split(pa) {
                    debug!("append new region {:x?}", new_region);
                    tmp.push(new_region);
                }
            }
        }
        inner.color_pa_info.region_list.retain(|region| !region.is_empty());
        inner.color_pa_info.region_list.append(&mut tmp);
        inner.balloon.push(guest_addr);
        drop(inner);
        self.pt_unmap_range(guest_addr, len, false);
    }
}

#[derive(Default, Debug, raii::RAII)]
struct VmColorPaInfo {
    region_list: Vec<ColorMemRegion>,
}

impl Drop for VmColorPaInfo {
    fn drop(&mut self) {
        for region in self.region_list.iter() {
            mem_color_region_free(region);
        }
    }
}

struct VmInnerMut {
    // memory config
    pt: PageTable,
    color_pa_info: VmColorPaInfo,
    #[cfg(feature = "iommu")]
    iommu_ctx_id: Option<usize>,

    #[cfg(feature = "balloon")]
    balloon: Vec<usize>,

    // VM timer
    #[cfg(feature = "vtimer")]
    running: usize,
    #[cfg(feature = "vtimer")]
    vtimer_offset: usize,
    #[cfg(feature = "vtimer")]
    vtimer: usize,
}

impl VmInnerMut {
    fn new() -> Self {
        Self {
            pt: if let Ok(pt_dir_frame) = mem_page_alloc() {
                PageTable::new(pt_dir_frame, true)
            } else {
                panic!("vmm_init_memory: page alloc failed");
            },
            color_pa_info: VmColorPaInfo::default(),
            #[cfg(feature = "iommu")]
            iommu_ctx_id: None,
            #[cfg(feature = "balloon")]
            balloon: vec![],
            #[cfg(feature = "vtimer")]
            running: 0,
            #[cfg(feature = "vtimer")]
            vtimer_offset: super::timer::get_counter(),
            #[cfg(feature = "vtimer")]
            vtimer: 0,
        }
    }
}

static VM_LIST: Mutex<Vec<Arc<Vm>>> = Mutex::new(Vec::new());

#[inline]
pub fn vm_list_walker<F>(mut f: F)
where
    F: FnMut(&Arc<Vm>),
{
    let vm_list = VM_LIST.lock();
    for vm in vm_list.iter() {
        f(vm);
    }
}

pub fn push_vm(id: usize, config: VmConfigEntry) -> Result<Arc<Vm>, ()> {
    let mut vm_list = VM_LIST.lock();
    if id >= CONFIG_VM_NUM_MAX || vm_list.iter().any(|x| x.id() == id) {
        error!("push_vm: vm {} already exists", id);
        Err(())
    } else {
        let vm = Vm::new(id, config);
        vm_list.push(vm.clone());
        Ok(vm)
    }
}

pub fn remove_vm(id: usize) -> Arc<Vm> {
    let mut vm_list = VM_LIST.lock();
    match vm_list.iter().position(|x| x.id() == id) {
        None => {
            panic!("VM[{}] not exist in VM LIST", id);
        }
        Some(idx) => vm_list.remove(idx),
    }
}

pub fn vm_by_id(id: usize) -> Option<Arc<Vm>> {
    let vm_list = VM_LIST.lock();
    vm_list.iter().find(|&x| x.id() == id).cloned()
}
