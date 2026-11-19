use core::sync::atomic::{AtomicBool, Ordering};

use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::RwLock;

use crate::arch::{LVL1_SHIFT, PAGE_SIZE, PTE_S1_NORMAL, VM_IPA_SIZE};
use crate::board::PLAT_DESC;
use crate::kernel::{current_cpu, ipi_send_msg, IpiInnerMsg, IpiType, IpiVmmPercoreMsg, Vm};
use crate::util::{barrier, round_down};

use super::VmmPercoreEvent;

// Here, we regrad IPA as part of HVA (Hypervisor VA)
// using the higher bits as VMID to distinguish

// convert ipa to pa and mapping the hva(from ipa) on current cpu()
pub fn vmm_setup_ipa2hva(vm: Arc<Vm>) {
    let mut flag = false;
    for target_cpu_id in 0..PLAT_DESC.cpu_desc.num {
        if target_cpu_id != current_cpu().id {
            let msg = IpiVmmPercoreMsg {
                vm: vm.clone(),
                event: VmmPercoreEvent::MapIPA,
            };
            if !ipi_send_msg(target_cpu_id, IpiType::Vmm, IpiInnerMsg::VmmPercoreMsg(msg)) {
                error!("vmm_setup_ipa2hva: failed to send ipi to Core {}", target_cpu_id);
            }
        } else {
            flag = true;
        }
    }
    // execute after notify all other cores
    if flag {
        vmm_map_ipa_percore(&vm, true);
    }
    info!("vmm_setup_ipa2hva: VM[{}] is ok", vm.id());
}

pub fn vmm_unmap_ipa2hva(vm: Arc<Vm>) {
    vm.reset_mem_regions();
    let mut flag = false;
    for target_cpu_id in 0..PLAT_DESC.cpu_desc.num {
        if target_cpu_id != current_cpu().id {
            let msg = IpiVmmPercoreMsg {
                vm: vm.clone(),
                event: VmmPercoreEvent::UnmapIPA,
            };
            if !ipi_send_msg(target_cpu_id, IpiType::Vmm, IpiInnerMsg::VmmPercoreMsg(msg)) {
                error!("vmm_unmap_ipa2hva: failed to send ipi to Core {}", target_cpu_id);
            }
        } else {
            flag = true;
        }
    }
    // execute after notify all other cores
    if flag {
        vmm_unmap_ipa_percore(&vm);
    }
    info!("vmm_unmap_ipa2hva: VM[{}] is ok", vm.id());
}

fn vm_flush_ipa(vm: &Vm) {
    for region in vm.config().memory_region().iter() {
        let hva = vm.ipa2hva(region.ipa_start);
        use crate::arch::{Arch, CacheInvalidate};
        Arch::dcache_clean_flush(hva, region.length);
    }
}

pub fn vmm_map_ipa_percore(vm: &Vm, is_master: bool) {
    static SHARED_PTE: RwLock<Vec<(usize, usize)>> = RwLock::new(Vec::new());
    static FINISH: AtomicBool = AtomicBool::new(false);

    trace!("vmm_map_ipa_percore: on core {}, for VM[{}]", current_cpu().id, vm.id());
    let config = vm.config();
    if is_master {
        let mut shared_pte_list = SHARED_PTE.write();
        shared_pte_list.clear();
        for region in config.memory_region().iter() {
            for ipa in region.as_range().step_by(PAGE_SIZE) {
                let hva = vm.ipa2hva(ipa);
                let pa = vm.ipa2pa(ipa).unwrap();
                current_cpu()
                    .pt()
                    .pt_map_range(hva, PAGE_SIZE, pa, PTE_S1_NORMAL, false);
            }

            // share one L1 entry per 1GB block of the pages mapped above:
            // sample the base of each block (stepping from ipa_start can
            // leave the last crossed block without a sample point) up to the
            // last page the per-page loop maps, since allocation rounds the
            // length but not the start to whole pages. Complete blocks only
            // inside the VM's own IPA window: beyond it ipa2hva() returns 0
            // and a shared entry would overwrite VA 0 of every other core's
            // root, so there keep only the samples the original stepping
            // produced.
            let block_size = 1 << LVL1_SHIFT;
            if let Some(last) = region.as_range().step_by(PAGE_SIZE).next_back() {
                let last_in_window = last.min((1 << VM_IPA_SIZE) - 1);
                let mut share = |ipa: usize| {
                    let hva = vm.ipa2hva(ipa);
                    let pte = current_cpu().pt().get_pte(hva, 1).unwrap();
                    shared_pte_list.push((hva, pte));
                };
                for ipa in (round_down(region.ipa_start, block_size)..=last_in_window).step_by(block_size) {
                    share(ipa);
                }
                for ipa in region
                    .as_range()
                    .step_by(block_size)
                    .skip_while(|ipa| *ipa < (1 << VM_IPA_SIZE))
                {
                    share(ipa);
                }
            }
        }
        FINISH.store(true, Ordering::Relaxed);
    } else {
        while !FINISH.load(Ordering::Relaxed) {
            core::hint::spin_loop();
        }
        for &(hva, pte) in SHARED_PTE.read().iter() {
            current_cpu().pt().set_pte(hva, 1, pte);
        }
    }
    barrier();
    vm_flush_ipa(vm);
    if is_master {
        FINISH.store(false, Ordering::Relaxed);
    }
}

pub fn vmm_unmap_ipa_percore(vm: &Vm) {
    trace!(
        "vmm_unmap_ipa_percore: on core {}, for VM[{}]",
        current_cpu().id,
        vm.id()
    );
    let config = vm.config();
    for region in config.memory_region().iter() {
        let hva = vm.ipa2hva(region.ipa_start);
        current_cpu().pt().pt_unmap_range(hva, region.length, false);
    }
    barrier();
}
