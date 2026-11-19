use core::arch::global_asm;

// use alloc::collections::BinaryHeap;
// use spin::{Mutex, Lazy};
use aarch64_cpu::registers::{ELR_EL1, ESR_EL1, ESR_EL2, FAR_EL1, ID_AA64PFR1_EL1, SCTLR_EL1, SPSR_EL1, VBAR_EL1};
pub use fp_switch::FSC_EXTABT;
use fp_switch::{abort_esr, abort_target, exception_entry_spsr, unknown_esr};
use tock_registers::interfaces::*;

use ffi_interface::c_interface;

use crate::arch::{ContextFrame, ContextFrameTrait, InterruptController};
use crate::kernel::interrupt_handler;
use crate::kernel::{active_vm, current_cpu};

use super::sync::{data_abort_handler, hvc_handler, smc_handler, sysreg_handler};
use super::{interrupt_arch_deactive_irq, IntCtrl};

global_asm!(
    include_str!("exception.S"),
    CONTEXT_SIZE = const core::mem::size_of::<crate::arch::ContextFrame>(),
);

#[inline(always)]
pub fn exception_esr() -> usize {
    ESR_EL2.get() as usize
}

#[inline(always)]
fn exception_far() -> usize {
    aarch64_cpu::registers::FAR_EL2.get() as usize
}

#[inline(always)]
fn exception_hpfar() -> usize {
    let hpfar: u64;
    mrs!(hpfar, HPFAR_EL2);
    hpfar as usize
}

#[allow(non_upper_case_globals)]
const ESR_ELx_S1PTW_SHIFT: usize = 7;
#[allow(non_upper_case_globals)]
const ESR_ELx_S1PTW: usize = 1 << ESR_ELx_S1PTW_SHIFT;

fn translate_far_to_hpfar(far: usize) -> Result<usize, ()> {
    /*
     * We have
     *	PAR[PA_Shift - 1 : 12] = PA[PA_Shift - 1 : 12]
     *	HPFAR[PA_Shift - 9 : 4]  = FIPA[PA_Shift - 1 : 12]
     */
    // #define PAR_TO_HPFAR(par) (((par) & GENMASK_ULL(PHYS_MASK_SHIFT - 1, 12)) >> 8)
    fn par_to_far(par: u64) -> u64 {
        let mask = ((1 << (52 - 12)) - 1) << 12;
        (par & mask) >> 8
    }

    use aarch64_cpu::registers::PAR_EL1;

    let par = PAR_EL1.get();
    arm_at!("s1e1r", far);
    let tmp = PAR_EL1.get();
    PAR_EL1.set(par);
    if (tmp & PAR_EL1::F::TranslationAborted.value) != 0 {
        Err(())
    } else {
        Ok(par_to_far(tmp) as usize)
    }
}

// addr be ipa
#[inline(always)]
pub fn exception_fault_addr() -> usize {
    let far = exception_far();
    let hpfar = if (exception_iss() & ESR_ELx_S1PTW) == 0 && exception_data_abort_is_permission_fault() {
        translate_far_to_hpfar(far).unwrap_or_else(|_| {
            error!("error happen in translate_far_to_hpfar");
            0
        })
    } else {
        exception_hpfar()
    };
    (far & 0xfff) | (hpfar << 8)
}

/// \return 1 means 32-bit instruction, 0 means 16-bit instruction
#[inline(always)]
fn exception_instruction_length() -> usize {
    ESR_EL2.read(ESR_EL2::IL) as usize
}

#[inline(always)]
pub fn exception_next_instruction_step() -> usize {
    2 + 2 * exception_instruction_length()
}

#[inline(always)]
pub fn exception_iss() -> usize {
    ESR_EL2.read(ESR_EL2::ISS) as usize
}

#[inline(always)]
pub fn exception_data_abort_emulatable() -> bool {
    // The emulated-device path needs a decodable access (ISV, bit 24) and a
    // valid fault address (FnV clear, bit 10); a fault on the guest's
    // stage-1 table walk (S1PTW) is a translation the guest asked for, not
    // a guest MMIO access.
    const ESR_ISS_DABT_ISV: usize = 1 << 24;
    (exception_iss() & (ESR_ISS_DABT_ISV | ESR_ISS_FNV | ESR_ELx_S1PTW)) == ESR_ISS_DABT_ISV
}

#[inline(always)]
pub fn exception_data_abort_is_translate_fault() -> bool {
    (exception_iss() & 0b111111 & (0xf << 2)) == 4
}

#[inline(always)]
pub fn exception_data_abort_is_permission_fault() -> bool {
    (exception_iss() & 0b111111 & (0xf << 2)) == 12
}

#[inline(always)]
pub fn exception_data_abort_access_width() -> usize {
    1 << ((exception_iss() >> 22) & 0b11)
}

#[inline(always)]
pub fn exception_data_abort_access_is_write() -> bool {
    (exception_iss() & (1 << 6)) != 0
}

#[inline(always)]
pub fn exception_data_abort_access_in_stage2() -> bool {
    (exception_iss() & (1 << 7)) != 0
}

#[inline(always)]
pub fn exception_data_abort_access_reg() -> usize {
    (exception_iss() >> 16) & 0b11111
}

#[inline(always)]
pub fn exception_data_abort_access_reg_width() -> usize {
    4 + 4 * ((exception_iss() >> 15) & 1)
}

#[inline(always)]
pub fn exception_data_abort_access_is_sign_ext() -> bool {
    ((exception_iss() >> 21) & 1) != 0
}

// SPSR (SPSR_EL2) mode fields of the trapped guest context.
const SPSR_M_MASK: u64 = 0b11111;
const SPSR_M_EL0T: u64 = 0b0000;
const SPSR_M_AARCH32: u64 = 0b1_0000;

// Guest EL1 controls read on exception entry: SCTLR_EL1.SPAN (bit 23) and
// DSSBS (bit 44), plus the ID_AA64PFR1_EL1 feature fields SSBS ([7:4]) and
// MTE ([11:8]).
const SCTLR_SPAN: u64 = 1 << 23;
const SCTLR_DSSBS: u64 = 1 << 44;
const ID_AA64PFR1_EL1_SSBS: u64 = 0xF << 4;
const ID_AA64PFR1_EL1_MTE: u64 = 0xF << 8;

// ESR_EL1 exception classes of an injected exception, taken at the guest's
// current exception level and from a lower one.
const ESR_EC_IABT_LOW: u64 = 0x20;
const ESR_EC_IABT_CUR: u64 = 0x21;
const ESR_EC_DABT_LOW: u64 = 0x24;
const ESR_EC_DABT_CUR: u64 = 0x25;

// ISS bits of a data or instruction abort: FAR_EL2 is not valid (bit 10),
// the trapped access is a write (bit 6) or a cache-maintenance access
// (bit 8).
const ESR_ISS_FNV: usize = 1 << 10;
const ESR_ISS_WNR: usize = 1 << 6;
const ESR_ISS_CM: usize = 1 << 8;

#[inline(always)]
fn exception_fault_addr_valid() -> bool {
    (exception_iss() & ESR_ISS_FNV) == 0
}

#[inline(always)]
fn exception_iss_bit(bit: usize) -> bool {
    (exception_iss() & bit) != 0
}

/// The faulting guest VA in FAR_EL2, or `None` when the hardware reported
/// no valid address (ISS.FnV). FAR_EL2 holds the guest VA whenever it is
/// valid, including for a fault on the guest's stage-1 walk, where
/// HPFAR_EL2 names the page being walked, not the data address.
#[inline]
pub fn exception_valid_far() -> Option<usize> {
    if exception_fault_addr_valid() {
        Some(exception_far())
    } else {
        None
    }
}

/// PSTATE after exception entry to EL1 for the trapped context, from the
/// live guest controls: SCTLR_EL1 (SPAN, DSSBS) and ID_AA64PFR1_EL1 (SSBS,
/// MTE feature fields, read at EL2).
fn exception_entry_pstate(source_spsr: u64) -> u64 {
    let sctlr = SCTLR_EL1.get();
    let pfr1 = ID_AA64PFR1_EL1.get();
    exception_entry_spsr(
        source_spsr,
        sctlr & SCTLR_SPAN == 0,
        sctlr & SCTLR_DSSBS != 0,
        pfr1 & ID_AA64PFR1_EL1_SSBS != 0,
        pfr1 & ID_AA64PFR1_EL1_MTE != 0,
    )
}

/// Emulate exception entry to EL1 so the active guest takes a synchronous
/// exception at its own vector table: the guest terminates the faulting task
/// or dies from its own fault, and the hypervisor and the other VMs keep
/// running. `esr_cur`/`esr_lower` is the ESR_EL1 for a fault taken at EL1 or
/// from EL0, `far_el1` the address for FAR_EL1 when valid.
fn inject_el1_sync_exception(esr_cur: u64, esr_lower: u64, far_el1: Option<usize>) {
    let ctx = unsafe { current_cpu().current_ctx().as_mut() }.unwrap();
    let source_spsr = ctx.spsr();
    let source_mode = source_spsr & SPSR_M_MASK;
    let target = abort_target(source_mode);
    let from_el0 = source_mode == SPSR_M_EL0T || source_mode & SPSR_M_AARCH32 != 0;

    ELR_EL1.set(ctx.exception_pc() as u64);
    SPSR_EL1.set(source_spsr);
    ESR_EL1.set(if from_el0 { esr_lower } else { esr_cur });
    if let Some(far) = far_el1 {
        FAR_EL1.set(far as u64);
    }
    ctx.set_spsr(exception_entry_pstate(source_spsr));
    ctx.set_exception_pc(VBAR_EL1.get() as usize + target.vector_offset as usize);
}

/// Inject the current data abort into the active guest with fault status
/// `fsc` (ISS bits [5:0]). Whenever the hardware reports a valid FAR (FnV
/// clear) the guest sees its own faulting virtual address; an invalid FAR
/// leaves FAR_EL1 untouched and reports FnV instead. A fault on the guest's
/// stage-1 table walk is deliberately re-presented as an abort on the data
/// access with the non-walk fault status: the guest's stage-1 walk level is
/// not observable from the trap and inventing one would be wrong (this is a
/// virtualization choice, not fidelity to the original exception).
pub fn exception_inject_data_abort(fsc: u64) {
    let far = exception_valid_far();
    let far_valid = far.is_some();
    let wnr = exception_iss_bit(ESR_ISS_WNR);
    let cm = exception_iss_bit(ESR_ISS_CM);
    inject_el1_sync_exception(
        abort_esr(ESR_EC_DABT_CUR, fsc, far_valid, wnr, cm),
        abort_esr(ESR_EC_DABT_LOW, fsc, far_valid, wnr, cm),
        far,
    );
}

/// Inject the current instruction abort into the active guest as a
/// synchronous external abort on the fetched guest VA.
pub fn exception_inject_instruction_abort() {
    let far = exception_valid_far();
    let far_valid = far.is_some();
    inject_el1_sync_exception(
        abort_esr(ESR_EC_IABT_CUR, FSC_EXTABT, far_valid, false, false),
        abort_esr(ESR_EC_IABT_LOW, FSC_EXTABT, far_valid, false, false),
        far,
    );
}

/// Inject an undefined-instruction exception into the active guest.
pub fn exception_inject_unknown() {
    inject_el1_sync_exception(unknown_esr(), unknown_esr(), None);
}

#[c_interface]
pub fn current_el_sp0_synchronous(ctx: *mut ContextFrame) {
    current_cpu().set_ctx(ctx);
    panic!("current_el_sp0_synchronous");
}

#[c_interface]
pub fn current_el_sp0_irq(ctx: *mut ContextFrame) {
    current_cpu().set_ctx(ctx);
    // lower_aarch64_irq(ctx);
    panic!("current_el_sp0_irq");
}

#[c_interface]
pub fn current_el_sp0_serror(ctx: *mut ContextFrame) {
    current_cpu().set_ctx(ctx);
    panic!("current_el0_serror");
}

#[c_interface]
pub fn current_el_spx_synchronous(ctx: *mut ContextFrame) {
    current_cpu().set_ctx(ctx);
    panic!(
        "current_elx_synchronous elr_el2 {:016x} sp_el0 {:016x} sp_el1 {:016x} sp_sel {}",
        aarch64_cpu::registers::ELR_EL2.get(),
        aarch64_cpu::registers::SP_EL0.get(),
        aarch64_cpu::registers::SP_EL1.get(),
        aarch64_cpu::registers::SPSel.get(),
    );
}

#[c_interface]
pub fn current_el_spx_irq(ctx: *mut ContextFrame) {
    trace!(">>> core {} current_el_spx_irq", current_cpu().id);
    lower_aarch64_irq(ctx);
}

#[c_interface]
pub fn current_el_spx_serror(ctx: *mut ContextFrame) {
    current_cpu().set_ctx(ctx);
    panic!("current_elx_serror");
}

#[c_interface]
pub fn lower_aarch64_synchronous(ctx: *mut ContextFrame) {
    trace!("lower_aarch64_synchronous");
    let prev_ctx = current_cpu().set_ctx(ctx);
    let esr = ESR_EL2.extract();
    match esr.read_as_enum(ESR_EL2::EC) {
        Some(ESR_EL2::EC::Value::DataAbortLowerEL) => {
            trace!("Core[{}] data_abort_handler", current_cpu().id);
            data_abort_handler();
        }
        Some(ESR_EL2::EC::Value::SMC64) => {
            smc_handler();
        }
        Some(ESR_EL2::EC::Value::HVC64) => {
            hvc_handler();
        }
        Some(ESR_EL2::EC::Value::TrappedMsrMrs) => sysreg_handler(exception_iss() as u32),
        Some(ESR_EL2::EC::Value::TrappedFP) => super::fpsimd::handle_trap(),
        Some(ESR_EL2::EC::Value::TrappedSve) => {
            // SVE register state is not switched, so the instruction must
            // not be retried: EL1 receives an undefined-instruction
            // exception through the common injection entry.
            exception_inject_unknown();
        }
        Some(ESR_EL2::EC::Value::InstrAbortLowerEL) => {
            // Executing from an IPA that has no stage-2 mapping and no
            // emulated device: the guest's own doing, deflect it back. The
            // log reports the raw syndrome and PC, and FAR_EL2 when the
            // hardware reported a valid address — HPFAR_EL2 is not
            // guaranteed valid for every instruction abort.
            error!(
                "core {} vm {}: instruction abort esr {:#x}, @pc {:#x}",
                current_cpu().id,
                active_vm().unwrap().id(),
                exception_esr(),
                current_cpu().exception_pc()
            );
            if let Some(far) = exception_valid_far() {
                error!("core {} instruction abort far {:#x}", current_cpu().id, far);
            }
            exception_inject_instruction_abort();
        }
        #[cfg(feature = "trap-wfi")]
        Some(ESR_EL2::EC::Value::TrappedWFIorWFE) => super::sync::wfi_wfe_handler(exception_iss() as u32),
        _ => {
            // No handler for this exception class, and no syndrome that
            // could be interpreted here. Report the exception to the guest
            // as an undefined instruction instead of killing the whole
            // machine.
            error!(
                "core {} vm {}: handler not presents for EC_{}, @pc {:#x}",
                current_cpu().id,
                active_vm().unwrap().id(),
                esr.read(ESR_EL2::EC),
                current_cpu().exception_pc()
            );
            exception_inject_unknown();
        }
    }
    current_cpu().set_ctx(prev_ctx);
}

#[cfg(feature = "preempt")]
fn interrupt_enter() {
    use super::cpu::{cpu_interrupt_disable, cpu_interrupt_enable};
    let level = cpu_interrupt_disable();
    // current_cpu().interrupt_nested += 1;
    cpu_interrupt_enable(level);
    // if current_cpu().interrupt_nested > 1 {
    //     trace!(
    //         "irq has come, core {} interrupt_nested {}",
    //         current_cpu().id,
    //         current_cpu().interrupt_nested,
    //     );
    // }
}

#[cfg(feature = "preempt")]
fn interrupt_leave() {
    use super::cpu::{cpu_interrupt_disable, cpu_interrupt_enable};
    // if current_cpu().interrupt_nested > 1 {
    //     trace!(
    //         "irq is going to leave, core {} interrupt_nested {}",
    //         current_cpu().id,
    //         current_cpu().interrupt_nested,
    //     );
    // }
    let level = cpu_interrupt_disable();
    // current_cpu().interrupt_nested -= 1;
    cpu_interrupt_enable(level);
}

// #[derive(Clone, PartialEq, Eq)]
// struct PendingIrq {
//     int_id: usize,
//     priority: usize,
//     sender: usize,
// }

// impl PendingIrq {
//     fn new(int_id: usize, priority: usize, sender: usize) -> Self {
//         Self {
//             int_id,
//             priority,
//             sender,
//         }
//     }
// }

// impl PartialOrd for PendingIrq {
//     fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
//         Some(self.cmp(other))
//     }
// }

// impl Ord for PendingIrq {
//     fn cmp(&self, other: &Self) -> core::cmp::Ordering {
//         match self.priority.cmp(&other.priority) {
//             core::cmp::Ordering::Equal => {}
//             ord => return ord,
//         }
//         match self.int_id.cmp(&other.int_id) {
//             core::cmp::Ordering::Equal => {}
//             ord => return ord,
//         }
//         self.sender.cmp(&other.sender)
//     }
// }

// // TODO: currently, this is useless
// static PENDING_IRQ_LIST: Lazy<Mutex<BinaryHeap<PendingIrq>>> = Lazy::new(|| Mutex::new(BinaryHeap::new()));

#[c_interface]
pub fn lower_aarch64_irq(ctx: *mut ContextFrame) {
    let prev_ctx = current_cpu().set_ctx(ctx);
    if let Some((int_id, _sender)) = IntCtrl::fetch() {
        #[cfg(feature = "preempt")]
        interrupt_enter();
        // let priority = IntCtrl::irq_priority(int_id);

        // PENDING_IRQ_LIST.lock().push(PendingIrq::new(int_id, priority, sender));
        let handled_by_hypervisor = interrupt_handler(int_id);
        // PENDING_IRQ_LIST.lock().pop();

        #[cfg(feature = "preempt")]
        interrupt_leave();
        interrupt_arch_deactive_irq(handled_by_hypervisor);
    }
    current_cpu().set_ctx(prev_ctx);
}

#[c_interface]
pub fn lower_aarch64_serror(ctx: *mut ContextFrame) {
    current_cpu().set_ctx(ctx);
    panic!("lower aarch64 serror");
}
