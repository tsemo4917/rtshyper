use tock_registers::interfaces::{ReadWriteable, Writeable};

use crate::arch::PAGE_SIZE;
use crate::board::{PlatOperation, Platform};
use crate::kernel::{cpu_map_self, CPU_STACK_OFFSET, CPU_STACK_SIZE};

#[repr(C, align(16))]
struct CoreBootStack([u8; PAGE_SIZE * 2]);

#[repr(transparent)]
struct BootStack<const NUM: usize>([CoreBootStack; NUM]);

impl<const NUM: usize> BootStack<NUM> {
    const fn new() -> Self {
        Self([const { CoreBootStack([0; PAGE_SIZE * 2]) }; NUM])
    }
}

#[link_section = ".bss.stack"]
static mut BOOT_STACK: BootStack<{ crate::board::static_config::CORE_NUM }> = BootStack::new();

extern "C" {
    fn _bss_begin();
    fn _bss_end();
    fn vectors();
}

#[unsafe(naked)]
#[no_mangle]
#[link_section = ".text.boot.entry"]
unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        r#"
        mov x20, x0 // save fdt pointer to x20

        // setup stack sp per core
        ldr x1, ={boot_stack}
        add sp, x1, #{CORE_BOOT_STACK_SIZE}

        mrs x0, mpidr_el1
        bl {mpidr2cpuid}
        mov x19, x0 // save core_id

        // disable cache and MMU
        mrs x1, sctlr_el2
        bic x1, x1, #0xf
        msr sctlr_el2, x1

        // cache_invalidate(0): clear dl1$
        mov x0, #0
        bl  {cache_invalidate}

        // if (core_id == 0) cache_invalidate(2): clear l2$
        mov x0, #2
        bl  {cache_invalidate}

        ic  iallu // clear icache

        // if core_id is not zero, skip bss clearing and pt_populate
        bl {clear_bss}
        adrp x0, {lvl1_page_table}
        adrp x1, {lvl2_page_table}
        bl  {pt_populate}

        // Trap nothing from EL1 to El2
        mov x3, xzr
        msr cptr_el2, x3

        adrp x0, {lvl1_page_table}
        bl  {mmu_init}

        mov x0, x19
        bl  {cpu_map_self}

        msr ttbr0_el2, x0

        bl {init_sysregs} // here, enable MMU and cache, then switch the stack

        msr spsel, #1
        ldr x1, ={CPU}
        add x1, x1, #{CPU_STACK_OFFSET}
        sub	sp, x1, #{CONTEXT_SIZE}

        mov x0, x19
        mov x1, x20
        bl  {init}
        "#,
        boot_stack = sym BOOT_STACK,
        CORE_BOOT_STACK_SIZE = const core::mem::size_of::<CoreBootStack>(),
        mpidr2cpuid = sym Platform::mpidr2cpuid,
        cache_invalidate = sym cache_invalidate,
        clear_bss = sym clear_bss,
        lvl1_page_table = sym super::mmu::LVL1_PAGE_TABLE,
        lvl2_page_table = sym super::mmu::LVL2_PAGE_TABLE,
        pt_populate = sym super::mmu::pt_populate,
        mmu_init = sym super::mmu::mmu_init,
        cpu_map_self = sym cpu_map_self,
        init_sysregs = sym init_sysregs,
        CPU = sym crate::kernel::CPU,
        CPU_STACK_OFFSET = const CPU_STACK_OFFSET + CPU_STACK_SIZE,
        CONTEXT_SIZE = const core::mem::size_of::<crate::arch::ContextFrame>(),
        init = sym crate::init,
    );
}

#[unsafe(naked)]
#[no_mangle]
#[link_section = ".text.boot"]
unsafe extern "C" fn _secondary_start() -> ! {
    core::arch::naked_asm!(
        r#"
        mov x19, x0 // save core id to x19

        // setup stack sp per core
        ldr x1, ={boot_stack}
        mov x2, #{CORE_BOOT_STACK_SIZE}
        mul x3, x0, x2
        add x1, x1, x2
        add sp, x1, x3

        // disable cache and MMU
        mrs x1, sctlr_el2
        bic x1, x1, #0xf
        msr sctlr_el2, x1

        // cache_invalidate(0): clear dl1$
        mov x0, #0
        bl  {cache_invalidate}

        ic  iallu

        // Trap nothing from EL1 to El2
        mov x3, xzr
        msr cptr_el2, x3

        adrp x0, {lvl1_page_table}
        bl  {mmu_init}

        mov x0, x19
        bl  {cpu_map_self}

        msr ttbr0_el2, x0

        bl {init_sysregs}

        msr spsel, #1
        ldr x1, ={CPU}
        add x1, x1, #{CPU_STACK_OFFSET}
        sub	sp, x1, #{CONTEXT_SIZE}

        mov x0, x19
        mov x1, x20
        bl  {init}
        "#,
        boot_stack = sym BOOT_STACK,
        CORE_BOOT_STACK_SIZE = const core::mem::size_of::<CoreBootStack>(),
        cache_invalidate = sym cache_invalidate,
        lvl1_page_table = sym super::mmu::LVL1_PAGE_TABLE,
        mmu_init = sym super::mmu::mmu_init,
        cpu_map_self = sym cpu_map_self,
        CPU = sym crate::kernel::CPU,
        CPU_STACK_OFFSET = const CPU_STACK_OFFSET + CPU_STACK_SIZE,
        CONTEXT_SIZE = const core::mem::size_of::<crate::arch::ContextFrame>(),
        init_sysregs = sym init_sysregs,
        init = sym crate::init,
    );
}

fn init_sysregs() {
    use aarch64_cpu::registers::{HCR_EL2, SCTLR_EL2, VBAR_EL2};
    HCR_EL2.write(
        HCR_EL2::VM::Enable
            + HCR_EL2::RW::EL1IsAarch64
            + HCR_EL2::IMO::EnableVirtualIRQ
            + HCR_EL2::FMO::EnableVirtualFIQ
            + HCR_EL2::TSC::EnableTrapEl1SmcToEl2,
    );
    VBAR_EL2.set(vectors as usize as u64); // clippy: casting a function pointer to usize/isize is portable
    SCTLR_EL2.modify(SCTLR_EL2::M::Enable + SCTLR_EL2::C::Cacheable + SCTLR_EL2::I::Cacheable);
    use crate::arch::traits::TlbInvalidate;
    crate::arch::Arch::invalid_hypervisor_all();
}

unsafe fn clear_bss() {
    core::slice::from_raw_parts_mut(_bss_begin as usize as *mut u8, _bss_end as usize - _bss_begin as usize).fill(0)
}

#[link_section = ".text.boot"]
unsafe fn cache_invalidate(cache_level: usize) {
    core::arch::asm!(
        r#"
        msr csselr_el1, {0}
        mrs x4, ccsidr_el1 // read cache size id.
        and x1, x4, #0x7
        add x1, x1, #0x4 // x1 = cache line size.
        ldr x3, =0x7fff
        and x2, x3, x4, lsr #13 // x2 = cache set number – 1.
        ldr x3, =0x3ff
        and x3, x3, x4, lsr #3 // x3 = cache associativity number – 1.
        clz w4, w3 // x4 = way position in the cisw instruction.
        mov x5, #0 // x5 = way counter way_loop.
    // way_loop:
    1:
        mov x6, #0 // x6 = set counter set_loop.
    // set_loop:
    2:
        lsl x7, x5, x4
        orr x7, {0}, x7 // set way.
        lsl x8, x6, x1
        orr x7, x7, x8 // set set.
        dc cisw, x7 // clean and invalidate cache line.
        add x6, x6, #1 // increment set counter.
        cmp x6, x2 // last set reached yet?
        ble 2b // if not, iterate set_loop,
        add x5, x5, #1 // else, next way.
        cmp x5, x3 // last way reached yet?
        ble 1b // if not, iterate way_loop
        "#,
        in(reg) cache_level,
        options(nostack)
    );
}
