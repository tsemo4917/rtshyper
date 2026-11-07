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

/// `source_mode` is `SPSR_EL2.M` of the trapped guest instruction.
pub fn undef_target(source_mode: u64) -> UndefTarget {
    match source_mode & 0xf {
        EL0T => UndefTarget {
            vector_offset: 0x400,
            target_mode: EL1H,
        },
        EL1T => UndefTarget {
            vector_offset: 0x000,
            target_mode: EL1T,
        },
        _ => UndefTarget {
            vector_offset: 0x200,
            target_mode: EL1H,
        },
    }
}

/// `PSTATE.PAN` after taking an exception to EL1.
///
/// `span_clear` is `SCTLR_EL1.SPAN == 0`. Otherwise PAN is preserved from
/// the trapped context.
pub fn pan_on_entry(span_clear: bool, source_pan: bool) -> bool {
    span_clear || source_pan
}

/// ESR_EL1 for an unknown reason. `il` is the trapped instruction's IL bit.
pub fn unknown_esr(il: bool) -> u64 {
    if il {
        1 << 25
    } else {
        0
    }
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
        assert_eq!(
            undef_target(0b0100),
            UndefTarget {
                vector_offset: 0x000,
                target_mode: 0b0100
            }
        );
        assert_eq!(
            undef_target(0b0101),
            UndefTarget {
                vector_offset: 0x200,
                target_mode: 0b0101
            }
        );
        assert!(pan_on_entry(true, false));
        assert!(!pan_on_entry(false, false));
        assert!(pan_on_entry(false, true));
        assert_eq!(unknown_esr(true), 1 << 25);
        assert_eq!(unknown_esr(false), 0);
    }
}
