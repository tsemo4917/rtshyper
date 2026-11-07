use core::ptr;

use spin::Once;

use crate::arch::ArchTrait;
use crate::arch::ContextFrame;
use crate::arch::ContextFrameTrait;
use crate::arch::PageTable;
use crate::arch::{pt_map_banked_cpu, TlbInvalidate, PAGE_SIZE, PTE_PER_PAGE};
use crate::board::{static_config, PLAT_DESC};
use crate::kernel::{Vcpu, Vm};
use crate::util::timer_list::TimerList;

use super::sched::get_scheduler;
use super::vcpu_array::VcpuArray;

pub const CPU_MASTER: usize = 0;
pub const CPU_STACK_SIZE: usize = PAGE_SIZE * 32;

// NOTE: the first page of CpuStack is the guard page
struct CpuStack([u8; CPU_STACK_SIZE + PAGE_SIZE]);

impl CpuStack {
    const fn new() -> Self {
        Self([0; CPU_STACK_SIZE + PAGE_SIZE])
    }
}

impl core::ops::Deref for CpuStack {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[repr(C, align(4096))]
pub struct CpuPt {
    pub lvl1: [usize; PTE_PER_PAGE],
    pub lvl2: [usize; PTE_PER_PAGE],
    pub lvl3: [usize; PTE_PER_PAGE],
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CpuState {
    Inv = 0,
    Idle = 1,
    Run = 2,
}

#[repr(C, align(4096))]
pub struct Cpu {
    pub id: usize,
    pub cpu_state: CpuState,
    pub active_vcpu: Option<Vcpu>,
    ctx: *mut ContextFrame,

    pub vcpu_array: VcpuArray,
    // timer
    pub(super) timer_list: TimerList,

    pub current_irq: usize,
    global_pt: Once<PageTable>,
    pub interrupt_nested: usize,
    pub cpu_pt: CpuPt,
    stack: CpuStack,
    // vCPU whose FP/SIMD registers are currently live on this core.
    // Only the lazy-switch path updates it. Placed after `stack` so
    // CPU_STACK_OFFSET stays page-aligned for the guard page.
    fp_owner: Option<Vcpu>,
}

pub const CPU_STACK_OFFSET: usize = core::mem::offset_of!(Cpu, stack);

impl Cpu {
    const fn default() -> Cpu {
        Cpu {
            id: 0,
            cpu_state: CpuState::Inv,
            active_vcpu: None,
            ctx: ptr::null_mut(),
            vcpu_array: VcpuArray::new(),
            timer_list: TimerList::new(),
            current_irq: 0,
            interrupt_nested: 0,
            global_pt: Once::new(),
            cpu_pt: CpuPt {
                lvl1: [0; PTE_PER_PAGE],
                lvl2: [0; PTE_PER_PAGE],
                lvl3: [0; PTE_PER_PAGE],
            },
            stack: CpuStack::new(),
            fp_owner: None,
        }
    }

    pub fn current_ctx(&self) -> *mut ContextFrame {
        self.ctx
    }

    pub fn set_ctx(&mut self, ctx: *mut ContextFrame) -> *mut ContextFrame {
        let prev = self.ctx;
        self.ctx = ctx;
        prev
    }

    pub fn set_gpr(&mut self, idx: usize, val: usize) {
        if let Some(ctx) = unsafe { self.ctx.as_mut() } {
            ctx.set_gpr(idx, val);
        }
    }

    pub fn get_gpr(&self, idx: usize) -> usize {
        if let Some(ctx) = unsafe { self.ctx.as_ref() } {
            ctx.gpr(idx)
        } else {
            0
        }
    }

    pub fn exception_pc(&self) -> usize {
        if let Some(ctx) = unsafe { self.ctx.as_ref() } {
            ctx.exception_pc()
        } else {
            0
        }
    }

    pub fn set_exception_pc(&mut self, val: usize) {
        if let Some(ctx) = unsafe { self.ctx.as_mut() } {
            ctx.set_exception_pc(val)
        }
    }

    pub(super) fn set_active_vcpu(&mut self, active_vcpu: Option<Vcpu>) {
        self.active_vcpu = active_vcpu;
    }

    pub fn assigned(&self) -> bool {
        self.vcpu_array.vcpu_num() != 0
    }

    pub fn pt(&self) -> &PageTable {
        self.global_pt.get().unwrap()
    }

    fn init_pt(&self, directory: usize) {
        debug!("cpu {} init_pt() pa {:#x}", self.id, directory);
        let pt = PageTable::from_pa(directory, false);
        self.global_pt.call_once(|| pt);
        crate::arch::Arch::invalid_hypervisor_all();
    }

    pub(super) fn reset_pt(&mut self, directory: usize) {
        // reset global pt without calling the destructor of PageTable
        let prev = core::mem::replace(&mut self.global_pt, Once::new());
        core::mem::forget(prev);
        assert!(self.global_pt.get().is_none());
        self.init_pt(directory);
    }

    pub fn stack_top(&self) -> usize {
        self.stack.as_ptr_range().end as usize
    }

    pub(crate) fn fp_owner(&self) -> Option<&Vcpu> {
        self.fp_owner.as_ref()
    }

    pub(crate) fn set_fp_owner(&mut self, owner: Option<Vcpu>) {
        self.fp_owner = owner;
    }
}

#[link_section = ".cpu_private"]
pub static mut CPU: Cpu = Cpu::default();

pub fn current_cpu() -> &'static mut Cpu {
    unsafe { &mut CPU }
}

pub fn active_vcpu_id() -> usize {
    let active_vcpu = current_cpu().active_vcpu.as_ref().unwrap();
    active_vcpu.id()
}

pub fn active_vm() -> Option<alloc::sync::Arc<Vm>> {
    match current_cpu().active_vcpu.as_ref() {
        None => None,
        Some(active_vcpu) => active_vcpu.vm(),
    }
}

fn cpu_init_pt() {
    let cpu = current_cpu();
    let directory = crate::arch::Arch::mem_translate(cpu.cpu_pt.lvl1.as_ptr() as usize).unwrap();
    cpu.init_pt(directory);
}

// TODO: add config for base slice
fn cpu_sched_init() {
    let rule = PLAT_DESC.cpu_desc.core_list[current_cpu().id].sched;
    trace!("cpu[{}] init {rule:?} Scheduler", current_cpu().id);
    current_cpu().vcpu_array.sched.call_once(|| {
        let mut scheduler = get_scheduler(rule);
        info!("core {} init {} scheduler", current_cpu().id, scheduler.name());
        scheduler.init();
        scheduler
    });
}

pub fn cpu_init() {
    let cpu_id = current_cpu().id;
    if cpu_id == 0 {
        use crate::board::{PlatOperation, Platform};
        Platform::power_on_secondary_cores();
    }
    // crate::arch::Arch::disable_prefetch();
    crate::kernel::interrupt_irqchip_init();
    crate::kernel::ipi_init();
    crate::arch::arch_pmu_init();
    cpu_init_pt();
    cpu_sched_init();
    current_cpu().cpu_state = CpuState::Idle;
    let sp = current_cpu().stack.as_ptr() as usize + CPU_STACK_SIZE;
    let size = core::mem::size_of::<ContextFrame>();
    current_cpu().set_ctx((sp - size) as *mut _);
    info!("Core {} init ok", cpu_id);

    crate::util::barrier();
    if cpu_id == 0 {
        info!("Cpu init ok, Bring up {} cores", PLAT_DESC.cpu_desc.num);
    }
}

static mut CPU_LIST: [Cpu; static_config::CORE_NUM] = [const { Cpu::default() }; static_config::CORE_NUM];

pub fn cpu_map_self(cpu_id: usize) -> usize {
    let cpu = unsafe { &mut CPU_LIST[cpu_id] };
    cpu.id = cpu_id;

    pt_map_banked_cpu(cpu)
}
