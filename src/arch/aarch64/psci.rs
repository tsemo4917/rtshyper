use crate::arch::{gic_cpu_init, interrupt_arch_deactive_irq};
use crate::board::PlatOperation;
use crate::kernel::IpiMessage;
use crate::kernel::{active_vm, ipi_send_msg, IpiInnerMsg, IpiPowerMessage, IpiType, PowerEvent};
use crate::kernel::{current_cpu, ipi_intra_broadcast_msg, Vcpu, VcpuState, Vm};
use crate::vmm::vmm_reboot;

use super::smc::smc_call;
use smccc::psci::*;

#[cfg(feature = "tx2")]
const TEGRA_SIP_GET_ACTMON_CLK_COUNTERS: u32 = 0xC2FFFE02;

pub fn power_arch_vm_shutdown_secondary_cores(vm: &Vm) {
    let m = IpiPowerMessage {
        src: vm.id(),
        event: PowerEvent::Reset,
        entry: 0,
        context: 0,
    };

    if !ipi_intra_broadcast_msg(vm, IpiType::Power, IpiInnerMsg::Power(m)) {
        warn!("power_arch_vm_shutdown_secondary_cores: fail to ipi_intra_broadcast_msg");
    }
}

pub fn power_arch_cpu_on(mpidr: usize, entry: usize, ctx: usize) -> usize {
    info!("power on cpu {mpidr:x}");
    let ret = smc_call(PSCI_CPU_ON_64, mpidr, entry, ctx).0;
    if ret != smccc::error::SUCCESS as usize {
        error!("power on cpu {mpidr:x} failed");
    }
    ret
}

#[allow(dead_code)]
pub fn power_arch_cpu_shutdown() {
    gic_cpu_init();
    interrupt_arch_deactive_irq(true);
    current_cpu().vcpu_array.resched();
}

pub fn power_arch_sys_reset() {
    smc_call(PSCI_SYSTEM_RESET, 0, 0, 0);
}

pub fn power_arch_sys_shutdown() {
    smc_call(PSCI_SYSTEM_OFF, 0, 0, 0);
}

fn psci_guest_sys_reset() -> usize {
    vmm_reboot();
    0
}

fn psci_guest_sys_off() -> usize {
    let vm_id = active_vm().unwrap().id();
    if vm_id == 0 {
        crate::board::Platform::sys_shutdown();
    } else {
        info!("VM[{}] system off, please remove it on MVM", vm_id);
        // vmm_remove_vm(vm_id);
    }
    0
}

#[inline(never)]
pub fn smc_guest_handler(fid: usize, x1: usize, x2: usize, x3: usize) -> bool {
    debug!(
        "smc_guest_handler: fid {:#x}, x1 {:#x}, x2 {:#x}, x3 {:#x}",
        fid, x1, x2, x3
    );
    let r = match fid as u32 {
        PSCI_FEATURES => match x1 as u32 {
            PSCI_VERSION | PSCI_CPU_ON_64 | PSCI_FEATURES => smccc::error::SUCCESS as usize,
            _ => error::NOT_SUPPORTED as usize,
        },
        PSCI_VERSION => smc_call(PSCI_VERSION, 0, 0, 0).0,
        PSCI_CPU_ON_64 => psci_guest_cpu_on(x1, x2, x3),
        PSCI_SYSTEM_RESET => psci_guest_sys_reset(),
        PSCI_SYSTEM_OFF => psci_guest_sys_off(),
        PSCI_MIGRATE_INFO_TYPE => MigrateType::MigrationNotRequired as usize,
        PSCI_AFFINITY_INFO_64 => 0,
        #[cfg(feature = "tx2")]
        TEGRA_SIP_GET_ACTMON_CLK_COUNTERS => {
            let result = smc_call(TEGRA_SIP_GET_ACTMON_CLK_COUNTERS, x1, x2, x3);
            // println!("x1 {:#x}, x2 {:#x}, x3 {:#x}", x1, x2, x3);
            // println!(
            //     "result.0 {:#x}, result.1 {:#x}, result.2 {:#x}",
            //     result.0, result.1, result.2
            // );
            current_cpu().set_gpr(1, result.1);
            current_cpu().set_gpr(2, result.2);
            result.0
        }
        _ => {
            // unimplemented!();
            return false;
        }
    };

    current_cpu().set_gpr(0, r);

    true
}

fn psci_vcpu_on(vcpu: &Vcpu, entry: usize, ctx: usize) {
    // println!("psci vcpu on， entry {:x}, ctx {:x}", entry, ctx);
    if vcpu.phys_id() != current_cpu().id {
        panic!(
            "cannot psci on vcpu on cpu {} by cpu {}",
            vcpu.phys_id(),
            current_cpu().id
        );
    }
    vcpu.set_gpr(0, ctx);
    vcpu.set_exception_pc(entry);
    // Just wake up the vcpu
    current_cpu().vcpu_array.wakeup_vcpu(vcpu);
}

// Todo: need to support more vcpu in one Core
pub fn psci_ipi_handler(msg: IpiMessage) {
    match msg.ipi_message {
        IpiInnerMsg::Power(power_msg) => {
            let trgt_vcpu = match current_cpu().vcpu_array.pop_vcpu_through_vmid(power_msg.src) {
                None => {
                    warn!(
                        "Core {} failed to find target vcpu, source vmid {}",
                        current_cpu().id,
                        power_msg.src
                    );
                    return;
                }
                Some(vcpu) => vcpu,
            };
            match power_msg.event {
                PowerEvent::CpuOn => {
                    if trgt_vcpu.state() != VcpuState::Inv {
                        warn!(
                            "psci_ipi_handler: target VCPU {} in VM {} is already running",
                            trgt_vcpu.id(),
                            trgt_vcpu.vm().unwrap().id()
                        );
                        return;
                    }
                    info!(
                        "Core {} (vm {}, vcpu {}) is woke up",
                        current_cpu().id,
                        trgt_vcpu.vm().unwrap().id(),
                        trgt_vcpu.id()
                    );
                    psci_vcpu_on(trgt_vcpu, power_msg.entry, power_msg.context);
                }
                PowerEvent::CpuOff => {
                    // TODO: 为什么ipi cpu off是当前vcpu shutdown，而vcpu shutdown 最后是把平台的物理核心shutdown
                    // 没有用到。不用管
                    // current_cpu().active_vcpu.clone().unwrap().shutdown();
                    unimplemented!("PowerEvent::PsciIpiCpuOff")
                }
                PowerEvent::Reset => {
                    let vcpu = current_cpu().active_vcpu.as_ref().unwrap();
                    vcpu.init_boot_info(active_vm().unwrap().config());
                }
            }
        }
        _ => {
            error!("psci_ipi_handler: receive illegal psci ipi type");
        }
    }
}

fn psci_guest_cpu_on(mpidr: usize, entry: usize, ctx: usize) -> usize {
    let vcpu_id = mpidr & 0xff;
    let vm = active_vm().unwrap();

    if let Some(phys_id) = vm.vcpuid_to_pcpuid(vcpu_id) {
        #[cfg(feature = "tx2")]
        {
            let cluster = (mpidr >> 8) & 0xff;
            if vm.id() == 0 && cluster != 1 {
                warn!("psci_guest_cpu_on: L4T only support cluster #1");
                return error::NOT_PRESENT as usize;
            }
        }

        let m = IpiPowerMessage {
            src: vm.id(),
            event: PowerEvent::CpuOn,
            entry,
            context: ctx,
        };

        if !ipi_send_msg(phys_id, IpiType::Power, IpiInnerMsg::Power(m)) {
            warn!("psci_guest_cpu_on: fail to send msg");
            return error::NOT_PRESENT as usize;
        }

        0
    } else {
        warn!("psci_guest_cpu_on: VM {} target vcpu {} not exist", vm.id(), vcpu_id);
        error::NOT_PRESENT as usize
    }
}
