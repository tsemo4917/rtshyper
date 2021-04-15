use crate::config::VmCpuConfig;
use crate::config::DEF_VM_CONFIG_TABLE;
use crate::kernel::VM_LIST;
use crate::kernel::{
    cpu_assigned, cpu_id, cpu_vcpu_pool_size, set_active_vcpu, set_cpu_assign, CPU,
};
use crate::kernel::{vcpu_pool_append, vcpu_pool_init};
use crate::kernel::{Vm, VmInner};
use crate::lib::barrier;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use crate::board::PLATFORM_VCPU_NUM_MAX;
use crate::kernel::Vcpu;
fn vmm_init_cpu(config: &VmCpuConfig, vm_arc: &Vm) -> bool {
    let vm_lock = vm_arc.inner();

    for i in 0..config.num {
        use crate::kernel::vcpu_alloc;
        if let Some(vcpu_arc_mutex) = vcpu_alloc() {
            let mut vm = vm_lock.lock();
            let mut vcpu = vcpu_arc_mutex.lock();
            vm.vcpu_list.push(vcpu_arc_mutex.clone());
            drop(vm);
            crate::kernel::vcpu_init(vm_arc, &mut *vcpu, i);
        } else {
            println!("failed to allocte vcpu");
            return false;
        }
    }

    // remain to be init when assigning vcpu
    let mut vm = vm_lock.lock();
    vm.cpu_num = 0;
    vm.ncpu = 0;
    println!(
        "VM {} init cpu: cores=<{}>, allocat_bits=<0b{:b}>",
        vm.id, config.num, config.allocate_bitmap
    );

    true
}

struct VmAssignment {
    has_master: bool,
    cpu_num: usize,
    cpus: usize,
}

impl VmAssignment {
    fn default() -> VmAssignment {
        VmAssignment {
            has_master: false,
            cpu_num: 0,
            cpus: 0,
        }
    }
}

static VM_ASSIGN: Mutex<Vec<Mutex<VmAssignment>>> = Mutex::new(Vec::new());

use crate::kernel::VM_IF_LIST;
fn vmm_assign_vcpu() {
    vcpu_pool_init();

    let cpu_id = cpu_id();
    set_cpu_assign(false);
    let def_vm_config = DEF_VM_CONFIG_TABLE.lock();
    let vm_num = def_vm_config.vm_num;
    drop(def_vm_config);

    if (cpu_id == 0) {
        let mut vm_assign_list = VM_ASSIGN.lock();
        for i in 0..vm_num {
            vm_assign_list.push(Mutex::new(VmAssignment::default()));
        }
    }
    barrier();

    for i in 0..vm_num {
        let vm_list = VM_LIST.lock();
        let vm = vm_list[i].clone();

        drop(vm_list);
        let vm_inner_lock = vm.inner();
        let vm_inner = vm_inner_lock.lock();
        let vm_id = vm_inner.id;

        let config = vm_inner.config.as_ref().unwrap();

        if (config.cpu.allocate_bitmap & (1 << cpu_id)) != 0 {
            let vm_assign_list = VM_ASSIGN.lock();
            let mut vm_assigned = vm_assign_list[i].lock();
            let cfg_master = config.cpu.master as usize;
            let cfg_cpu_num = config.cpu.num;

            if cpu_id == cfg_master
                || (!vm_assigned.has_master && vm_assigned.cpu_num == cfg_cpu_num - 1)
            {
                let vcpu = vm_inner.vcpu_list[0].clone();
                let vcpu_inner = vcpu.lock();
                let vcpu_id = vcpu_inner.id;
                drop(vcpu_inner);

                if !vcpu_pool_append(vcpu) {
                    panic!("core {} too many vcpu", cpu_id);
                }
                // TODO: vm_if_list.master_vcpu_id
                let mut vm_if = VM_IF_LIST[i].lock();
                vm_if.master_vcpu_id = cpu_id;

                vm_assigned.has_master = true;
                vm_assigned.cpu_num += 1;
                vm_assigned.cpus |= 1 << cpu_id;
                set_cpu_assign(true);
                println!(
                    "* Core {} is assigned => vm {}, vcpu {}",
                    cpu_id, vm_id, vcpu_id
                );
                // The remain core become secondary vcpu
            } else if vm_assigned.cpu_num < cfg_cpu_num {
                let mut trgt_id = cfg_cpu_num - vm_assigned.cpu_num - 1;
                if vm_assigned.has_master {
                    trgt_id += 1;
                }

                let vcpu = vm_inner.vcpu_list[trgt_id].clone();
                let vcpu_inner = vcpu.lock();
                let vcpu_id = vcpu_inner.id;
                drop(vcpu_inner);

                if !vcpu_pool_append(vcpu) {
                    panic!("core {} too many vcpu", cpu_id);
                }
                set_cpu_assign(true);
                vm_assigned.cpu_num += 1;
                vm_assigned.cpus |= 1 << cpu_id;
                println!(
                    "* Core {} is assigned => vm {}, vcpu {}",
                    cpu_id, vm_id, vcpu_id
                );
            }
        }
    }
    barrier();

    if cpu_assigned() {
        if let Some(vcpu_pool) = unsafe { &mut CPU.vcpu_pool } {
            for i in 0..vcpu_pool.content.len() {
                let vcpu_arc = vcpu_pool.content[i].vcpu.clone();
                let mut vcpu = vcpu_arc.lock();
                vcpu.phys_id = cpu_id;
                let vm_id = vcpu.vm_id();

                let vm_assign_list = VM_ASSIGN.lock();
                let mut vm_assigned = vm_assign_list[vm_id].lock();
                let vm_list = VM_LIST.lock();
                let vm = vm_list[vm_id].clone();
                drop(vm_list);
                vm.set_ncpu(vm_assigned.cpus);
                vm.set_cpu_num(vm_assigned.cpu_num);
            }
        }
        let size = cpu_vcpu_pool_size();
        set_active_vcpu(size - 1);
    }
    barrier();
}

pub fn vmm_init() {
    barrier();

    if cpu_id() == 0 {
        super::vmm_init_config();

        use crate::config::{VmConfigTable, DEF_VM_CONFIG_TABLE};
        let vm_cfg_table = DEF_VM_CONFIG_TABLE.lock();
        let vm_num = vm_cfg_table.vm_num;

        for i in 0..vm_num {
            let mut vm_list = VM_LIST.lock();
            let vm = Vm::new(i);
            vm_list.push(vm);

            let vm_arc = vm_list[i].inner();
            let mut vm = vm_arc.lock();

            vm.config = Some(vm_cfg_table.entries[i].clone());
            drop(vm);

            vmm_init_cpu(&vm_cfg_table.entries[i].cpu, &vm_list[i]);
        }
        drop(vm_cfg_table);
    }

    barrier();

    // TODO vmm_assign_vcpu
    vmm_assign_vcpu();

    if cpu_id() == 0 {
        println!("Sybilla Hypervisor init ok\n\nStart booting VMs ...");
    }

    barrier();
}
