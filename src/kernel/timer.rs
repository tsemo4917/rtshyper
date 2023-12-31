use alloc::sync::Arc;

use crate::arch::INTERRUPT_IRQ_HYPERVISOR_TIMER;
use crate::kernel::current_cpu;
use crate::util::timer_list::{TimerEvent, TimerValue};

pub fn timer_init() {
    crate::arch::timer::timer_arch_init();
    timer_enable(false);

    crate::util::barrier();
    if current_cpu().id == 0 {
        crate::kernel::interrupt_reserve_int(INTERRUPT_IRQ_HYPERVISOR_TIMER, timer_irq_handler);
        info!("Timer frequency: {}Hz", crate::arch::timer::timer_arch_get_frequency());
        info!("Timer init ok");
    }
}

pub fn timer_enable(val: bool) {
    debug!("Core {} EL2 timer {}", current_cpu().id, val);
    super::interrupt::interrupt_cpu_enable(INTERRUPT_IRQ_HYPERVISOR_TIMER, val);
}

pub fn now() -> core::time::Duration {
    core::time::Duration::from_nanos(crate::arch::timer::gettime_ns() as u64)
}

#[allow(dead_code)]
pub fn get_counter() -> usize {
    crate::arch::timer::timer_arch_get_counter()
}

fn timer_notify_after(ms: usize) {
    use crate::arch::timer::{timer_arch_enable_irq, timer_arch_set};
    if ms == 0 {
        return;
    }

    timer_arch_set(ms);
    timer_arch_enable_irq();
}

fn check_timer_event(current_time: TimerValue) {
    while let Some((_timeout, event)) = current_cpu().timer_list.pop(current_time) {
        event.callback(current_time);
    }
}

pub fn timer_irq_handler() {
    use crate::arch::timer::timer_arch_disable_irq;

    timer_arch_disable_irq();

    check_timer_event(now());

    current_cpu().vcpu_array.resched();

    timer_notify_after(10);
}

#[allow(dead_code)]
pub fn start_timer_event(period: TimerValue, event: Arc<dyn TimerEvent>) {
    let timeout = now() + period;
    current_cpu().timer_list.push(timeout, event);
}

#[allow(dead_code)]
pub fn remove_timer_event<F>(condition: F)
where
    F: Fn(&Arc<dyn TimerEvent>) -> bool,
{
    current_cpu().timer_list.remove_all(condition);
}
