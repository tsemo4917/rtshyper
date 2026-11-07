use crate::kernel::Vcpu;

impl Vcpu {
    pub fn set_gich_ctlr(&self, ctlr: u32) {
        let mut inner = self.0.inner_mut.lock();
        inner.intc_ctx.ctlr = ctlr;
    }

    pub fn set_hcr(&self, hcr: u64) {
        let mut inner = self.0.inner_mut.lock();
        inner.vm_ctx.hcr_el2 = hcr;
    }

    pub(crate) fn save_fpsimd(&self) {
        self.0.inner_mut.lock().vm_ctx.save_fpsimd();
    }

    pub(crate) fn restore_fpsimd(&self) {
        self.0.inner_mut.lock().vm_ctx.restore_fpsimd();
    }
}
