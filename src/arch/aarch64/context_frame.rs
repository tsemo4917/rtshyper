use core::arch::global_asm;

use cortex_a::registers::*;

use super::timer::GenericTimerContext;

global_asm!(include_str!("fpsimd.S"));

extern "C" {
    fn fpsimd_save_ctx(fpsimd_addr: usize);
    fn fpsimd_restore_ctx(fpsimd_addr: usize);
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Aarch64ContextFrame {
    gpr: [u64; 31],
    spsr: u64,
    elr: u64,
    sp: u64,
}

impl core::fmt::Display for Aarch64ContextFrame {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for i in 0..31 {
            write!(f, "x{:02}: {:016x}   ", i, self.gpr[i])?;
            if (i + 1) % 2 == 0 {
                writeln!(f)?;
            }
        }
        writeln!(f, "spsr:{:016x}", self.spsr)?;
        write!(f, "elr: {:016x}", self.elr)?;
        writeln!(f, "   sp:  {:016x}", self.sp)?;
        Ok(())
    }
}

impl crate::arch::ContextFrameTrait for Aarch64ContextFrame {
    fn exception_pc(&self) -> usize {
        self.elr as usize
    }

    fn set_exception_pc(&mut self, pc: usize) {
        self.elr = pc as u64;
    }

    fn stack_pointer(&self) -> usize {
        self.sp as usize
    }

    fn set_stack_pointer(&mut self, sp: usize) {
        self.sp = sp as u64;
    }

    fn set_argument(&mut self, arg: usize) {
        self.gpr[0] = arg as u64;
    }

    fn set_gpr(&mut self, index: usize, val: usize) {
        self.gpr[index] = val as u64;
    }

    fn gpr(&self, index: usize) -> usize {
        self.gpr[index] as usize
    }
}

impl Aarch64ContextFrame {
    pub fn new_privileged() -> Self {
        Self {
            spsr: (SPSR_EL2::M::EL2h + SPSR_EL2::F::Masked + SPSR_EL2::A::Masked + SPSR_EL2::D::Masked).value,
            ..Default::default()
        }
    }
}

impl Default for Aarch64ContextFrame {
    fn default() -> Self {
        Self {
            gpr: [0; 31],
            spsr: (SPSR_EL2::M::EL1h
                + SPSR_EL2::I::Masked
                + SPSR_EL2::F::Masked
                + SPSR_EL2::A::Masked
                + SPSR_EL2::D::Masked)
                .value,
            elr: 0,
            sp: 0,
        }
    }
}

#[repr(C, align(16))]
#[derive(Copy, Clone, Debug)]
struct FpsimdState {
    fpsimd: [u64; 64],
    fpsr: u32,
    fpcr: u32,
}

impl Default for FpsimdState {
    fn default() -> Self {
        Self {
            fpsimd: [0; 64],
            fpsr: 0,
            fpcr: 0,
        }
    }
}

impl FpsimdState {
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        *self = Default::default();
    }
}

#[repr(C, align(16))]
#[derive(Debug, Copy, Clone, Default)]
pub struct VmContext {
    pub generic_timer: GenericTimerContext,

    // vpidr and vmpidr
    // vpidr_el2: u32,
    pub vmpidr_el2: u64,

    // 64bit EL1/EL0 register
    sp_el0: u64,
    sp_el1: u64,
    elr_el1: u64,
    spsr_el1: u32,
    sctlr_el1: u32,
    actlr_el1: u64,
    cpacr_el1: u32,
    ttbr0_el1: u64,
    ttbr1_el1: u64,
    tcr_el1: u64,
    esr_el1: u32,
    far_el1: u64,
    par_el1: u64,
    mair_el1: u64,
    amair_el1: u64,
    vbar_el1: u64,
    contextidr_el1: u32,
    tpidr_el0: u64,
    tpidr_el1: u64,
    tpidrro_el0: u64,

    // hypervisor context
    pub hcr_el2: u64,
    cptr_el2: u64,
    hstr_el2: u64,
    #[cfg(not(feature = "memory-reservation"))]
    pub pmcr_el0: u64,
    // pub vtcr_el2: u64,

    // exception
    far_el2: u64,
    hpfar_el2: u64,
    fpsimd: FpsimdState,
}

impl VmContext {
    pub fn new() -> Self {
        Self {
            sctlr_el1: 0x30C50830,
            ..Default::default()
        }
    }

    #[allow(dead_code)]
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn ext_regs_store(&mut self) {
        // MRS!("self.vpidr_el2, VPIDR_EL2, "x");
        mrs!(self.vmpidr_el2, VMPIDR_EL2);

        mrs!(self.sp_el0, SP_EL0);
        mrs!(self.sp_el1, SP_EL1);
        mrs!(self.elr_el1, ELR_EL1);
        mrs!(self.spsr_el1, SPSR_EL1, "x");
        mrs!(self.sctlr_el1, SCTLR_EL1, "x");
        mrs!(self.cpacr_el1, CPACR_EL1, "x");
        mrs!(self.ttbr0_el1, TTBR0_EL1);
        mrs!(self.ttbr1_el1, TTBR1_EL1);
        mrs!(self.tcr_el1, TCR_EL1);
        mrs!(self.esr_el1, ESR_EL1, "x");
        mrs!(self.far_el1, FAR_EL1);
        mrs!(self.par_el1, PAR_EL1);
        mrs!(self.mair_el1, MAIR_EL1);
        mrs!(self.amair_el1, AMAIR_EL1);
        mrs!(self.vbar_el1, VBAR_EL1);
        mrs!(self.contextidr_el1, CONTEXTIDR_EL1, "x");
        mrs!(self.tpidr_el0, TPIDR_EL0);
        mrs!(self.tpidr_el1, TPIDR_EL1);
        mrs!(self.tpidrro_el0, TPIDRRO_EL0);

        #[cfg(not(feature = "memory-reservation"))]
        mrs!(self.pmcr_el0, PMCR_EL0);
        // mrs!(self.vtcr_el2, VTCR_EL2);
        mrs!(self.hcr_el2, HCR_EL2);
        // MRS!(self.cptr_el2, CPTR_EL2);
        // MRS!(self.hstr_el2, HSTR_EL2);
        // MRS!(self.far_el2, FAR_EL2);
        // MRS!(self.hpfar_el2, HPFAR_EL2);
        mrs!(self.actlr_el1, ACTLR_EL1);
        self.generic_timer.save();
        unsafe {
            fpsimd_save_ctx(&self.fpsimd as *const _ as usize);
        }
    }

    pub fn ext_regs_restore(&self) {
        self.generic_timer.restore();

        // MSR!(VPIDR_EL2, self.vpidr_el2, "x");
        msr!(VMPIDR_EL2, self.vmpidr_el2);

        msr!(SP_EL0, self.sp_el0);
        msr!(SP_EL1, self.sp_el1);
        msr!(ELR_EL1, self.elr_el1);
        msr!(SPSR_EL1, self.spsr_el1, "x");
        msr!(SCTLR_EL1, self.sctlr_el1, "x");
        msr!(CPACR_EL1, self.cpacr_el1, "x");
        msr!(TTBR0_EL1, self.ttbr0_el1);
        msr!(TTBR1_EL1, self.ttbr1_el1);
        msr!(TCR_EL1, self.tcr_el1);
        msr!(ESR_EL1, self.esr_el1, "x");
        msr!(FAR_EL1, self.far_el1);
        msr!(PAR_EL1, self.par_el1);
        msr!(MAIR_EL1, self.mair_el1);
        msr!(AMAIR_EL1, self.amair_el1);
        msr!(VBAR_EL1, self.vbar_el1);
        msr!(CONTEXTIDR_EL1, self.contextidr_el1, "x");
        msr!(TPIDR_EL0, self.tpidr_el0);
        msr!(TPIDR_EL1, self.tpidr_el1);
        msr!(TPIDRRO_EL0, self.tpidrro_el0);

        #[cfg(not(feature = "memory-reservation"))]
        msr!(PMCR_EL0, self.pmcr_el0);
        // msr!(VTCR_EL2, self.vtcr_el2);
        msr!(HCR_EL2, self.hcr_el2);
        // MSR!(CPTR_EL2, self.cptr_el2);
        // MSR!(HSTR_EL2, self.hstr_el2);
        // MSR!(FAR_EL2, self.far_el2);
        // MSR!(HPFAR_EL2, self.hpfar_el2);
        msr!(ACTLR_EL1, self.actlr_el1);
        unsafe {
            fpsimd_restore_ctx(&self.fpsimd as *const _ as usize);
        }
    }
}
