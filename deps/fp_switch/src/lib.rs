//! Decisions for lazy FP/SIMD ownership.
//!
//! The hypervisor applies these results to `CPTR_EL2` and the per-core owner.
//! It does not save or restore the Q registers on its own; callers do that
//! only for the actions returned here.

#![no_std]

/// What to do with the FP registers when a vCPU traps on its first access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrapAction {
    /// Hardware already holds this vCPU's latest state.
    KeepHardware,
    /// Write the previous owner's registers to memory, then load this vCPU.
    SavePrevious,
    /// Nobody owns the registers. Load this vCPU from its memory copy.
    RestoreOnly,
}

/// `true` when entry must set `CPTR_EL2.TFP` so the next FP access traps.
pub fn trap_fp_on_enter(owner_is_entering: bool) -> bool {
    !owner_is_entering
}

pub fn trap_action(has_owner: bool, owner_is_current: bool) -> TrapAction {
    if owner_is_current {
        TrapAction::KeepHardware
    } else if has_owner {
        TrapAction::SavePrevious
    } else {
        TrapAction::RestoreOnly
    }
}

/// `true` when `removed` currently owns the live registers and they must be
/// written back before ownership is cleared.
pub fn flush_on_remove(owner_is_removed: bool) -> bool {
    owner_is_removed
}

/// Where an undefined-instruction exception injected into EL1 should land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UndefTarget {
    pub vector_offset: u64,
    /// `SPSR_EL2.M` value of the EL1 handler.
    pub target_mode: u64,
}

const EL0T: u64 = 0b0000;
const EL1T: u64 = 0b0100;
const EL1H: u64 = 0b0101;
const AARCH32: u64 = 0b1_0000;

/// `source_mode` is `SPSR_EL2.M` of the trapped guest instruction.
///
/// The handler always runs at EL1h: exception entry sets `PSTATE.SP` to 1,
/// so the processor uses `SP_EL1` even though the vector slot is selected
/// by the *interrupted* stack pointer.
pub fn undef_target(source_mode: u64) -> UndefTarget {
    match source_mode & 0xf {
        EL0T => UndefTarget {
            vector_offset: 0x400,
            target_mode: EL1H,
        },
        EL1T => UndefTarget {
            vector_offset: 0x000,
            target_mode: EL1H,
        },
        _ => UndefTarget {
            vector_offset: 0x200,
            target_mode: EL1H,
        },
    }
}

/// Where a synchronous abort injected into EL1 should land.
///
/// `source_mode` is `SPSR_EL2.M` of the trapped context. Unlike an undefined
/// instruction, an abort can come from an EL0 task in AArch32 state (M[4]
/// set), which takes the lower-EL AArch32 vector slot.
pub fn abort_target(source_mode: u64) -> UndefTarget {
    if source_mode & AARCH32 != 0 {
        return UndefTarget {
            vector_offset: 0x600,
            target_mode: EL1H,
        };
    }
    undef_target(source_mode)
}

// PSTATE (SPSR) fields carried across exception entry to EL1.
const SPSR_DAIF: u64 = (1 << 9) | (1 << 8) | (1 << 7) | (1 << 6);
const SPSR_NZCV: u64 = 0xF << 28;
const SPSR_PAN: u64 = 1 << 22;
const SPSR_DIT: u64 = 1 << 24;
const SPSR_SSBS: u64 = 1 << 12;
// PSTATE bits entry clears because the whitelist above never sets them:
// Software Step (21), Illegal execution state (20), UAO (23).
// BTYPE is [11:10] and TCO (25) is set only when MTE is implemented.

/// PSTATE after `AArch64.TakeException` for a synchronous exception to EL1,
/// starting from `source_spsr` (`SPSR_EL2` of the trapped context).
///
/// `span_clear` is `SCTLR_EL1.SPAN == 0` (bit 23, forces PAN on) and `dssbs`
/// is `SCTLR_EL1.DSSBS` (bit 44, the default `PSTATE.SSBS` on exception
/// entry). `ssbs` and `mte` tell whether `ID_AA64PFR1_EL1` reports FEAT_SSBS
/// (field [7:4]) and FEAT_MTE (field [11:8]): without the features the
/// corresponding PSTATE bits are RES0, with them entry assigns SSBS from
/// DSSBS (not from the trapped context) and sets TCO.
pub fn exception_entry_spsr(source_spsr: u64, span_clear: bool, dssbs: bool, ssbs: bool, mte: bool) -> u64 {
    let mut spsr = (source_spsr & (SPSR_NZCV | SPSR_PAN | SPSR_DIT)) | EL1H | SPSR_DAIF;
    if ssbs && dssbs {
        spsr |= SPSR_SSBS;
    }
    if mte {
        spsr |= 1 << 25;
    }
    if span_clear || source_spsr & SPSR_PAN != 0 {
        spsr |= SPSR_PAN;
    }
    spsr
}

// ESR_EL1 fields of an injected exception.
const ESR_EC_SHIFT: u64 = 26;
const ESR_IL: u64 = 1 << 25;
// Synchronous external abort, not on a translation table walk.
pub const FSC_EXTABT: u64 = 0x10;
const ESR_ISS_WNR: u64 = 1 << 6;
// Cache maintenance access (data aborts only).
const ESR_ISS_CM: u64 = 1 << 8;
const ESR_ISS_FNV: u64 = 1 << 10;

/// ESR_EL1 for an injected synchronous abort with fault status `fsc`
/// (ISS bits [5:0]).
///
/// `wnr` and `cm` report the attributes of the trapped access (data aborts
/// only; RES0 for instruction aborts). `far_valid` mirrors the validity of
/// the fault address: an invalid input FAR leaves `FAR_EL1` untouched and
/// reports FnV. IL is always set: an instruction-fetch abort and the
/// ISV-less data abort of a 32-bit guest instruction both report a 32-bit
/// instruction length.
pub fn abort_esr(ec: u64, fsc: u64, far_valid: bool, wnr: bool, cm: bool) -> u64 {
    let mut esr = (ec << ESR_EC_SHIFT) | ESR_IL | fsc;
    if wnr {
        esr |= ESR_ISS_WNR;
    }
    if cm {
        esr |= ESR_ISS_CM;
    }
    if !far_valid {
        esr |= ESR_ISS_FNV;
    }
    esr
}

/// ESR_EL1 for an injected undefined-instruction exception: EC 0, IL 1 (a
/// 32-bit guest instruction), no instruction syndrome.
pub fn unknown_esr() -> u64 {
    ESR_IL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Saved([Option<u64>; 4]);

    impl Saved {
        fn new() -> Self {
            Self([None; 4])
        }

        fn get(&self, id: u8) -> Option<u64> {
            self.0[id as usize]
        }

        fn insert(&mut self, id: u8, value: u64) {
            self.0[id as usize] = Some(value);
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Core {
        owner: Option<u8>,
        tfp: bool,
        hardware: u64,
        active: Option<u8>,
    }

    impl Core {
        fn new() -> Self {
            Self {
                owner: None,
                tfp: true,
                hardware: 0,
                active: None,
            }
        }

        fn enter(&mut self, id: u8) {
            self.active = Some(id);
            self.tfp = trap_fp_on_enter(self.owner == Some(id));
        }

        fn idle(&mut self) {
            self.active = None;
            self.tfp = true;
        }

        fn trap(&mut self, saved: &mut Saved) {
            let current = self.active.expect("trap requires an active vcpu");
            let action = trap_action(self.owner.is_some(), self.owner == Some(current));
            self.tfp = false;
            match action {
                TrapAction::KeepHardware => {}
                TrapAction::SavePrevious => {
                    let prev = self.owner.expect("owner");
                    saved.insert(prev, self.hardware);
                    self.hardware = saved.get(current).unwrap_or(0);
                    self.owner = Some(current);
                }
                TrapAction::RestoreOnly => {
                    self.hardware = saved.get(current).unwrap_or(0);
                    self.owner = Some(current);
                }
            }
        }

        fn guest_fp(&mut self, id: u8, new_value: u64, saved: &mut Saved) {
            assert_eq!(self.active, Some(id));
            if self.tfp {
                self.trap(saved);
            }
            assert!(!self.tfp);
            assert_eq!(self.owner, Some(id));
            self.hardware = new_value;
        }

        fn remove(&mut self, id: u8, saved: &mut Saved) {
            if flush_on_remove(self.owner == Some(id)) {
                saved.insert(id, self.hardware);
                self.owner = None;
                self.tfp = true;
            }
            if self.active == Some(id) {
                self.active = None;
            }
        }
    }

    #[test]
    fn first_use_then_switch_without_fp_keeps_live_regs() {
        let mut core = Core::new();
        let mut saved = Saved::new();
        core.enter(1);
        core.guest_fp(1, 11, &mut saved);
        assert!(saved.get(1).is_none());

        core.enter(2);
        assert!(core.tfp);
        assert_eq!(core.hardware, 11);

        core.enter(1);
        assert!(!core.tfp);
        assert_eq!(core.hardware, 11);
        assert!(saved.get(1).is_none());
    }

    #[test]
    fn both_vcpus_using_fp_do_not_cross() {
        let mut core = Core::new();
        let mut saved = Saved::new();
        core.enter(1);
        core.guest_fp(1, 11, &mut saved);
        core.enter(2);
        core.guest_fp(2, 22, &mut saved);
        assert_eq!(saved.get(1), Some(11));
        assert_eq!(core.hardware, 22);

        core.enter(1);
        core.guest_fp(1, 12, &mut saved);
        assert_eq!(saved.get(2), Some(22));
        assert_eq!(core.hardware, 12);
    }

    #[test]
    fn idle_then_wake_does_not_reload_zeros() {
        let mut core = Core::new();
        let mut saved = Saved::new();
        core.enter(1);
        core.guest_fp(1, 11, &mut saved);
        core.idle();
        assert!(core.tfp);
        assert_eq!(core.owner, Some(1));

        core.enter(1);
        assert!(!core.tfp);
        assert_eq!(core.hardware, 11);
        core.guest_fp(1, 12, &mut saved);
        assert_eq!(core.hardware, 12);
    }

    #[test]
    fn remove_flushes_live_regs_before_the_next_vcpu() {
        let mut core = Core::new();
        let mut saved = Saved::new();
        core.enter(1);
        core.guest_fp(1, 11, &mut saved);
        core.remove(1, &mut saved);
        assert_eq!(saved.get(1), Some(11));
        assert!(core.owner.is_none());
        assert!(core.tfp);

        core.enter(2);
        core.guest_fp(2, 22, &mut saved);
        assert_eq!(core.hardware, 22);
        assert_eq!(saved.get(1), Some(11));
    }

    #[test]
    fn sve_undef_targets_el1_vector() {
        assert_eq!(
            undef_target(0b0000),
            UndefTarget {
                vector_offset: 0x400,
                target_mode: 0b0101
            }
        );
        // Exception entry sets PSTATE.SP: even from EL1t the handler runs at
        // EL1h, only the vector slot keeps the interrupted SP_EL0 column.
        assert_eq!(
            undef_target(0b0100),
            UndefTarget {
                vector_offset: 0x000,
                target_mode: 0b0101
            }
        );
        assert_eq!(
            undef_target(0b0101),
            UndefTarget {
                vector_offset: 0x200,
                target_mode: 0b0101
            }
        );
    }

    #[test]
    fn injected_abort_landing_slots() {
        // Every injected exception lands the handler at EL1h; the vector
        // slot follows the *interrupted* mode (Arm ARM vector table).
        assert_eq!(
            abort_target(0b0100),
            UndefTarget {
                vector_offset: 0x000,
                target_mode: 0b0101
            }
        );
        assert_eq!(
            abort_target(0b0101),
            UndefTarget {
                vector_offset: 0x200,
                target_mode: 0b0101
            }
        );
        assert_eq!(
            abort_target(0b0000),
            UndefTarget {
                vector_offset: 0x400,
                target_mode: 0b0101
            }
        );
        // An EL0 task in AArch32 state takes the lower-EL AArch32 slot.
        assert_eq!(
            abort_target(0b10000),
            UndefTarget {
                vector_offset: 0x600,
                target_mode: 0b0101
            }
        );
    }

    #[test]
    fn exception_entry_pstate_follows_takeexception() {
        // Live NZCV survives, DAIF is fully masked, the handler is at EL1h,
        // and DIT is preserved.
        let src = 0xF0000000 | 0b0101 | (1 << 24) | (1 << 12);
        let dst = exception_entry_spsr(src, false, false, false, false);
        assert_eq!(dst & 0xF0000000, 0xF0000000);
        assert_eq!(dst & 0x3C0, 0x3C0);
        assert_eq!(dst & 0x1F, 0b0101);
        assert_eq!(dst, 0xF00003C5 | (1 << 24));
        // Entry clears SS (21), IL (20), UAO (23), BTYPE ([11:10]) and the
        // source SSBS even when the source carries them: with FEAT_SSBS the
        // entry value comes from SCTLR_EL1.DSSBS, not from the source.
        let dirty = src | (1 << 21) | (1 << 20) | (1 << 23) | (0b11 << 10);
        assert_eq!(
            exception_entry_spsr(dirty, false, false, false, false),
            0xF00003C5 | (1 << 24)
        );
        // SSBS is assigned from DSSBS only when FEAT_SSBS is reported.
        assert_eq!(
            exception_entry_spsr(1 << 12, false, true, true, false) & (1 << 12),
            1 << 12
        );
        assert_eq!(exception_entry_spsr(1 << 12, false, false, true, false) & (1 << 12), 0);
        // FEAT_MTE sets TCO on exception entry; without it the bit stays
        // RES0 even when the source carries it.
        assert_eq!(exception_entry_spsr(0, false, false, false, true), 0x20003C5);
        assert_eq!(exception_entry_spsr(1 << 25, false, false, false, false), 0x3C5);
        // SCTLR_EL1.SPAN == 0 forces PAN on, even from a source without it;
        // otherwise the source PAN is preserved.
        assert_eq!(exception_entry_spsr(0, true, false, false, false), 0x4003C5);
        assert_eq!(exception_entry_spsr(1 << 22, false, false, false, false), 0x4003C5);
    }

    #[test]
    fn injected_esr_matches_the_architecture() {
        // Data abort at EL1, plain 64-bit load / store: EC 0x25, IL 1,
        // synchronous external abort, WnR tells the direction.
        assert_eq!(abort_esr(0x25, FSC_EXTABT, true, false, false), 0x96000010);
        assert_eq!(abort_esr(0x25, FSC_EXTABT, true, true, false), 0x96000050);
        // The same access from EL0: EC 0x24.
        assert_eq!(abort_esr(0x24, FSC_EXTABT, true, false, false), 0x92000010);
        assert_eq!(abort_esr(0x24, FSC_EXTABT, true, true, false), 0x92000050);
        // Instruction abort at EL1 / from EL0: EC 0x21 / 0x20, IL 1.
        assert_eq!(abort_esr(0x21, FSC_EXTABT, true, false, false), 0x86000010);
        assert_eq!(abort_esr(0x20, FSC_EXTABT, true, false, false), 0x82000010);
        // A cache-maintenance access keeps its CM attribute (ISS bit 8).
        assert_eq!(abort_esr(0x25, FSC_EXTABT, true, false, true), 0x96000110);
        assert_eq!(abort_esr(0x25, FSC_EXTABT, true, true, true), 0x96000150);
        // An invalid input FAR is reported as FnV; FAR_EL1 stays untouched.
        assert_eq!(abort_esr(0x25, FSC_EXTABT, false, true, false), 0x96000450);
        // The fault status is the caller's choice: a stage-2 L3 permission
        // refusal (DFSC 0x0f) is re-presented as such, not as an external
        // abort — including when the refusal hit the guest's stage-1 walk
        // (S1PTW set): the permission fault status wins over reporting a
        // walk abort.
        assert_eq!(abort_esr(0x25, 0x0f, true, true, false), 0x9600004f);
        let trapped = 0x0f | (1 << 7) | (1 << 6); // S1PTW + WnR + L3 permission
        assert_eq!(abort_esr(0x25, trapped & 0b111111, true, true, false), 0x9600004f);
        // Undefined: EC 0, IL 1, ISS 0.
        assert_eq!(unknown_esr(), 0x02000000);
    }
}
