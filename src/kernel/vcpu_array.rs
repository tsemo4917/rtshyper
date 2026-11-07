use crate::{
    arch::ArchTrait,
    kernel::{current_cpu, CpuState, Vcpu, CONFIG_VM_NUM_MAX},
};
use alloc::{
    boxed::Box,
    slice::{Iter, IterMut},
};
use spin::Once;

use super::{sched::Scheduler, timer::timer_enable, VcpuState};

pub struct VcpuArray {
    array: [Option<Vcpu>; CONFIG_VM_NUM_MAX],
    pub(super) sched: Once<Box<dyn Scheduler<SchedItem = Vcpu>>>,
    len: usize,
    active: usize,
    timer_on: bool,
}

cfg_if::cfg_if! {
    if #[cfg(feature = "memory-reservation")] {
        const ENABLE_TIMER_ACTIVE_NUM: usize = 1;
    } else {
        const ENABLE_TIMER_ACTIVE_NUM: usize = 2;
    }
}

impl VcpuArray {
    pub const fn new() -> Self {
        Self {
            array: [const { None }; CONFIG_VM_NUM_MAX],
            sched: Once::new(),
            len: 0,
            active: 0,
            timer_on: false,
        }
    }

    #[inline]
    pub fn pop_vcpu_through_vmid(&self, vm_id: usize) -> Option<&Vcpu> {
        match self.array.get(vm_id) {
            Some(vcpu) => vcpu.as_ref(),
            None => None,
        }
    }

    #[inline]
    pub(super) fn vcpu_num(&self) -> usize {
        self.len
    }

    pub fn append_vcpu(&mut self, vcpu: Vcpu) {
        // There is only 1 VCPU from a VM in a PCPU
        let vm_id = vcpu.vm_id();
        match self.array.get_mut(vm_id) {
            Some(x) => match x {
                Some(_) => error!("self.array[{vm_id}].is_some()"),
                None => {
                    debug_assert_eq!(current_cpu().id, vcpu.phys_id());
                    debug!(
                        "append_vcpu: append VM[{}] vcpu {} on core {}",
                        vm_id,
                        vcpu.id(),
                        current_cpu().id
                    );
                    *x = Some(vcpu);
                    self.len += 1;
                }
            },
            None => error!("vm_id > self.array.len()"),
        }
    }

    pub fn wakeup_vcpu(&mut self, vcpu: &Vcpu) {
        if let Some(vcpu) = self
            .array
            .iter()
            .flatten()
            .find(|&array_vcpu| array_vcpu == vcpu)
            .cloned()
        {
            trace!(
                "core {} VM {} vcpu {} wakeup",
                current_cpu().id,
                vcpu.vm_id(),
                vcpu.id()
            );
            #[cfg(feature = "memory-reservation")]
            {
                if vcpu.state() == VcpuState::Inv {
                    if let Some(event) = vcpu.pmu_event() {
                        debug!("VM {} vcpu {} register pmu event", vcpu.vm_id(), vcpu.id());
                        let period = vcpu.bw_info().period();
                        super::timer::start_timer_event(period, event);
                    }
                }
            }
            current_cpu().cpu_state = CpuState::Run;
            // set vcpu state
            vcpu.set_state(VcpuState::Runnable);
            // determine the timer
            self.active += 1;
            if !self.timer_on && self.active >= ENABLE_TIMER_ACTIVE_NUM {
                self.timer_on = true;
                timer_enable(true);
            }
            // do scheduling
            self.scheduler().put(vcpu);
            if current_cpu().active_vcpu.is_none() {
                self.resched();
            }
        }
    }

    fn scheduler(&mut self) -> &mut dyn Scheduler<SchedItem = Vcpu> {
        match self.sched.get_mut() {
            Some(scheduler) => scheduler.as_mut(),
            None => panic!("scheduler is None"),
        }
    }

    pub fn remove_vcpu(&mut self, vm_id: usize) -> Option<Vcpu> {
        match self.array.get_mut(vm_id) {
            Some(x) => x.take().map(|vcpu| {
                self.len -= 1;
                if vcpu.state() != VcpuState::Inv {
                    self.active -= 1;
                    assert_ne!(self.active, usize::MAX);
                }
                vcpu.set_state(VcpuState::Inv);
                if self.timer_on && self.active < ENABLE_TIMER_ACTIVE_NUM {
                    self.timer_on = false;
                    timer_enable(false);
                }
                #[cfg(feature = "memory-reservation")]
                {
                    if let Some(vcpu_event) = vcpu.pmu_event() {
                        use super::timer::remove_timer_event;
                        use crate::arch::PmuTimerEvent;
                        remove_timer_event(|event| {
                            use alloc::sync::Arc;
                            if let Some(event) = event.as_any().downcast_ref::<PmuTimerEvent>() {
                                core::ptr::addr_of!(*event) == Arc::as_ptr(&vcpu_event)
                            } else {
                                false
                            }
                        });
                    }
                }
                // remove vcpu from scheduler
                self.scheduler().remove(&vcpu);
                // Before clearing active_vcpu: a removed owner must not keep the Vcpu Arc alive.
                crate::arch::on_vcpu_remove(&vcpu);
                if current_cpu().active_vcpu.as_ref() == Some(&vcpu) {
                    current_cpu().set_active_vcpu(None);
                    self.resched();
                }
                vcpu
            }),
            None => None,
        }
    }

    pub fn resched(&mut self) {
        if let Some(next_vcpu) = self.scheduler().next() {
            self.switch_to(next_vcpu);
        } else if current_cpu().active_vcpu.is_none() {
            super::run_idle_thread();
        }
    }

    fn switch_to(&mut self, next_vcpu: Vcpu) {
        if let Some(prev_vcpu) = current_cpu().active_vcpu.clone() {
            if prev_vcpu.ne(&next_vcpu) {
                trace!(
                    "next vm {} vcpu {}, prev vm {} vcpu {}",
                    next_vcpu.vm_id(),
                    next_vcpu.id(),
                    prev_vcpu.vm_id(),
                    prev_vcpu.id()
                );
                prev_vcpu.context_vm_store();
                prev_vcpu.set_state(VcpuState::Runnable);
                // put the prev_vcpu to scheduler
                self.scheduler().put(prev_vcpu);
            } else {
                return;
            }
        }
        // NOTE: Must set active first and then restore context!!!
        //      because context restore while inject pending interrupt for VM
        //      and will judge if current active vcpu
        next_vcpu.set_state(VcpuState::Running);
        current_cpu().set_active_vcpu(Some(next_vcpu.clone()));
        next_vcpu.context_vm_restore();
        crate::arch::Arch::install_vm_page_table(next_vcpu.vm_pt_dir(), next_vcpu.vm_id());
    }

    #[allow(dead_code)]
    pub fn block_current(&mut self) {
        if let Some(vcpu) = current_cpu().active_vcpu.take() {
            trace!("core {} VM {} vcpu {} block", current_cpu().id, vcpu.vm_id(), vcpu.id());
            vcpu.context_vm_store();
            vcpu.set_state(VcpuState::Blocked);
            self.scheduler().remove(&vcpu);
            self.resched();
        }
    }

    pub fn iter(&self) -> Iter<'_, Option<Vcpu>> {
        self.array.iter()
    }

    #[allow(dead_code)]
    pub fn iter_mut(&mut self) -> IterMut<'_, Option<Vcpu>> {
        self.array.iter_mut()
    }
}
