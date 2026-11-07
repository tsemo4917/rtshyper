pub use self::cache::*;
pub use self::context_frame::*;
pub use self::gic::*;
pub use self::interface::*;
pub use self::interrupt::*;
pub use self::mmu::PLATFORM_PHYSICAL_LIMIT_GB;
pub use self::page_table::*;
pub use self::psci::*;
#[cfg(feature = "smmuv2")]
pub use self::smmu::*;
pub use self::vgic::*;
pub use pmuv3::arch_pmu_init;
#[cfg(feature = "memory-reservation")]
pub use pmuv3::{vcpu_start_pmu, vcpu_stop_pmu, PmuTimerEvent};

#[macro_use]
mod regs;

#[allow(dead_code)]
mod cache;
mod context_frame;
mod cpu;
#[allow(dead_code)]
mod exception;
mod fpsimd;
pub use fpsimd::{on_idle, on_vcpu_enter, on_vcpu_remove};
#[allow(dead_code)]
mod gic;
mod interface;
mod interrupt;
mod mmu;
#[allow(dead_code)]
mod page_table;
mod pmuv3;
mod psci;
mod smc;
#[allow(dead_code)]
#[cfg(feature = "smmuv2")]
mod smmu;
mod start;
mod sync;
pub mod timer;
mod tlb;
mod vcpu;
mod vgic;
mod vm;

pub struct SmmuDesc {
    pub base: usize,
    pub interrupt_id: usize,
    pub global_mask: u16,
}
