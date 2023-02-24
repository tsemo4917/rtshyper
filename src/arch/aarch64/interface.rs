use crate::arch::ArchTrait;

pub const PAGE_SHIFT: usize = 12;
pub const PAGE_SIZE: usize = 1 << PAGE_SHIFT;
pub const ENTRY_PER_PAGE: usize = PAGE_SIZE / 8;

pub type ContextFrame = super::context_frame::Aarch64ContextFrame;

pub const WORD_SIZE: usize = core::mem::size_of::<usize>();
pub const PTE_PER_PAGE: usize = PAGE_SIZE / WORD_SIZE;

// The size offset of the memory region addressed by TTBR0_EL2
// see TCR_EL2::T0SZ
pub const HYP_VA_SIZE: u64 = 39;
// The size offset of the memory region addressed by VTTBR_EL2
// see VTCR_EL2::T0SZ
pub const VM_IPA_SIZE: u64 = 35;

pub type Arch = Aarch64Arch;

pub struct Aarch64Arch;

impl ArchTrait for Aarch64Arch {
    fn exception_init() {
        todo!()
    }

    fn invalidate_tlb() {
        todo!()
    }

    fn wait_for_interrupt() {
        cortex_a::asm::wfi();
    }

    fn nop() {
        cortex_a::asm::nop();
    }

    fn fault_address() -> usize {
        todo!()
    }

    fn install_vm_page_table(base: usize, vmid: usize) {
        // restore vm's Stage2 MMU context
        let vttbr = (vmid << 48) | base;
        // println!("vttbr {:#x}", vttbr);
        msr!(VTTBR_EL2, vttbr);
        unsafe {
            core::arch::asm!("isb");
        }
    }

    fn install_self_page_table(base: usize) {
        cortex_a::registers::TTBR0_EL2.set_baddr(base as u64);
    }
}
