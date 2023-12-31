use tock_registers::interfaces::*;
use tock_registers::*;

use crate::arch::{pt_lvl1_idx, pt_lvl2_idx, Address};
use crate::arch::{LVL1_SHIFT, LVL2_SHIFT};
use crate::board::PLAT_DESC;
use crate::mm::_image_end;
use crate::util::round_up;

use super::interface::*;

pub const PLATFORM_PHYSICAL_LIMIT_GB: usize = 16;

register_bitfields! {u64,
    pub PageDescriptorS1 [
        UXN      OFFSET(54) NUMBITS(1) [
            False = 0,
            True = 1
        ],
        PXN      OFFSET(53) NUMBITS(1) [
            False = 0,
            True = 1
        ],
        OUTPUT_PPN OFFSET(12) NUMBITS(36) [], // [47:12]
        AF       OFFSET(10) NUMBITS(1) [
            False = 0,
            True = 1
        ],
        SH       OFFSET(8) NUMBITS(2) [
            OuterShareable = 0b10,
            InnerShareable = 0b11
        ],
        AP       OFFSET(6) NUMBITS(2) [
            RW_ELx = 0b00,
            RW_ELx_EL0 = 0b01,
            RO_ELx = 0b10,
            RO_ELx_EL0 = 0b11
        ],
        AttrIndx OFFSET(2) NUMBITS(3) [
            Attr0 = 0b000,
            Attr1 = 0b001,
            Attr2 = 0b010
        ],
        TYPE     OFFSET(1) NUMBITS(1) [
            Block = 0,
            Table = 1
        ],
        VALID    OFFSET(0) NUMBITS(1) [
            False = 0,
            True = 1
        ]
    ]
}

#[derive(Copy, Clone)]
#[repr(transparent)]
struct BlockDescriptor(u64);

impl BlockDescriptor {
    fn new(output_addr: usize, device: bool) -> BlockDescriptor {
        BlockDescriptor(
            (PageDescriptorS1::OUTPUT_PPN.val((output_addr >> PAGE_SHIFT) as u64)
                + PageDescriptorS1::AF::True
                + PageDescriptorS1::AP::RW_ELx
                + PageDescriptorS1::TYPE::Block
                + PageDescriptorS1::VALID::True
                + if device {
                    PageDescriptorS1::AttrIndx::Attr0 + PageDescriptorS1::SH::OuterShareable
                } else {
                    PageDescriptorS1::AttrIndx::Attr1 + PageDescriptorS1::SH::InnerShareable
                })
            .value,
        )
    }

    fn table(output_addr: usize) -> BlockDescriptor {
        BlockDescriptor(
            (PageDescriptorS1::OUTPUT_PPN.val((output_addr >> PAGE_SHIFT) as u64)
                + PageDescriptorS1::VALID::True
                + PageDescriptorS1::TYPE::Table)
                .value,
        )
    }
    const fn invalid() -> BlockDescriptor {
        BlockDescriptor(0)
    }
}

#[repr(C, align(4096))]
pub struct PageTables {
    entry: [BlockDescriptor; ENTRY_PER_PAGE],
}

pub static mut LVL1_PAGE_TABLE: PageTables = PageTables {
    entry: [BlockDescriptor(0); ENTRY_PER_PAGE],
};
pub static mut LVL2_PAGE_TABLE: PageTables = PageTables {
    entry: [BlockDescriptor(0); ENTRY_PER_PAGE],
};

pub fn pt_populate(lvl1_pt: &mut PageTables, lvl2_pt: &mut PageTables) {
    let lvl2_base = lvl2_pt as *const _ as usize;
    let image_end_align_gb = round_up(_image_end as usize, 1 << LVL1_SHIFT);

    if cfg!(feature = "tx2") {
        // Name                         Address Range
        // Always DRAM (2G – 16G)       0x0_8000_0000 – 0x3_FFFF_FFFF
        // Reclaimable PCIe (1G – 2G)   0x0_4000_0000 – 0x7FFF_FFFF
        // Always SysRAM (0.75G – 1.0G) 0x0_3000_0000 – 0x0_3FFF_FFFF
        // RESERVED (0.5G – 0.75G)      0x0_2000_0000 – 0x0_2FFF_FFFF
        // Always MMIO (0.0G – 0.5G)    0x0_0000_0000 – 0x1FFF_FFFF
        for i in 0..PLATFORM_PHYSICAL_LIMIT_GB {
            let output_addr = i << LVL1_SHIFT;
            lvl1_pt.entry[i] = if (PLAT_DESC.mem_desc.base..image_end_align_gb).contains(&output_addr) {
                BlockDescriptor::new(output_addr, false)
            } else {
                BlockDescriptor::invalid()
            }
        }
        // for i in PLATFORM_PHYSICAL_LIMIT_GB..ENTRY_PER_PAGE {
        //     pt.lvl1[i] = BlockDescriptor::invalid();
        // }

        lvl1_pt.entry[pt_lvl1_idx(0)] = BlockDescriptor::table(lvl2_base);
        // 0x200000 ~ 2MB
        // UART0 ~ 0x3000000 - 0x3200000 (0x3100000)
        // UART1 ~ 0xc200000 - 0xc400000 (0xc280000)
        // EMMC ~ 0x3400000 - 0x3600000 (0x3460000)
        // GIC  ~ 0x3800000 - 0x3a00000 (0x3881000)
        // SMMU ~ 0x12000000 - 0x13000000
        lvl2_pt.entry[pt_lvl2_idx(0x3000000)] = BlockDescriptor::new(0x3000000, true);
        lvl2_pt.entry[pt_lvl2_idx(0xc200000)] = BlockDescriptor::new(0xc200000, true);
        // lvl2_pt.lvl1[pt_lvl2_idx(0x3400000)] = BlockDescriptor::new(0x3400000, true);
        lvl2_pt.entry[pt_lvl2_idx(0x3800000)] = BlockDescriptor::new(0x3800000, true);
        for addr in (0x12000000..0x13000000).step_by(1 << LVL2_SHIFT) {
            lvl2_pt.entry[pt_lvl2_idx(addr)] = BlockDescriptor::new(addr, true);
        }
    } else if cfg!(feature = "pi4") {
        // TODO: image_end_align_gb to map va
        // 0x0_0000_0000 ~ 0x0_c000_0000 --> normal memory (3GB)
        let normal_memory_0 = 0x0_0000_0000..0x0_c000_0000;
        for (i, pa) in normal_memory_0.step_by(1 << LVL1_SHIFT).enumerate() {
            lvl1_pt.entry[i] = BlockDescriptor::new(pa, false);
        }
        // 0x0_c000_0000 ~ 0x0_fc00_0000 --> normal memory (960MB)
        let normal_memory_1 = 0x0_c000_0000..0x0_fc00_0000;
        lvl1_pt.entry[pt_lvl1_idx(normal_memory_1.start)] = BlockDescriptor::table(lvl2_base);
        for (i, pa) in normal_memory_1.step_by(1 << LVL2_SHIFT).enumerate() {
            lvl2_pt.entry[i] = BlockDescriptor::new(pa, false);
        }
        // 0x0_fc00_0000 ~ 0x1_0000_0000 --> device memory (64MB)
        let device_memory = 0x0_fc00_0000..0x1_0000_0000;
        for (i, pa) in device_memory.step_by(1 << LVL2_SHIFT).enumerate() {
            lvl2_pt.entry[i] = BlockDescriptor::new(pa, true);
        }
        // 0x1_0000_0000 ~ 0x2_0000_0000 --> normal memory (4GB)
        let normal_memory_2 = 0x1_0000_0000..0x2_0000_0000;
        let invalid_start = normal_memory_2.end;
        for (i, pa) in normal_memory_2.step_by(1 << LVL1_SHIFT).enumerate() {
            lvl1_pt.entry[i] = BlockDescriptor::new(pa, false);
        }
        for i in pt_lvl1_idx(invalid_start)..512 {
            lvl1_pt.entry[i] = BlockDescriptor::invalid();
        }
    } else if cfg!(feature = "qemu") {
        for index in 0..PLATFORM_PHYSICAL_LIMIT_GB {
            let pa = index << LVL1_SHIFT;
            lvl1_pt.entry[index] = if pa < PLAT_DESC.mem_desc.base {
                BlockDescriptor::new(pa, true)
            } else if (PLAT_DESC.mem_desc.base..image_end_align_gb).contains(&pa) {
                BlockDescriptor::new(pa, false)
            } else {
                BlockDescriptor::invalid()
            };
        }
        lvl1_pt.entry[pt_lvl1_idx(0)] = BlockDescriptor::table(lvl2_base);
        for (index, pa) in (0..PLAT_DESC.mem_desc.base)
            .step_by(1 << LVL2_SHIFT)
            .take(PTE_PER_PAGE)
            .enumerate()
        {
            lvl2_pt.entry[index] = BlockDescriptor::new(pa, true);
        }
    }

    // map pa to hva
    for i in 0..PLATFORM_PHYSICAL_LIMIT_GB {
        let pa = i << LVL1_SHIFT;
        lvl1_pt.entry[pt_lvl1_idx(pa.pa2hva())] = BlockDescriptor::new(pa, false);
    }
}

const PA_RANGE_TABLE: &[u64] = &[32, 36, 40, 42, 44, 48, 52];

pub fn pa_range() -> u64 {
    use aarch64_cpu::registers::ID_AA64MMFR0_EL1;
    // current only support 3-level page table (39bits), so the max is 36bits and max index is 1
    ID_AA64MMFR0_EL1.read(ID_AA64MMFR0_EL1::PARange).min(1)
}

pub fn pa_range_val(pa_range_idx: usize) -> u64 {
    PA_RANGE_TABLE[pa_range_idx]
}

pub fn mmu_init(pt: &PageTables) {
    use aarch64_cpu::{asm::barrier, registers::*};
    MAIR_EL2.write(
        MAIR_EL2::Attr0_Device::nonGathering_nonReordering_noEarlyWriteAck
            + MAIR_EL2::Attr1_Normal_Outer::WriteBack_NonTransient_ReadWriteAlloc
            + MAIR_EL2::Attr1_Normal_Inner::WriteBack_NonTransient_ReadWriteAlloc
            + MAIR_EL2::Attr2_Normal_Outer::NonCacheable
            + MAIR_EL2::Attr2_Normal_Inner::NonCacheable,
    );
    TTBR0_EL2.set(&pt.entry as *const _ as u64);

    TCR_EL2.write(
        TCR_EL2::PS::Bits_48
            + TCR_EL2::SH0::Inner
            + TCR_EL2::TG0::KiB_4
            + TCR_EL2::ORGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable
            + TCR_EL2::IRGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable
            + TCR_EL2::T0SZ.val(64 - HYP_VA_SIZE),
    );

    let pa_range_idx = pa_range();
    let pa_range = pa_range_val(pa_range_idx as usize);

    VTCR_EL2.write(
        VTCR_EL2::PS.val(pa_range_idx)
            + VTCR_EL2::TG0::Granule4KB
            + VTCR_EL2::SH0::Inner
            + VTCR_EL2::ORGN0::NormalWBRAWA
            + VTCR_EL2::IRGN0::NormalWBRAWA
            + VTCR_EL2::SL0.val(if pa_range < 44 { 0b01 } else { 0b10 })
            + VTCR_EL2::T0SZ.val(64 - pa_range),
    );

    barrier::isb(barrier::SY);
}
