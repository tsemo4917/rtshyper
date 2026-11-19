use alloc::collections::LinkedList;
use alloc::sync::Arc;

use spin::Mutex;

use crate::arch::INTERRUPT_IRQ_IPI;
use crate::board::static_config;
use crate::board::PLAT_DESC;
use crate::device::{VirtioMmio, Virtq};
use crate::kernel::{current_cpu, interrupt_cpu_ipi_send};
use crate::kernel::{interrupt_reserve_int, interrupt_vm_inject};
use crate::vmm::{VmmEvent, VmmPercoreEvent};

use super::interrupt_cpu_enable;
use super::Vm;

#[derive(Copy, Clone, Debug)]
pub enum InitcEvent {
    GichEn,
    SetEn,
    SetAct,
    SetPend,
    SetPrio,
    SetTrgt,
    SetCfg,
    Route,
}

#[derive(Copy, Clone)]
pub enum PowerEvent {
    CpuOn,
    CpuOff,
    Reset,
}

#[derive(Clone)]
pub struct IpiInitcMessage {
    pub event: InitcEvent,
    pub vm_id: usize,
    pub int_id: u16,
    pub val: u8,
}

/*
* src: src vm id
*/
#[derive(Clone)]
pub struct IpiPowerMessage {
    pub src: usize,
    pub event: PowerEvent,
    pub entry: usize,
    pub context: usize,
}

// #[derive(Copy, Clone)]
// pub struct IpiEthernetAckMsg {
//     pub len: usize,
//     pub succeed: bool,
// }

#[derive(Clone)]
pub struct IpiEthernetMsg {
    pub trgt_nic: Arc<VirtioMmio>,
}

#[derive(Clone)]
pub struct IpiVmmMsg {
    pub vmid: usize,
    pub event: VmmEvent,
}

#[derive(Clone)]
pub struct IpiVmmPercoreMsg {
    pub vm: Arc<Vm>,
    pub event: VmmPercoreEvent,
}

// only support for mediated blk
#[derive(Clone)]
pub struct IpiMediatedMsg {
    pub src_vm: Arc<Vm>,
    pub vq: Arc<Virtq>,
    pub blk: Arc<VirtioMmio>,
}

#[derive(Clone)]
pub struct IpiMediatedNotifyMsg {
    pub vm_id: usize,
}

#[derive(Clone)]
pub struct IpiHvcMsg {
    pub src_vmid: usize,
    pub trgt_vmid: usize,
    pub fid: usize,
    pub event: usize,
}

#[derive(Clone)]
pub struct IpiIntInjectMsg {
    pub vm_id: usize,
    pub int_id: usize,
}

declare_enum_with_handler! {
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    #[repr(usize)]
    pub enum IpiType => static IPI_HANDLER_LIST: &[fn(IpiMessage)] {
        Intc => crate::arch::vgic_ipi_handler,
        Power => crate::arch::psci_ipi_handler,
        EthernetMsg => crate::device::ethernet_ipi_rev_handler,
        Hvc => crate::kernel::hvc_ipi_handler,
        Vmm => crate::vmm::vmm_ipi_handler,
        MediatedDev => crate::device::mediated_ipi_handler,
        IntInject => interrupt_inject_ipi_handler,
    }
}

#[derive(Clone)]
pub enum IpiInnerMsg {
    // IpiTIntc
    Initc(IpiInitcMessage),
    // IpiTPower
    Power(IpiPowerMessage),
    // IpiTEthernetMsg
    EnternetMsg(IpiEthernetMsg),
    // IpiTVMM
    VmmMsg(IpiVmmMsg),
    // IpiTVMM
    VmmPercoreMsg(IpiVmmPercoreMsg),
    // IpiTMediatedDev
    MediatedMsg(IpiMediatedMsg),
    // unused
    MediatedNotifyMsg(IpiMediatedNotifyMsg),
    // IpiTHvc
    HvcMsg(IpiHvcMsg),
    // IpiTIntInject
    IntInjectMsg(IpiIntInjectMsg),
}

pub struct IpiMessage {
    pub ipi_type: IpiType,
    pub ipi_message: IpiInnerMsg,
}

struct CpuIf {
    msg_queue: LinkedList<IpiMessage>,
}

impl CpuIf {
    const fn new() -> Self {
        Self {
            msg_queue: LinkedList::new(),
        }
    }

    fn push(&mut self, ipi_msg: IpiMessage) {
        self.msg_queue.push_back(ipi_msg);
    }

    fn pop(&mut self) -> Option<IpiMessage> {
        self.msg_queue.pop_front()
    }
}

pub fn ipi_init() {
    if current_cpu().id == 0 {
        interrupt_reserve_int(INTERRUPT_IRQ_IPI, ipi_irq_handler);

        info!("Interrupt init ok");
    }
    interrupt_cpu_enable(INTERRUPT_IRQ_IPI, true);
}

fn interrupt_inject_ipi_handler(msg: IpiMessage) {
    match msg.ipi_message {
        IpiInnerMsg::IntInjectMsg(int_msg) => {
            let vm_id = int_msg.vm_id;
            let int_id = int_msg.int_id;
            match current_cpu().vcpu_array.pop_vcpu_through_vmid(vm_id) {
                None => {
                    panic!("inject int {} to illegal cpu {}", int_id, current_cpu().id);
                }
                Some(vcpu) => {
                    interrupt_vm_inject(&vcpu.vm().unwrap(), vcpu, int_id);
                }
            }
        }
        _ => {
            error!("interrupt_inject_ipi_handler: illegal ipi type");
        }
    }
}

static CPU_IF_LIST: [Mutex<CpuIf>; static_config::CORE_NUM] =
    [const { Mutex::new(CpuIf::new()) }; static_config::CORE_NUM];

fn ipi_pop_message(cpu_id: usize) -> Option<IpiMessage> {
    let mut cpu_if = CPU_IF_LIST[cpu_id].lock();
    let msg = cpu_if.pop();
    // drop the lock manully
    drop(cpu_if);
    msg
}

fn ipi_irq_handler() {
    let cpu_id = current_cpu().id;

    while let Some(ipi_msg) = ipi_pop_message(cpu_id) {
        ipi_dispatch(ipi_msg);
    }
}

fn ipi_dispatch(ipi_msg: IpiMessage) {
    let ipi_type = ipi_msg.ipi_type;

    if let Some(handler) = IPI_HANDLER_LIST.get(ipi_type as usize) {
        handler(ipi_msg);
    } else {
        error!("illegal ipi type {:?}", ipi_type)
    }
}

// Handle only the IPI messages an EL2 wait loop can run without affecting
// the waiter itself: the MapIPA broadcast of a VM create. That handler only
// installs page-table entries for a brand-new VM on this core -- never the
// waiter's own VM, whose mappings and exception frame stay untouched -- and
// ends in the all-core barrier the waiter would otherwise stall until its
// wait is over. Every other message (the unmap and cpu removal of a VM
// remove, which may hit the waiter's own VM and frame, vcpu wakeups, device
// and injection work) stays queued for the normal interrupt path once the
// wait returns, exactly as before; the loop stops at the first queued
// message that is not MapIPA, so the FIFO order is preserved. Callers run
// with interrupts masked, so nothing can change the queue between the peek
// and the pop.
//
// Only waits that hold no lock of their own may call this: arriving at the
// barrier while holding, e.g., the unilib list lock would make releasing
// that lock depend on every other core arriving.
pub fn ipi_process_pending() {
    let cpu_id = current_cpu().id;

    while ipi_front_is_mapipa(cpu_id) {
        match ipi_pop_message(cpu_id) {
            Some(ipi_msg) => ipi_dispatch(ipi_msg),
            None => break,
        }
    }
}

fn ipi_front_is_mapipa(cpu_id: usize) -> bool {
    let cpu_if = CPU_IF_LIST[cpu_id].lock();
    matches!(
        cpu_if.msg_queue.front(),
        Some(IpiMessage {
            ipi_message: IpiInnerMsg::VmmPercoreMsg(msg),
            ..
        }) if matches!(msg.event, VmmPercoreEvent::MapIPA)
    )
}

fn ipi_send(target_id: usize, msg: IpiMessage) -> bool {
    if target_id >= PLAT_DESC.cpu_desc.num {
        error!("ipi_send: core {} not exist", target_id);
        return false;
    }

    CPU_IF_LIST[target_id].lock().push(msg);
    interrupt_cpu_ipi_send(target_id, INTERRUPT_IRQ_IPI);

    true
}

pub fn ipi_send_msg(target_id: usize, ipi_type: IpiType, ipi_message: IpiInnerMsg) -> bool {
    let msg = IpiMessage { ipi_type, ipi_message };
    ipi_send(target_id, msg)
}

pub fn ipi_intra_broadcast_msg(vm: &Vm, ipi_type: IpiType, msg: IpiInnerMsg) -> bool {
    let mut i = 0;
    let mut n = 0;
    while n < (vm.cpu_num() - 1) {
        if ((1 << i) & vm.ncpu()) != 0 && i != current_cpu().id {
            n += 1;
            if !ipi_send_msg(i, ipi_type, msg.clone()) {
                error!(
                    "ipi_intra_broadcast_msg: Failed to send ipi request, cpu {} type {}",
                    i, ipi_type as usize
                );
                return false;
            }
        }

        i += 1;
    }
    true
}
