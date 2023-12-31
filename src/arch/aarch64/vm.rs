use crate::kernel::{IntCtrlType, Vm};

impl Vm {
    // TODO: move to ArchVm
    pub fn init_intc_mode(&self, intc_type: IntCtrlType) {
        use super::{GICC_CTLR_EN_BIT, GICC_CTLR_EOIMODENS_BIT};
        use aarch64_cpu::registers::HCR_EL2;

        let (gich_ctlr, hcr) = match intc_type {
            IntCtrlType::Emulated => (
                (GICC_CTLR_EN_BIT | GICC_CTLR_EOIMODENS_BIT) as u32,
                (HCR_EL2::VM::Enable
                    + HCR_EL2::RW::EL1IsAarch64
                    + HCR_EL2::IMO::EnableVirtualIRQ
                    + HCR_EL2::FMO::EnableVirtualFIQ
                    + HCR_EL2::TSC::EnableTrapEl1SmcToEl2)
                    .value,
            ),
            #[cfg(not(feature = "memory-reservation"))]
            IntCtrlType::Passthrough => (
                GICC_CTLR_EN_BIT as u32,
                (HCR_EL2::VM::Enable + HCR_EL2::RW::EL1IsAarch64 + HCR_EL2::TSC::EnableTrapEl1SmcToEl2).value,
            ),
        };
        // hcr |= 1 << 17; // set HCR_EL2.TID2=1, trap for cache id sysregs
        cfg_if::cfg_if! {
            if #[cfg(feature = "trap-wfi")] {
                const HCR_EL2_TWI: u64 = 1 << 13;
                let hcr = hcr | HCR_EL2_TWI;
            }
        }
        for vcpu in self.vcpu_list() {
            debug!("vm {} vcpu {} set {:?} hcr", self.id(), vcpu.id(), intc_type);
            vcpu.set_gich_ctlr(gich_ctlr);
            vcpu.set_hcr(hcr);
        }
    }
}
