use alloc::string::String;
use alloc::vec::Vec;

use crate::arch::INTERRUPT_IRQ_GUEST_TIMER;
use crate::board::*;
use crate::config::vm_cfg_add_vm_entry;
use crate::device::EmuDeviceType;
use crate::kernel::VmType;

use super::{
    DtbDevType, PassthroughRegion, VMDtbDevConfigList, VmConfigEntry, VmCpuConfig, VmDtbDevConfig,
    VmEmulatedDeviceConfig, VmEmulatedDeviceConfigList, VmImageConfig, VmMemoryConfig, VmPassthroughDeviceConfig,
    VmRegion,
};

pub fn init_tmp_config_for_bma1() {
    info!("init_tmp_config_for_bma1");
    // #################### bare metal app emu (vm1) ######################
    let mut emu_dev_config: Vec<VmEmulatedDeviceConfig> = Vec::new();
    emu_dev_config.push(VmEmulatedDeviceConfig {
        name: String::from("intc@8000000"),
        base_ipa: 0x8000000,
        length: 0x1000,
        irq_id: 0,
        cfg_list: Vec::new(),
        emu_type: EmuDeviceType::EmuDeviceTGicd,
        mediated: false,
    });
    emu_dev_config.push(VmEmulatedDeviceConfig {
        name: String::from("virtio_blk@a000000"),
        base_ipa: 0xa000000,
        length: 0x1000,
        irq_id: 32 + 0x10,
        cfg_list: vec![0, 209715200], // 100G
        emu_type: EmuDeviceType::EmuDeviceTVirtioBlk,
        mediated: true,
    });

    // bma passthrough
    let mut pt_dev_config: VmPassthroughDeviceConfig = VmPassthroughDeviceConfig::default();
    pt_dev_config.regions = vec![
        PassthroughRegion {
            ipa: 0x9000000,
            pa: Platform::UART_1_ADDR,
            length: 0x1000,
            dev_property: true,
        },
        PassthroughRegion {
            ipa: 0x8010000,
            pa: Platform::GICV_BASE,
            length: 0x2000,
            dev_property: true,
        },
    ];
    pt_dev_config.irqs = vec![Platform::UART_1_INT];

    // bma vm_region
    let mut vm_region: Vec<VmRegion> = Vec::new();
    vm_region.push(VmRegion {
        ipa_start: 0x40000000,
        length: 0x40000000,
    });

    // bma config
    let bma_config = VmConfigEntry {
        id: 0,
        name: String::from("guest-bma-0"),
        os_type: VmType::VmTBma,
        memory: VmMemoryConfig {
            region: vm_region,
            colors: vec![],
            ..Default::default()
        },
        image: VmImageConfig {
            kernel_img_name: None,
            kernel_load_ipa: 0x40080000,
            kernel_entry_point: 0x40080000,
            device_tree_load_ipa: 0,
            ramdisk_load_ipa: 0,
        },
        cpu: VmCpuConfig {
            num: 1,
            allocate_bitmap: 0b0010,
            master: Some(1),
        },
        vm_emu_dev_confg: VmEmulatedDeviceConfigList {
            emu_dev_list: emu_dev_config,
        },
        vm_pt_dev_confg: pt_dev_config,
        vm_dtb_devs: VMDtbDevConfigList::default(),
        cmdline: String::from(""),
        mediated_block_index: None,
    };
    let _ = vm_cfg_add_vm_entry(bma_config);
}

pub fn init_tmp_config_for_bma2() {
    info!("init_tmp_config_for_bma2");
    // #################### bare metal app emu (vm1) ######################
    let mut emu_dev_config: Vec<VmEmulatedDeviceConfig> = Vec::new();
    emu_dev_config.push(VmEmulatedDeviceConfig {
        name: String::from("intc@8000000"),
        base_ipa: 0x8000000,
        length: 0x1000,
        irq_id: 0,
        cfg_list: Vec::new(),
        emu_type: EmuDeviceType::EmuDeviceTGicd,
        mediated: false,
    });
    emu_dev_config.push(VmEmulatedDeviceConfig {
        name: String::from("virtio_blk@a000000"),
        base_ipa: 0xa000000,
        length: 0x1000,
        irq_id: 32 + 0x10,
        cfg_list: vec![0, 209715200], // 100G
        emu_type: EmuDeviceType::EmuDeviceTVirtioBlk,
        mediated: true,
    });

    // bma passthrough
    let mut pt_dev_config: VmPassthroughDeviceConfig = VmPassthroughDeviceConfig::default();
    pt_dev_config.regions = vec![
        PassthroughRegion {
            ipa: 0x9000000,
            pa: Platform::UART_1_ADDR,
            length: 0x1000,
            dev_property: true,
        },
        PassthroughRegion {
            ipa: 0x8010000,
            pa: Platform::GICV_BASE,
            length: 0x2000,
            dev_property: true,
        },
    ];
    // pt_dev_config.irqs = vec![UART_1_INT];

    // bma vm_region
    let mut vm_region: Vec<VmRegion> = Vec::new();
    vm_region.push(VmRegion {
        ipa_start: 0x40000000,
        length: 0x40000000,
    });

    // bma config
    let bma_config = VmConfigEntry {
        id: 0,
        name: String::from("guest-bma-1"),
        os_type: VmType::VmTBma,
        memory: VmMemoryConfig {
            region: vm_region,
            colors: vec![],
            ..Default::default()
        },
        image: VmImageConfig {
            kernel_img_name: None,
            kernel_load_ipa: 0x40080000,
            kernel_entry_point: 0x40080000,
            device_tree_load_ipa: 0,
            ramdisk_load_ipa: 0,
        },
        cpu: VmCpuConfig {
            num: 1,
            allocate_bitmap: 0b0100,
            master: Some(2),
        },
        vm_emu_dev_confg: VmEmulatedDeviceConfigList {
            emu_dev_list: emu_dev_config,
        },
        vm_pt_dev_confg: pt_dev_config,
        vm_dtb_devs: VMDtbDevConfigList::default(),
        cmdline: String::from(""),
        mediated_block_index: None,
    };
    let _ = vm_cfg_add_vm_entry(bma_config);
}

pub fn init_tmp_config_for_vm1() {
    info!("init_tmp_config_for_vm1");

    // #################### vm1 emu ######################
    let mut emu_dev_config: Vec<VmEmulatedDeviceConfig> = Vec::new();
    emu_dev_config.push(VmEmulatedDeviceConfig {
        name: String::from("intc@8000000"),
        base_ipa: 0x8000000,
        length: 0x1000,
        irq_id: 0,
        cfg_list: Vec::new(),
        emu_type: EmuDeviceType::EmuDeviceTGicd,
        mediated: false,
    });
    emu_dev_config.push(VmEmulatedDeviceConfig {
        name: String::from("virtio_blk@a000000"),
        base_ipa: 0xa000000,
        length: 0x1000,
        irq_id: 32 + 0x10,
        // cfg_list: vec![DISK_PARTITION_2_START, DISK_PARTITION_2_SIZE],
        // cfg_list: vec![0, 8388608],
        // cfg_list: vec![0, 67108864i], // 32G
        cfg_list: vec![0, 209715200], // 100G
        emu_type: EmuDeviceType::EmuDeviceTVirtioBlk,
        mediated: true,
    });
    emu_dev_config.push(VmEmulatedDeviceConfig {
        name: String::from("virtio_net@a001000"),
        base_ipa: 0xa001000,
        length: 0x1000,
        irq_id: 32 + 0x11,
        cfg_list: vec![0x74, 0x56, 0xaa, 0x0f, 0x47, 0xd1],
        emu_type: EmuDeviceType::EmuDeviceTVirtioNet,
        mediated: false,
    });
    emu_dev_config.push(VmEmulatedDeviceConfig {
        name: String::from("virtio_console@a002000"),
        base_ipa: 0xa002000,
        length: 0x1000,
        irq_id: 32 + 0x12,
        cfg_list: vec![0, 0xa002000],
        emu_type: EmuDeviceType::EmuDeviceTVirtioConsole,
        mediated: false,
    });
    // emu_dev_config.push(VmEmulatedDeviceConfig {
    //     name: String::from("vm_service"),
    //     base_ipa: 0,
    //     length: 0,
    //     irq_id: HVC_IRQ,
    //     cfg_list: Vec::new(),
    //     emu_type: EmuDeviceType::EmuDeviceTShyper,
    //     mediated: false,
    // });

    // vm1 passthrough
    let mut pt_dev_config: VmPassthroughDeviceConfig = VmPassthroughDeviceConfig::default();
    pt_dev_config.regions = vec![
        // PassthroughRegion {
        //     ipa: UART_1_ADDR,
        //     pa: UART_1_ADDR,
        //     length: 0x1000,
        //     dev_property: true
        // },
        PassthroughRegion {
            ipa: 0x8010000,
            pa: Platform::GICV_BASE,
            length: 0x2000,
            dev_property: true,
        },
    ];
    // pt_dev_config.irqs = vec![UART_1_INT, INTERRUPT_IRQ_GUEST_TIMER];
    pt_dev_config.irqs = vec![INTERRUPT_IRQ_GUEST_TIMER];

    // vm1 vm_region
    let mut vm_region: Vec<VmRegion> = Vec::new();
    vm_region.push(VmRegion {
        ipa_start: 0x80000000,
        length: 0x40000000,
    });

    let mut vm_dtb_devs: Vec<VmDtbDevConfig> = vec![];
    vm_dtb_devs.push(VmDtbDevConfig {
        name: String::from("gicd"),
        dev_type: DtbDevType::Gicd,
        irqs: vec![],
        addr_region: VmRegion {
            ipa_start: 0x8000000,
            length: 0x1000,
        },
    });
    vm_dtb_devs.push(VmDtbDevConfig {
        name: String::from("gicc"),
        dev_type: DtbDevType::Gicc,
        irqs: vec![],
        addr_region: VmRegion {
            ipa_start: 0x8010000,
            length: 0x2000,
        },
    });
    // vm_dtb_devs.push(VmDtbDevConfig {
    //     name: String::from("serial"),
    //     dev_type: DtbDevType::DevSerial,
    //     irqs: vec![UART_1_INT],
    //     addr_region: VmRegion {
    //         ipa_start: UART_1_ADDR,
    //         length: 0x1000,
    //     },
    // });

    // vm1 config
    let vm1_config = VmConfigEntry {
        id: 1,
        name: String::from("guest-os-0"),
        os_type: VmType::VmTOs,
        // cmdline: "root=/dev/vda rw audit=0",
        cmdline: String::from("earlycon console=hvc0,115200n8 root=/dev/vda rw audit=0"),

        image: VmImageConfig {
            kernel_img_name: Some("Image_vanilla"),
            kernel_load_ipa: 0x80080000,
            kernel_entry_point: 0x80080000,
            device_tree_load_ipa: 0x80000000,
            ramdisk_load_ipa: 0, //0x83000000,
        },
        memory: VmMemoryConfig {
            region: vm_region,
            colors: vec![],
            ..Default::default()
        },
        cpu: VmCpuConfig {
            num: 1,
            allocate_bitmap: 0b0010,
            master: Some(1),
        },
        vm_emu_dev_confg: VmEmulatedDeviceConfigList {
            emu_dev_list: emu_dev_config,
        },
        vm_pt_dev_confg: pt_dev_config,
        vm_dtb_devs: VMDtbDevConfigList {
            dtb_device_list: vm_dtb_devs,
        },
        mediated_block_index: Some(0),
    };
    info!("generate tmp_config for vm1");
    let _ = vm_cfg_add_vm_entry(vm1_config);
}

pub fn init_tmp_config_for_vm2() {
    info!("init_tmp_config_for_vm2");

    // #################### vm2 emu ######################
    let mut emu_dev_config: Vec<VmEmulatedDeviceConfig> = Vec::new();
    emu_dev_config.push(VmEmulatedDeviceConfig {
        name: String::from("intc@8000000"),
        base_ipa: 0x8000000,
        length: 0x1000,
        irq_id: 0,
        cfg_list: Vec::new(),
        emu_type: EmuDeviceType::EmuDeviceTGicd,
        mediated: false,
    });
    emu_dev_config.push(VmEmulatedDeviceConfig {
        name: String::from("virtio_blk@a000000"),
        base_ipa: 0xa000000,
        length: 0x1000,
        irq_id: 32 + 0x10,
        cfg_list: vec![0, 209715200], // 100G
        emu_type: EmuDeviceType::EmuDeviceTVirtioBlk,
        mediated: true,
    });
    emu_dev_config.push(VmEmulatedDeviceConfig {
        name: String::from("virtio_net@a001000"),
        base_ipa: 0xa001000,
        length: 0x1000,
        irq_id: 32 + 0x11,
        cfg_list: vec![0x74, 0x56, 0xaa, 0x0f, 0x47, 0xd2],
        emu_type: EmuDeviceType::EmuDeviceTVirtioNet,
        mediated: false,
    });
    emu_dev_config.push(VmEmulatedDeviceConfig {
        name: String::from("virtio_console@a003000"),
        base_ipa: 0xa003000,
        length: 0x1000,
        irq_id: 32 + 0x12,
        cfg_list: vec![0, 0xa003000],
        emu_type: EmuDeviceType::EmuDeviceTVirtioConsole,
        mediated: false,
    });

    // vm2 passthrough
    let mut pt_dev_config: VmPassthroughDeviceConfig = VmPassthroughDeviceConfig::default();
    pt_dev_config.regions = vec![
        // PassthroughRegion {
        //     ipa: UART_1_ADDR,
        //     pa: UART_1_ADDR,
        //     length: 0x1000,
        //     dev_property: true,
        // },
        PassthroughRegion {
            ipa: 0x8010000,
            pa: Platform::GICV_BASE,
            length: 0x2000,
            dev_property: true,
        },
    ];
    // pt_dev_config.irqs = vec![UART_1_INT, INTERRUPT_IRQ_GUEST_TIMER];
    pt_dev_config.irqs = vec![INTERRUPT_IRQ_GUEST_TIMER];

    // vm2 vm_region
    let mut vm_region: Vec<VmRegion> = Vec::new();
    vm_region.push(VmRegion {
        ipa_start: 0x80000000,
        length: 0x40000000,
    });

    let mut vm_dtb_devs: Vec<VmDtbDevConfig> = vec![];
    vm_dtb_devs.push(VmDtbDevConfig {
        name: String::from("gicd"),
        dev_type: DtbDevType::Gicd,
        irqs: vec![],
        addr_region: VmRegion {
            ipa_start: 0x8000000,
            length: 0x1000,
        },
    });
    vm_dtb_devs.push(VmDtbDevConfig {
        name: String::from("gicc"),
        dev_type: DtbDevType::Gicc,
        irqs: vec![],
        addr_region: VmRegion {
            ipa_start: 0x8010000,
            length: 0x2000,
        },
    });
    // vm_dtb_devs.push(VmDtbDevConfig {
    //     name: String::from("serial"),
    //     dev_type: DtbDevType::DevSerial,
    //     irqs: vec![UART_1_INT],
    //     addr_region: VmRegion {
    //         ipa_start: UART_1_ADDR,
    //         length: 0x1000,
    //     },
    // });

    // vm2 config
    let vm2_config = VmConfigEntry {
        id: 2,
        name: String::from("guest-os-1"),
        os_type: VmType::VmTOs,
        // cmdline: "root=/dev/vda rw audit=0",
        cmdline: String::from("earlycon console=ttyS0,115200n8 root=/dev/vda rw audit=0"),

        image: VmImageConfig {
            kernel_img_name: Some("Image_vanilla"),
            kernel_load_ipa: 0x80080000,
            kernel_entry_point: 0x80080000,
            device_tree_load_ipa: 0x80000000,
            ramdisk_load_ipa: 0, //0x83000000,
        },
        memory: VmMemoryConfig {
            region: vm_region,
            colors: vec![],
            ..Default::default()
        },
        cpu: VmCpuConfig {
            num: 1,
            allocate_bitmap: 0b0100,
            master: Some(2),
        },
        vm_emu_dev_confg: VmEmulatedDeviceConfigList {
            emu_dev_list: emu_dev_config,
        },
        vm_pt_dev_confg: pt_dev_config,
        vm_dtb_devs: VMDtbDevConfigList {
            dtb_device_list: vm_dtb_devs,
        },
        mediated_block_index: Some(1),
    };
    let _ = vm_cfg_add_vm_entry(vm2_config);
}
