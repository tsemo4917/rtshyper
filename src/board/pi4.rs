use crate::arch::GicDesc;
use crate::arch::SmmuDesc;

use super::platform_common::{
    ArchDesc, PlatCpuConfig, PlatCpuCoreConfig, PlatMemoryConfig, PlatOperation, PlatformConfig, SchedRule,
};

pub struct Platform;

impl PlatOperation for Platform {
    const UART_0_ADDR: usize = 0xFE201000;
    const UART_1_ADDR: usize = 0xFE201400;

    const UART_0_INT: usize = 32 + 0x79;
    const UART_1_INT: usize = 32 + 0x79;

    const HYPERVISOR_UART_BASE: usize = Self::UART_0_ADDR;

    const GICD_BASE: usize = 0xFF841000;
    const GICC_BASE: usize = 0xFF842000;
    const GICH_BASE: usize = 0xFF844000;
    const GICV_BASE: usize = 0xFF846000;

    fn cpuid_to_cpuif(cpuid: usize) -> usize {
        cpuid
    }

    fn cpuif_to_cpuid(cpuif: usize) -> usize {
        cpuif
    }

    #[inline]
    fn device_regions() -> &'static [core::ops::Range<usize>] {
        static DEVICES: &[core::ops::Range<usize>] = &[0x0_fc00_0000..0x1_0000_0000];
        DEVICES
    }

    #[inline]
    fn pmu_irq_list() -> &'static [usize] {
        &[]
    }
}

pub static PLAT_DESC: PlatformConfig = PlatformConfig {
    cpu_desc: PlatCpuConfig {
        num: 4,
        core_list: &[
            PlatCpuCoreConfig {
                mpidr: 0x80000000,
                sched: SchedRule::RoundRobin,
            },
            PlatCpuCoreConfig {
                mpidr: 0x80000001,
                sched: SchedRule::RoundRobin,
            },
            PlatCpuCoreConfig {
                mpidr: 0x80000002,
                sched: SchedRule::RoundRobin,
            },
            PlatCpuCoreConfig {
                mpidr: 0x80000003,
                sched: SchedRule::RoundRobin,
            },
        ],
    },
    mem_desc: PlatMemoryConfig {
        regions: &[
            0xf0000000..0xf0000000 + 0xc000000,
            0x200000..0x3e000000,
            0x40000000..0xf0000000,
            0x100000000..0x100000000 + 0x100000000,
        ],
        base: 0xf0000000,
    },
    arch_desc: ArchDesc {
        gic_desc: GicDesc {
            gicd_addr: Platform::GICD_BASE,
            gicc_addr: Platform::GICC_BASE,
            gich_addr: Platform::GICH_BASE,
            gicv_addr: Platform::GICV_BASE,
            maintenance_int_id: 25,
        },
        smmu_desc: SmmuDesc {
            base: 0,
            interrupt_id: 0,
            global_mask: 0,
        },
    },
};
