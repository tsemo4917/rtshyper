pub trait ContextFrameTrait {
    fn exception_pc(&self) -> usize;
    fn set_exception_pc(&mut self, pc: usize);
    fn set_argument(&mut self, arg: usize);
    fn set_gpr(&mut self, index: usize, val: usize);
    fn gpr(&self, index: usize) -> usize;
}

pub trait InterruptContextTriat: Default {
    fn save_state(&mut self);
    fn restore_state(&self);
}

pub trait ArchPageTableEntryTrait {
    fn from_pte(value: usize) -> Self;
    fn from_pa(pa: usize) -> Self;
    fn to_pte(&self) -> usize;
    fn to_pa(&self) -> usize;
    fn valid(&self) -> bool;
    fn entry(&self, index: usize) -> Self;
    fn set_entry(&self, index: usize, value: Self);
    fn make_table(frame_pa: usize) -> Self;
}

pub trait ArchTrait: TlbInvalidate + CacheInvalidate {
    fn exception_init();
    fn wait_for_interrupt();
    fn nop();
    fn fault_address() -> usize;
    fn install_vm_page_table(base: usize, vmid: usize);
    fn install_self_page_table(base: usize);
    fn disable_prefetch();
    fn mem_translate(va: usize) -> Option<usize>;
    fn current_stack_pointer() -> usize;
}

pub trait TlbInvalidate {
    fn invalid_hypervisor_va(va: usize);
    fn invalid_hypervisor_all();
    fn invalid_guest_ipa(ipa: usize);
    fn invalid_guest_all();
    // Same as `invalid_guest_all`, but for the VM identified by the stage-2
    // page table `base` and `vmid`, regardless of the VM current running.
    fn invalid_guest_all_by_vmid(base: usize, vmid: usize);
}

pub trait CacheInvalidate {
    fn dcache_flush(va: usize, len: usize);
    fn dcache_clean_flush(va: usize, len: usize);
}

pub trait Address {
    fn pa2hva(self) -> usize;
}

pub trait InterruptController {
    const NUM_MAX: usize;
    const IRQ_IPI: usize;
    const IRQ_HYPERVISOR_TIMER: usize;
    const IRQ_GUEST_TIMER: usize;

    fn init();
    fn enable(int_id: usize, en: bool);
    fn fetch() -> Option<(usize, usize)>;
    fn finish(int_id: usize);
    fn irq_priority(int_id: usize) -> usize;
}
