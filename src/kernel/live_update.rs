use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use cortex_a::asm::nop;
use spin::{Mutex, RwLock};

use crate::arch::{
    emu_intc_handler, GIC_LRS_NUM, gic_maintenance_handler, gicc_clear_current_irq, PageTable,
    partial_passthrough_intc_handler, psci_ipi_handler, TIMER_FREQ, TIMER_SLICE, Vgic, vgic_ipi_handler,
};
use crate::config::{
    DEF_VM_CONFIG_TABLE, vm_cfg_entry, VmConfigEntry, VmConfigTable, VmDtbDevConfig, VMDtbDevConfigList,
    VmEmulatedDeviceConfig, VmEmulatedDeviceConfigList, VmMemoryConfig, VmPassthroughDeviceConfig,
};
use crate::device::{
    EMU_DEVS_LIST, emu_virtio_mmio_handler, EmuDevEntry, EmuDeviceType, EmuDevs, ethernet_ipi_rev_handler,
    MEDIATED_BLK_LIST, mediated_ipi_handler, mediated_notify_ipi_handler, MediatedBlk, virtio_blk_notify_handler,
    virtio_console_notify_handler, virtio_mediated_blk_notify_handler, virtio_net_notify_handler, VirtioMmio,
};
use crate::kernel::{
    CPU, Cpu, cpu_idle, CPU_IF_LIST, CpuIf, CpuState, current_cpu, HEAP_REGION, HeapRegion, hvc_ipi_handler,
    INTERRUPT_GLB_BITMAP, INTERRUPT_HANDLERS, INTERRUPT_HYPER_BITMAP, interrupt_inject_ipi_handler, InterruptHandler,
    IPI_HANDLER_LIST, ipi_irq_handler, ipi_register, IpiHandler, IpiInnerMsg, IpiMediatedMsg, IpiMessage, IpiType,
    mem_heap_region_init, SchedType, SchedulerRR, timer_irq_handler, Vcpu, VCPU_LIST, VcpuInner, VcpuPool, vm, Vm,
    VM_IF_LIST, vm_ipa2pa, VM_LIST, VM_NUM_MAX, VM_REGION, VmInner, VmInterface, VmRegion,
};
use crate::lib::{BitAlloc256, BitMap, FlexBitmap};
use crate::mm::{heap_init, PageFrame};
use crate::vmm::vmm_ipi_handler;

#[derive(Copy, Clone, Debug, PartialEq)]
enum FreshStatus {
    Start,
    FreshVM,
    Finish,
    None,
}

static FRESH_STATUS: RwLock<FreshStatus> = RwLock::new(FreshStatus::None);
// static FRESH_STATUS: FreshStatus = FreshStatus::None;

fn set_fresh_status(status: FreshStatus) {
    *FRESH_STATUS.write() = status;
}

fn fresh_status() -> FreshStatus {
    *FRESH_STATUS.read()
}

#[repr(C)]
pub struct HypervisorAddr {
    cpu_id: usize,
    vm_list: usize,
    vm_config_table: usize,
    vcpu_list: usize,
    cpu: usize,
    emu_dev_list: usize,
    interrupt_hyper_bitmap: usize,
    interrupt_glb_bitmap: usize,
    interrupt_handlers: usize,
    vm_region: usize,
    heap_region: usize,
    vm_if_list: usize,
    gic_lrs_num: usize,
    // address for ipi
    cpu_if_list: usize,
    ipi_handler_list: usize,
    // arch time
    time_freq: usize,
    time_slice: usize,
    // mediated blk
    mediated_blk_list: usize,
}

pub fn hyper_fresh_ipi_handler(_msg: &IpiMessage) {
    update_request();
}

pub fn update_request() {
    // println!("Src Hypervisor Core[{}] send update request", current_cpu().id);
    extern "C" {
        pub fn update_request(address_list: &HypervisorAddr);
    }
    unsafe {
        let vm_config_table = &DEF_VM_CONFIG_TABLE as *const _ as usize;
        let emu_dev_list = &EMU_DEVS_LIST as *const _ as usize;
        let interrupt_hyper_bitmap = &INTERRUPT_HYPER_BITMAP as *const _ as usize;
        let interrupt_glb_bitmap = &INTERRUPT_GLB_BITMAP as *const _ as usize;
        let interrupt_handlers = &INTERRUPT_HANDLERS as *const _ as usize;
        let vm_region = &VM_REGION as *const _ as usize;
        let heap_region = &HEAP_REGION as *const _ as usize;
        let vm_list = &VM_LIST as *const _ as usize;
        let vm_if_list = &VM_IF_LIST as *const _ as usize;
        let vcpu_list = &VCPU_LIST as *const _ as usize;
        let cpu = &CPU as *const _ as usize;
        let cpu_if_list = &CPU_IF_LIST as *const _ as usize;
        let gic_lrs_num = &GIC_LRS_NUM as *const _ as usize;
        let ipi_handler_list = &IPI_HANDLER_LIST as *const _ as usize;
        let time_freq = &TIMER_FREQ as *const _ as usize;
        let time_slice = &TIMER_SLICE as *const _ as usize;
        let mediated_blk_list = &MEDIATED_BLK_LIST as *const _ as usize;

        let addr_list = HypervisorAddr {
            cpu_id: current_cpu().id,
            vm_config_table,
            emu_dev_list,
            interrupt_hyper_bitmap,
            interrupt_glb_bitmap,
            interrupt_handlers,
            vm_region,
            heap_region,
            vm_list,
            vm_if_list,
            vcpu_list,
            cpu,
            cpu_if_list,
            gic_lrs_num,
            ipi_handler_list,
            time_freq,
            time_slice,
            mediated_blk_list,
        };
        update_request(&addr_list);
    }
}

#[no_mangle]
pub extern "C" fn rust_shyper_update(address_list: &HypervisorAddr) {
    // TODO: SHARED_MEM
    // TODO: vm0_dtb?
    // TODO: mediated dev
    // TODO: async task
    if address_list.cpu_id == 0 {
        heap_init();
        mem_heap_region_init();
        set_fresh_status(FreshStatus::Start);
        unsafe {
            // DEF_VM_CONFIG_TABLE
            let vm_config_table = &*(address_list.vm_config_table as *const Mutex<VmConfigTable>);
            vm_config_table_update(vm_config_table);

            // VM_LIST
            let vm_list = &*(address_list.vm_list as *const Mutex<Vec<Vm>>);
            vm_list_update(vm_list);

            // VCPU_LIST
            let vcpu_list = &*(address_list.vcpu_list as *const Mutex<Vec<Vcpu>>);
            vcpu_update(vcpu_list, vm_list);

            set_fresh_status(FreshStatus::FreshVM);
            // CPU: Must update after vcpu and vm
            let cpu = &*(address_list.cpu as *const Cpu);
            current_cpu_update(cpu);

            // EMU_DEVS_LIST
            let emu_dev_list = &*(address_list.emu_dev_list as *const Mutex<Vec<EmuDevEntry>>);
            emu_dev_list_update(emu_dev_list);

            // INTERRUPT_HYPER_BITMAP, INTERRUPT_GLB_BITMAP, INTERRUPT_HANDLERS
            let interrupt_hyper_bitmap = &*(address_list.interrupt_hyper_bitmap as *const Mutex<BitMap<BitAlloc256>>);
            let interrupt_glb_bitmap = &*(address_list.interrupt_glb_bitmap as *const Mutex<BitMap<BitAlloc256>>);
            let interrupt_handlers =
                &*(address_list.interrupt_handlers as *const Mutex<BTreeMap<usize, InterruptHandler>>);
            interrupt_update(interrupt_hyper_bitmap, interrupt_glb_bitmap, interrupt_handlers);

            // VM_REGION
            let vm_region = &*(address_list.vm_region as *const Mutex<VmRegion>);
            vm_region_update(vm_region);

            // HEAP_REGION
            let heap_region = &*(address_list.heap_region as *const Mutex<HeapRegion>);
            heap_region_update(heap_region);

            // GIC_LRS_NUM
            let gic_lrs_num = &*(address_list.gic_lrs_num as *const Mutex<usize>);
            gic_lrs_num_update(gic_lrs_num);

            // VM_IF_LIST
            let vm_if_list = &*(address_list.vm_if_list as *const [Mutex<VmInterface>; VM_NUM_MAX]);
            vm_if_list_update(vm_if_list);

            // IPI_HANDLER_LIST
            let ipi_handler_list = &*(address_list.ipi_handler_list as *const Mutex<Vec<IpiHandler>>);
            ipi_handler_list_update(ipi_handler_list);

            // cpu_if_list
            let cpu_if = &*(address_list.cpu_if_list as *const Mutex<Vec<CpuIf>>);
            cpu_if_update(cpu_if);

            // TIMER_FREQ & TIMER_SLICE
            let time_freq = &*(address_list.time_freq as *const Mutex<usize>);
            let time_slice = &*(address_list.time_slice as *const Mutex<usize>);
            arch_time_update(time_freq, time_slice);

            // MEDIATED_BLK_LIST
            let mediated_blk_list = &*(address_list.mediated_blk_list as *const Mutex<Vec<MediatedBlk>>);
            mediated_blk_list_update(mediated_blk_list);

            set_fresh_status(FreshStatus::Finish);
        }
    } else {
        let cpu = unsafe { &*(address_list.cpu as *const Cpu) };
        while fresh_status() != FreshStatus::FreshVM && fresh_status() != FreshStatus::Finish {
            for i in 0..10000 {
                nop();
            }
        }
        if fresh_status() == FreshStatus::FreshVM || fresh_status() == FreshStatus::Finish {
            // CPU: Must update after vcpu and vm
            current_cpu_update(cpu);
        }
    }
    fresh_hyper();
}

pub fn fresh_hyper() {
    extern "C" {
        pub fn fresh_cpu();
        pub fn fresh_hyper(ctx: usize);
    }
    if current_cpu().id == 0 {
        let ctx = current_cpu().ctx.unwrap();
        println!("CPU[{}] ctx {:x}", current_cpu().id, ctx);
        current_cpu().clear_ctx();
        unsafe { fresh_hyper(ctx) };
    } else {
        match current_cpu().cpu_state {
            CpuState::CpuInv => {
                panic!("Core[{}] state {:#?}", current_cpu().id, CpuState::CpuInv);
            }
            CpuState::CpuIdle => {
                println!("Core[{}] state {:#?}", current_cpu().id, CpuState::CpuIdle);
                unsafe { fresh_cpu() };
                println!(
                    "Core[{}] current cpu irq {}",
                    current_cpu().id,
                    current_cpu().current_irq
                );
                gicc_clear_current_irq(true);
                cpu_idle();
            }
            CpuState::CpuRun => {
                println!("Core[{}] state {:#?}", current_cpu().id, CpuState::CpuRun);
                println!(
                    "Core[{}] current cpu irq {}",
                    current_cpu().id,
                    current_cpu().current_irq
                );
                gicc_clear_current_irq(true);
                let ctx = current_cpu().ctx.unwrap();
                current_cpu().clear_ctx();
                unsafe { fresh_hyper(ctx) };
            }
        }
    }
}

pub fn mediated_blk_list_update(src_mediated_blk_list: &Mutex<Vec<MediatedBlk>>) {
    let mut mediated_blk_list = MEDIATED_BLK_LIST.lock();
    assert_eq!(mediated_blk_list.len(), 0);
    mediated_blk_list.clear();
    for blk in src_mediated_blk_list.lock().iter() {
        mediated_blk_list.push(MediatedBlk {
            base_addr: blk.base_addr,
            avail: blk.avail,
        });
    }
}

pub fn arch_time_update(src_time_freq: &Mutex<usize>, src_time_slice: &Mutex<usize>) {
    *TIMER_FREQ.lock() = *src_time_freq.lock();
    *TIMER_SLICE.lock() = *src_time_slice.lock();
}

pub fn cpu_if_update(src_cpu_if: &Mutex<Vec<CpuIf>>) {
    let mut cpu_if_list = CPU_IF_LIST.lock();
    assert_eq!(cpu_if_list.len(), 0);
    cpu_if_list.clear();
    for cpu_if in src_cpu_if.lock().iter() {
        let mut new_cpu_if = CpuIf::default();
        for msg in cpu_if.msg_queue.iter() {
            // Copy ipi msg
            let new_ipi_msg = match msg.ipi_message.clone() {
                IpiInnerMsg::Initc(initc) => IpiInnerMsg::Initc(initc),
                IpiInnerMsg::Power(power) => IpiInnerMsg::Power(power),
                IpiInnerMsg::EnternetMsg(eth_msg) => IpiInnerMsg::EnternetMsg(eth_msg),
                IpiInnerMsg::VmmMsg(vmm_msg) => IpiInnerMsg::VmmMsg(vmm_msg),
                IpiInnerMsg::MediatedMsg(mediated_msg) => {
                    let mmio_id = mediated_msg.blk.id();
                    let vm_id = mediated_msg.src_id;
                    let vq_idx = mediated_msg.vq.vq_indx();

                    let vm = vm(vm_id).unwrap();
                    match vm.emu_dev(mmio_id) {
                        EmuDevs::VirtioBlk(blk) => {
                            let new_vq = blk.vq(vq_idx).clone().unwrap();
                            IpiInnerMsg::MediatedMsg(IpiMediatedMsg {
                                src_id: vm_id,
                                vq: new_vq.clone(),
                                blk: blk.clone(),
                            })
                        }
                        _ => {
                            panic!("illegal mmio dev type in cpu_if_update");
                        }
                    }
                }
                IpiInnerMsg::MediatedNotifyMsg(notify_msg) => IpiInnerMsg::MediatedNotifyMsg(notify_msg),
                IpiInnerMsg::HvcMsg(hvc_msg) => IpiInnerMsg::HvcMsg(hvc_msg),
                IpiInnerMsg::IntInjectMsg(inject_msg) => IpiInnerMsg::IntInjectMsg(inject_msg),
                IpiInnerMsg::HyperFreshMsg() => IpiInnerMsg::HyperFreshMsg(),
                IpiInnerMsg::None => IpiInnerMsg::None,
            };
            new_cpu_if.msg_queue.push(IpiMessage {
                ipi_type: msg.ipi_type,
                ipi_message: new_ipi_msg,
            })
        }
        cpu_if_list.push(new_cpu_if);
    }
    println!("Update CPU_IF_LIST");
}

pub fn ipi_handler_list_update(src_ipi_handler_list: &Mutex<Vec<IpiHandler>>) {
    for ipi_handler in src_ipi_handler_list.lock().iter() {
        let handler = match ipi_handler.ipi_type {
            IpiType::IpiTIntc => vgic_ipi_handler,
            IpiType::IpiTPower => psci_ipi_handler,
            IpiType::IpiTEthernetMsg => ethernet_ipi_rev_handler,
            IpiType::IpiTHvc => hvc_ipi_handler,
            IpiType::IpiTVMM => vmm_ipi_handler,
            IpiType::IpiTMediatedDev => mediated_ipi_handler,
            IpiType::IpiTMediatedNotify => mediated_notify_ipi_handler,
            IpiType::IpiTIntInject => interrupt_inject_ipi_handler,
            IpiType::IpiTHyperFresh => hyper_fresh_ipi_handler,
        };
        ipi_register(ipi_handler.ipi_type, handler);
    }
    println!("Update IPI_HANDLER_LIST");
}

pub fn vm_if_list_update(src_vm_if_list: &[Mutex<VmInterface>; VM_NUM_MAX]) {
    for (idx, vm_if_lock) in src_vm_if_list.iter().enumerate() {
        let vm_if = vm_if_lock.lock();
        let mut cur_vm_if = VM_IF_LIST[idx].lock();
        cur_vm_if.master_cpu_id = vm_if.master_cpu_id;
        cur_vm_if.state = vm_if.state;
        cur_vm_if.vm_type = vm_if.vm_type;
        cur_vm_if.mac = vm_if.mac;
        cur_vm_if.ivc_arg = vm_if.ivc_arg;
        cur_vm_if.ivc_arg_ptr = vm_if.ivc_arg_ptr;
        cur_vm_if.mem_map = match &vm_if.mem_map {
            None => None,
            Some(mem_map) => Some(FlexBitmap {
                len: mem_map.len,
                map: {
                    let mut map = vec![];
                    for v in mem_map.map.iter() {
                        map.push(*v);
                    }
                    map
                },
            }),
        };
        cur_vm_if.mem_map_cache = match &vm_if.mem_map_cache {
            None => None,
            Some(cache) => Some(PageFrame::new(cache.pa)),
        };
    }
    println!("Update VM_IF_LIST")
}

pub fn current_cpu_update(src_cpu: &Cpu) {
    let cpu = current_cpu();
    // only need to alloc a new VcpuPool from heap, other props all map at 0x400000000
    // current_cpu().sched = src_cpu.sched;
    match &src_cpu.sched {
        SchedType::SchedRR(rr) => {
            let new_rr = SchedulerRR {
                pool: VcpuPool::default(),
            };
            for idx in 0..rr.pool.vcpu_num() {
                let src_vcpu = rr.pool.vcpu(idx);
                let vm_id = src_vcpu.vm_id();
                let new_vcpu = vm(vm_id).unwrap().vcpu(src_vcpu.id()).unwrap();
                new_rr.pool.append_vcpu(new_vcpu.clone());
            }
            new_rr.pool.set_running(rr.pool.running());
            new_rr.pool.set_slice(rr.pool.slice());
            if rr.pool.active_idx() < rr.pool.vcpu_num() {
                new_rr.pool.set_active_vcpu(rr.pool.active_idx());
                cpu.active_vcpu = Some(new_rr.pool.vcpu(rr.pool.active_idx()));
            } else {
                cpu.active_vcpu = None;
            }
            cpu.sched = SchedType::SchedRR(new_rr);
        }
        SchedType::None => {
            cpu.sched = SchedType::None;
        }
    }

    assert_eq!(cpu.id, src_cpu.id);
    assert_eq!(cpu.ctx, src_cpu.ctx);
    assert_eq!(cpu.cpu_state, src_cpu.cpu_state);
    assert_eq!(cpu.assigned, src_cpu.assigned);
    assert_eq!(cpu.current_irq, src_cpu.current_irq);
    assert_eq!(cpu.cpu_pt, src_cpu.cpu_pt);
    assert_eq!(cpu.stack, src_cpu.stack);
    println!("Update CPU[{}]", cpu.id);
}

pub fn gic_lrs_num_update(src_gic_lrs_num: &Mutex<usize>) {
    let gic_lrs_num = *src_gic_lrs_num.lock();
    *GIC_LRS_NUM.lock() = gic_lrs_num;
    println!("Update GIC_LRS_NUM");
}

// Set vm.vcpu_list in vcpu_update
pub fn vm_list_update(src_vm_list: &Mutex<Vec<Vm>>) {
    let mut vm_list = VM_LIST.lock();
    assert_eq!(vm_list.len(), 0);
    vm_list.clear();
    drop(vm_list);
    for vm in src_vm_list.lock().iter() {
        let old_inner = vm.inner.lock();
        let pt = match &old_inner.pt {
            None => None,
            Some(page_table) => {
                let new_page_table = PageTable {
                    directory: PageFrame::new(page_table.directory.pa),
                    pages: Mutex::new(vec![]),
                };
                for page in page_table.pages.lock().iter() {
                    new_page_table.pages.lock().push(PageFrame::new(page.pa));
                }
                Some(new_page_table)
            }
        };

        let new_inner = VmInner {
            id: old_inner.id,
            ready: old_inner.ready,
            config: vm_cfg_entry(old_inner.id),
            dtb: old_inner.dtb, // maybe need to reset
            pt,
            mem_region_num: old_inner.mem_region_num,
            pa_region: {
                let mut pa_region = vec![];
                for region in old_inner.pa_region.iter() {
                    pa_region.push(*region);
                }
                pa_region
            },
            entry_point: old_inner.entry_point,
            has_master: old_inner.has_master,
            vcpu_list: vec![],
            cpu_num: old_inner.cpu_num,
            ncpu: old_inner.ncpu,
            intc_dev_id: old_inner.intc_dev_id,
            int_bitmap: old_inner.int_bitmap,
            share_mem_base: old_inner.share_mem_base,
            migrate_save_pf: {
                let mut pf = vec![];
                for page in old_inner.migrate_save_pf.iter() {
                    pf.push(PageFrame::new(page.pa));
                }
                pf
            },
            migrate_restore_pf: {
                let mut pf = vec![];
                for page in old_inner.migrate_restore_pf.iter() {
                    pf.push(PageFrame::new(page.pa));
                }
                pf
            },
            med_blk_id: old_inner.med_blk_id,
            emu_devs: {
                let mut emu_devs = vec![];
                drop(old_inner);
                let old_emu_devs = vm.inner.lock().emu_devs.clone();
                for dev in old_emu_devs.iter() {
                    // TODO: wip
                    let new_dev = match dev {
                        EmuDevs::Vgic(vgic) => {
                            // set vgic after vcpu update
                            EmuDevs::None
                        }
                        EmuDevs::VirtioBlk(blk) => {
                            let mmio = VirtioMmio::new(0);
                            assert_eq!(
                                (blk.vq(0).unwrap().desc_table()),
                                vm_ipa2pa(vm.clone(), blk.vq(0).unwrap().desc_table_addr())
                            );
                            assert_eq!(
                                (blk.vq(0).unwrap().used()),
                                vm_ipa2pa(vm.clone(), blk.vq(0).unwrap().used_addr())
                            );
                            assert_eq!(
                                (blk.vq(0).unwrap().avail()),
                                vm_ipa2pa(vm.clone(), blk.vq(0).unwrap().avail_addr())
                            );
                            mmio.save_mmio(
                                blk.clone(),
                                if blk.dev().mediated() {
                                    Some(virtio_mediated_blk_notify_handler)
                                } else {
                                    Some(virtio_blk_notify_handler)
                                },
                            );
                            EmuDevs::VirtioBlk(mmio)
                        }
                        EmuDevs::VirtioNet(net) => {
                            let mmio = VirtioMmio::new(0);
                            assert_eq!(
                                (net.vq(0).unwrap().desc_table()),
                                vm_ipa2pa(vm.clone(), net.vq(0).unwrap().desc_table_addr())
                            );
                            assert_eq!(
                                (net.vq(0).unwrap().used()),
                                vm_ipa2pa(vm.clone(), net.vq(0).unwrap().used_addr())
                            );
                            assert_eq!(
                                (net.vq(0).unwrap().avail()),
                                vm_ipa2pa(vm.clone(), net.vq(0).unwrap().avail_addr())
                            );
                            mmio.save_mmio(net.clone(), Some(virtio_net_notify_handler));
                            EmuDevs::VirtioNet(mmio)
                        }
                        EmuDevs::VirtioConsole(console) => {
                            let mmio = VirtioMmio::new(0);
                            assert_eq!(
                                (console.vq(0).unwrap().desc_table()),
                                vm_ipa2pa(vm.clone(), console.vq(0).unwrap().desc_table_addr())
                            );
                            assert_eq!(
                                (console.vq(0).unwrap().used()),
                                vm_ipa2pa(vm.clone(), console.vq(0).unwrap().used_addr())
                            );
                            assert_eq!(
                                (console.vq(0).unwrap().avail()),
                                vm_ipa2pa(vm.clone(), console.vq(0).unwrap().avail_addr())
                            );
                            mmio.save_mmio(console.clone(), Some(virtio_console_notify_handler));
                            EmuDevs::VirtioConsole(mmio)
                        }
                        EmuDevs::None => EmuDevs::None,
                    };
                    emu_devs.push(new_dev);
                }
                emu_devs
            },
        };
        let mut vm_list = VM_LIST.lock();
        vm_list.push(Vm {
            inner: Arc::new(Mutex::new(new_inner)),
        });
    }
    println!("Update VM_LIST");
}

pub fn heap_region_update(src_heap_region: &Mutex<HeapRegion>) {
    let mut heap_region = HEAP_REGION.lock();
    let src_region = src_heap_region.lock();
    heap_region.map = src_region.map;
    heap_region.region = src_region.region;
    assert_eq!(heap_region.region, src_region.region);
    println!("Update HEAP_REGION");
}

pub fn vm_region_update(src_vm_region: &Mutex<VmRegion>) {
    let mut vm_region = VM_REGION.lock();
    assert_eq!(vm_region.region.len(), 0);
    vm_region.region.clear();
    for mem_region in src_vm_region.lock().region.iter() {
        vm_region.region.push(*mem_region);
    }
    assert_eq!(vm_region.region, src_vm_region.lock().region);
    println!("Update {} region for VM_REGION", vm_region.region.len());
}

pub fn interrupt_update(
    src_hyper_bitmap: &Mutex<BitMap<BitAlloc256>>,
    src_glb_bitmap: &Mutex<BitMap<BitAlloc256>>,
    src_handlers: &Mutex<BTreeMap<usize, InterruptHandler>>,
) {
    let mut hyper_bitmap = INTERRUPT_HYPER_BITMAP.lock();
    *hyper_bitmap = *src_hyper_bitmap.lock();
    let mut glb_bitmap = INTERRUPT_GLB_BITMAP.lock();
    *glb_bitmap = *src_glb_bitmap.lock();
    let mut handlers = INTERRUPT_HANDLERS.lock();
    for (int_id, handler) in src_handlers.lock().iter() {
        match handler {
            InterruptHandler::IpiIrqHandler(_) => {
                handlers.insert(*int_id, InterruptHandler::IpiIrqHandler(ipi_irq_handler));
            }
            InterruptHandler::GicMaintenanceHandler(_) => {
                handlers.insert(
                    *int_id,
                    InterruptHandler::GicMaintenanceHandler(gic_maintenance_handler),
                );
            }
            InterruptHandler::TimeIrqHandler(_) => {
                handlers.insert(*int_id, InterruptHandler::TimeIrqHandler(timer_irq_handler));
            }
            InterruptHandler::None => {
                handlers.insert(*int_id, InterruptHandler::None);
            }
        }
    }
    println!("Update INTERRUPT_GLB_BITMAP / INTERRUPT_HYPER_BITMAP / INTERRUPT_HANDLERS");
}

pub fn emu_dev_list_update(src_emu_dev_list: &Mutex<Vec<EmuDevEntry>>) {
    let mut emu_dev_list = EMU_DEVS_LIST.lock();
    assert_eq!(emu_dev_list.len(), 0);
    emu_dev_list.clear();
    for emu_dev_entry in src_emu_dev_list.lock().iter() {
        let emu_handler = match emu_dev_entry.emu_type {
            EmuDeviceType::EmuDeviceTGicd => emu_intc_handler,
            EmuDeviceType::EmuDeviceTGPPT => partial_passthrough_intc_handler,
            EmuDeviceType::EmuDeviceTVirtioBlk => emu_virtio_mmio_handler,
            EmuDeviceType::EmuDeviceTVirtioNet => emu_virtio_mmio_handler,
            EmuDeviceType::EmuDeviceTVirtioConsole => emu_virtio_mmio_handler,
            _ => {
                panic!("not support emu dev entry type {:#?}", emu_dev_entry.emu_type);
            }
        };
        emu_dev_list.push(EmuDevEntry {
            emu_type: emu_dev_entry.emu_type,
            vm_id: emu_dev_entry.vm_id,
            id: emu_dev_entry.id,
            ipa: emu_dev_entry.ipa,
            size: emu_dev_entry.size,
            handler: emu_handler,
        });
    }
    println!("Update {} emu dev for EMU_DEVS_LIST", emu_dev_list.len());
}

pub fn vm_config_table_update(src_vm_config_table: &Mutex<VmConfigTable>) {
    let mut vm_config_table = DEF_VM_CONFIG_TABLE.lock();
    let src_config_table = src_vm_config_table.lock();
    vm_config_table.name = src_config_table.name;
    vm_config_table.vm_bitmap = src_config_table.vm_bitmap;
    vm_config_table.vm_num = src_config_table.vm_num;
    assert_eq!(vm_config_table.entries.len(), 0);
    vm_config_table.entries.clear();
    for entry in src_config_table.entries.iter() {
        let image = *entry.image.lock();
        let memory = VmMemoryConfig {
            region: {
                let mut region = vec![];
                for mem in entry.memory.lock().region.iter() {
                    region.push(*mem);
                }
                assert_eq!(region, entry.memory.lock().region);
                region
            },
        };
        let cpu = *entry.cpu.lock();
        // emu dev config
        let mut vm_emu_dev_confg = VmEmulatedDeviceConfigList { emu_dev_list: vec![] };
        let src_emu_dev_confg_list = entry.vm_emu_dev_confg.lock();
        for emu_config in &src_emu_dev_confg_list.emu_dev_list {
            vm_emu_dev_confg.emu_dev_list.push(VmEmulatedDeviceConfig {
                name: Some(String::from(emu_config.name.as_ref().unwrap())),
                base_ipa: emu_config.base_ipa,
                length: emu_config.length,
                irq_id: emu_config.irq_id,
                cfg_list: {
                    let mut cfg_list = vec![];
                    for cfg in emu_config.cfg_list.iter() {
                        cfg_list.push(*cfg);
                    }
                    assert_eq!(cfg_list, emu_config.cfg_list);
                    cfg_list
                },
                emu_type: emu_config.emu_type,
                mediated: emu_config.mediated,
            })
        }
        // passthrough dev config
        let src_pt = entry.vm_pt_dev_confg.lock();
        let mut vm_pt_dev_confg = VmPassthroughDeviceConfig {
            regions: vec![],
            irqs: vec![],
            streams_ids: vec![],
        };
        for region in src_pt.regions.iter() {
            vm_pt_dev_confg.regions.push(*region);
        }
        for irq in src_pt.irqs.iter() {
            vm_pt_dev_confg.irqs.push(*irq);
        }
        for streams_id in src_pt.streams_ids.iter() {
            vm_pt_dev_confg.streams_ids.push(*streams_id);
        }
        assert_eq!(vm_pt_dev_confg.regions, src_pt.regions);
        assert_eq!(vm_pt_dev_confg.irqs, src_pt.irqs);
        assert_eq!(vm_pt_dev_confg.streams_ids, src_pt.streams_ids);

        // dtb config
        let mut vm_dtb_devs = VMDtbDevConfigList {
            dtb_device_list: vec![],
        };
        let src_dtb_confg_list = entry.vm_dtb_devs.lock();
        for dtb_config in src_dtb_confg_list.dtb_device_list.iter() {
            vm_dtb_devs.dtb_device_list.push(VmDtbDevConfig {
                name: String::from(&dtb_config.name),
                dev_type: dtb_config.dev_type,
                irqs: {
                    let mut irqs = vec![];
                    for irq in dtb_config.irqs.iter() {
                        irqs.push(*irq);
                    }
                    assert_eq!(irqs, dtb_config.irqs);
                    irqs
                },
                addr_region: dtb_config.addr_region,
            });
        }

        vm_config_table.entries.push(VmConfigEntry {
            id: entry.id,
            name: Some(String::from(entry.name.as_ref().unwrap())),
            os_type: entry.os_type,
            cmdline: String::from(&entry.cmdline),
            image: Arc::new(Mutex::new(image)),
            memory: Arc::new(Mutex::new(memory)),
            cpu: Arc::new(Mutex::new(cpu)),
            vm_emu_dev_confg: Arc::new(Mutex::new(vm_emu_dev_confg)),
            vm_pt_dev_confg: Arc::new(Mutex::new(vm_pt_dev_confg)),
            vm_dtb_devs: Arc::new(Mutex::new(vm_dtb_devs)),
        });
    }
    assert_eq!(vm_config_table.entries.len(), src_config_table.entries.len());
    assert_eq!(vm_config_table.vm_num, src_config_table.vm_num);
    assert_eq!(vm_config_table.vm_bitmap, src_config_table.vm_bitmap);
    assert_eq!(vm_config_table.name, src_config_table.name);
    println!("Update {} VM to DEF_VM_CONFIG_TABLE", vm_config_table.vm_num);
}

pub fn vcpu_update(src_vcpu_list: &Mutex<Vec<Vcpu>>, src_vm_list: &Mutex<Vec<Vm>>) {
    let mut vcpu_list = VCPU_LIST.lock();
    assert_eq!(vcpu_list.len(), 0);
    vcpu_list.clear();
    for vcpu in src_vcpu_list.lock().iter() {
        let src_inner = vcpu.inner.lock();
        let src_vm_option = src_inner.vm.clone();
        let vm = match src_vm_option {
            None => None,
            Some(src_vm) => {
                let vm_id = src_vm.id();
                vm(vm_id)
            }
        };

        let vcpu_inner = VcpuInner {
            id: src_inner.id,
            phys_id: src_inner.phys_id,
            state: src_inner.state,
            vm: vm.clone(),
            int_list: {
                let mut int_list = vec![];
                for int in src_inner.int_list.iter() {
                    int_list.push(*int);
                }
                int_list
            },
            vcpu_ctx: src_inner.vcpu_ctx,
            vm_ctx: src_inner.vm_ctx,
        };
        assert_eq!(vcpu_inner.int_list, src_inner.int_list);
        let vcpu = Vcpu {
            inner: Arc::new(Mutex::new(vcpu_inner)),
        };
        vm.unwrap().push_vcpu(vcpu.clone());
        vcpu_list.push(vcpu);
    }

    // Add vgic emu dev for vm
    for src_vm in src_vm_list.lock().iter() {
        let src_vgic = src_vm.vgic();
        let new_vgic = Vgic::default();
        new_vgic.save_vgic(src_vgic.clone());

        let vm = vm(src_vm.id()).unwrap();
        if let EmuDevs::None = vm.emu_dev(vm.intc_dev_id()) {
        } else {
            panic!("illegal vgic emu dev idx in vm.emu_devs");
        }
        vm.set_emu_devs(vm.intc_dev_id(), EmuDevs::Vgic(Arc::new(new_vgic)));
    }
    assert_eq!(vcpu_list.len(), src_vcpu_list.lock().len());
    println!("Update {} Vcpu to VCPU_LIST", vcpu_list.len());
}