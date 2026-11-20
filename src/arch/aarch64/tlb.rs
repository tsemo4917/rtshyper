use core::arch::asm;

use crate::arch::{ArchTrait, TlbInvalidate};

use super::Aarch64Arch;

impl TlbInvalidate for Aarch64Arch {
    fn invalid_hypervisor_va(va: usize) {
        unsafe {
            asm!(
                "dsb ish",
                "tlbi vae2is, {0}",
                "dsb ish",
                "isb",
                in(reg) va >> 12,
                options(nostack)
            );
        }
    }

    #[inline]
    fn invalid_hypervisor_all() {
        unsafe {
            asm!("dsb ish", "tlbi alle2is", "dsb ish", "isb", options(nostack));
        }
    }

    fn invalid_guest_ipa(ipa: usize) {
        unsafe {
            asm!(
                "dsb ish",
                "tlbi ipas2e1is, {0}",
                "dsb ish",
                "isb",
                in(reg) ipa >> 12,
                options(nostack)
            );
        }
    }

    #[inline]
    fn invalid_guest_all() {
        unsafe {
            asm!("dsb ish", "tlbi vmalls12e1is", "dsb ish", "isb", options(nostack));
        }
    }

    // A by-VMID invalidation applies to the VMID held in VTTBR_EL2 of the
    // executing core, so temporarily install the target VM's stage-2 context
    // and restore the previous one after the inner-shareable broadcast.
    fn invalid_guest_all_by_vmid(base: usize, vmid: usize) {
        let vttbr: u64;
        mrs!(vttbr, VTTBR_EL2);
        Self::install_vm_page_table(base, vmid);
        Self::invalid_guest_all();
        msr!(VTTBR_EL2, vttbr);
        isb!();
    }
}
