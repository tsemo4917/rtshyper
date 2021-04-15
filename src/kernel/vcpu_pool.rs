use crate::kernel::{Vcpu, VcpuState};
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

pub const VCPU_POOL_MAX: usize = 4;

pub struct VcpuContent {
    pub vcpu: Arc<Mutex<Vcpu>>,
}

pub struct VcpuPool {
    pub content: Vec<VcpuContent>,
    pub active_idx: usize,
    pub running: usize,
}

impl VcpuPool {
    fn default() -> VcpuPool {
        VcpuPool {
            content: Vec::new(),
            active_idx: 0,
            running: 0,
        }
    }

    fn append_vcpu(&mut self, vcpu: Arc<Mutex<Vcpu>>) {
        self.content.push(VcpuContent { vcpu });
        self.running += 1;
    }
}

use crate::kernel::{set_cpu_vcpu_pool, CPU};
pub fn vcpu_pool_init() {
    set_cpu_vcpu_pool(Box::new(VcpuPool::default()));
}

pub fn vcpu_pool_append(vcpu: Arc<Mutex<Vcpu>>) -> bool {
    if let Some(vcpu_pool) = unsafe { &mut CPU.vcpu_pool } {
        if vcpu_pool.content.len() >= VCPU_POOL_MAX {
            println!("can't append more vcpu!");
            return false;
        }
        let mut vcpu_lock = vcpu.lock();
        vcpu_lock.state = VcpuState::VcpuPend;
        drop(vcpu_lock);

        vcpu_pool.append_vcpu(vcpu.clone());
    } else {
        panic!("CPU's vcpu pool is NULL");
    }
    true
}
