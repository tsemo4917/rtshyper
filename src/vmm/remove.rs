use alloc::sync::Arc;

use crate::arch::{interrupt_arch_deactive_irq, Arch, TlbInvalidate, INTERRUPT_IRQ_GUEST_TIMER};
use crate::kernel::vm_if_reset;
use crate::kernel::{
    current_cpu, interrupt_cpu_enable, interrupt_vm_remove, ipi_send_msg, remove_vm, remove_vm_async_task, vm_by_id,
    IpiInnerMsg, IpiType, IpiVmmPercoreMsg, Vm,
};
use crate::vmm::address::vmm_unmap_ipa2hva;
use crate::vmm::VmmPercoreEvent;

pub fn vmm_remove_vm(vm_id: usize) {
    if vm_id == 0 {
        warn!("{} do not support remove vm0", env!("CARGO_PKG_NAME"));
        return;
    }

    // remove vm: page table / mmio / vgic will be removed when vm drop
    if let Some(vm) = vm_by_id(vm_id) {
        // vcpu
        vmm_remove_vcpu(&vm);
        // reset vm interface
        vm_if_reset(vm_id);
        // passthrough dev
        vmm_remove_passthrough_device(&vm);
        // clear async task list
        remove_vm_async_task(vm_id);
        crate::device::remove_virtio_nic(vm_id);
        // remove vm cfg
        let _ = crate::config::del_vm(vm_id);
        #[cfg(feature = "unilib")]
        // remove vm unilib
        crate::util::unilib::unilib_fs_remove(vm_id);
        // unmap ipa(hva) percore at last
        vmm_unmap_ipa2hva(vm.clone());
        // Invalidate the stage-2 TLB entries of the VM before its pages go
        // back to the color pool: generate_vm_id() recycles the VMID, and a
        // stale entry would let the next VM with this VMID translate to
        // memory that has been handed to someone else.
        Arch::invalid_guest_all_by_vmid(vm.pt_dir(), vm.id());
        remove_vm(vm_id);
        info!("remove vm[{}] successfully", vm_id);
    } else {
        error!("VM[{vm_id}] does not exist!");
    }
}

pub fn vmm_remove_vcpu_percore(vm: &Vm) {
    current_cpu().vcpu_array.remove_vcpu(vm.id());
    if !current_cpu().assigned() {
        // hard code: remove el1 timer interrupt 27
        interrupt_cpu_enable(INTERRUPT_IRQ_GUEST_TIMER, false);
        interrupt_arch_deactive_irq(true);
    }
}

fn vmm_remove_vcpu(vm: &Arc<Vm>) {
    for vcpu in vm.vcpu_list() {
        if vcpu.phys_id() == current_cpu().id {
            vmm_remove_vcpu_percore(vm);
        } else {
            let m = IpiVmmPercoreMsg {
                vm: vm.clone(),
                event: VmmPercoreEvent::RemoveCpu,
            };
            if !ipi_send_msg(vcpu.phys_id(), IpiType::Vmm, IpiInnerMsg::VmmPercoreMsg(m)) {
                warn!("vmm_remove_vcpu: failed to send ipi to Core {}", vcpu.phys_id());
            }
        }
    }
}

fn vmm_remove_passthrough_device(vm: &Vm) {
    for irq in vm.config().passthrough_device_irqs() {
        interrupt_vm_remove(vm, *irq);
        debug!("VM[{}] remove irq {}", vm.id(), irq);
    }
}
