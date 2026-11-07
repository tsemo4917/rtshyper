//! Lazy FP/SIMD switching for vCPUs that share a physical core.
//!
//! `CPTR_EL2.TFP` (bit 10, nVHE / `HCR_EL2.E2H == 0`) traps the first guest FP
//! access to EL2. `CPACR_EL1` stays the guest's own FPEN and is switched with
//! the rest of the vCPU system registers; once TFP is clear it traps to EL1
//! on its own.
//!
//! `CPTR_EL2.TZ` (bit 8) stays set. SVE is not part of the saved state, so an
//! SVE instruction is reported to EL1 as undefined instead of being retried
//! after a Q-register restore.

use aarch64_cpu::registers::{CPTR_EL2, ELR_EL1, ESR_EL1, ESR_EL2, SCTLR_EL1, SPSR_EL1, VBAR_EL1};
use fp_switch::{pan_on_entry, trap_action, trap_fp_on_enter, undef_target, unknown_esr, TrapAction};
use tock_registers::interfaces::{Readable, Writeable};

use crate::arch::ContextFrame;
use crate::arch::ContextFrameTrait;
use crate::kernel::{current_cpu, Vcpu};

/// Trap Advanced SIMD and floating-point to EL2 when HCR_EL2.E2H is 0.
const CPTR_EL2_TFP: u64 = 1 << 10;
/// Trap SVE instructions to EL2. Takes priority over TFP and reports EC 0x19.
const CPTR_EL2_TZ: u64 = 1 << 8;
const PSTATE_PAN: u64 = 1 << 22;
const SCTLR_SPAN: u64 = 1 << 23;
const PSTATE_DAIF: u64 = (1 << 9) | (1 << 8) | (1 << 7) | (1 << 6);

fn set_fp_trap(trap_fp: bool) {
    let mut cptr = CPTR_EL2.get();
    cptr |= CPTR_EL2_TZ;
    if trap_fp {
        cptr |= CPTR_EL2_TFP;
    } else {
        cptr &= !CPTR_EL2_TFP;
    }
    CPTR_EL2.set(cptr);
    isb!();
}

/// Arm or disarm the EL2 FP trap for `vcpu` according to who owns the live registers.
pub fn on_vcpu_enter(vcpu: &Vcpu) {
    let owned = current_cpu().fp_owner() == Some(vcpu);
    set_fp_trap(trap_fp_on_enter(owned));
}

/// Idle runs at EL2 and does not use FP. Trap accesses so a stray FP instruction
/// cannot overwrite the owner's live registers.
pub fn on_idle() {
    set_fp_trap(true);
}

/// Write back live registers when `vcpu` owns them, then drop ownership.
pub fn on_vcpu_remove(vcpu: &Vcpu) {
    if !fp_switch::flush_on_remove(current_cpu().fp_owner() == Some(vcpu)) {
        return;
    }
    // The save asm uses Q registers, so TFP has to be clear first. TZ stays set.
    set_fp_trap(false);
    vcpu.save_fpsimd();
    current_cpu().set_fp_owner(None);
    // Hardware no longer matches any vCPU. The guest still on this core, if
    // any, must trap before its next FP instruction.
    set_fp_trap(true);
}

/// EC 0x07 from a lower EL. Clear TFP, load this vCPU's FPSIMD state if needed,
/// and return so the faulting instruction is retried.
pub(super) fn handle_trap() {
    let current = match current_cpu().active_vcpu.clone() {
        Some(vcpu) => vcpu,
        None => panic!("core {} fpsimd trap without an active vcpu", current_cpu().id),
    };

    // EL2 save/restore uses Q registers. TFP must be clear before that asm runs,
    // and the retried guest instruction must observe the new value.
    set_fp_trap(false);

    let owner = current_cpu().fp_owner().cloned();
    let action = trap_action(owner.is_some(), owner.as_ref() == Some(&current));
    match action {
        // Hardware already holds this vCPU's state. Restoring the stale memory
        // copy would clobber updates made since it took ownership.
        TrapAction::KeepHardware => {}
        TrapAction::SavePrevious => {
            let prev = owner.expect("fp owner");
            prev.save_fpsimd();
            current.restore_fpsimd();
            current_cpu().set_fp_owner(Some(current));
        }
        TrapAction::RestoreOnly => {
            current.restore_fpsimd();
            current_cpu().set_fp_owner(Some(current));
        }
    }
}

/// EC 0x19. SVE register state is not switched, so the instruction must not
/// be retried. EL1 receives an undefined-instruction exception instead.
pub(super) fn handle_sve_trap(ctx: &mut ContextFrame) {
    let source_spsr = ctx.spsr();
    let source_elr = ctx.exception_pc() as u64;
    let target = undef_target(source_spsr & 0xf);
    let span_clear = SCTLR_EL1.get() & SCTLR_SPAN == 0;
    let pan = pan_on_entry(span_clear, source_spsr & PSTATE_PAN != 0);
    let mut new_spsr = target.target_mode | PSTATE_DAIF;
    if pan {
        new_spsr |= PSTATE_PAN;
    }
    let il = ESR_EL2.read(ESR_EL2::IL) != 0;

    ELR_EL1.set(source_elr);
    SPSR_EL1.set(source_spsr);
    ESR_EL1.set(unknown_esr(il));
    ctx.set_spsr(new_spsr);
    ctx.set_exception_pc(VBAR_EL1.get() as usize + target.vector_offset as usize);
}
