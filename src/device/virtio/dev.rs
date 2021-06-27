use crate::config::VmEmulatedDeviceConfig;
use crate::device::{BlkDesc, VirtioBlkReq, BLOCKIF_IOV_MAX};
use crate::mm::PageFrame;
use alloc::sync::Arc;
use spin::Mutex;

use crate::device::{VIRTIO_BLK_F_SEG_MAX, VIRTIO_BLK_F_SIZE_MAX, VIRTIO_F_VERSION_1};

#[derive(Copy, Clone)]
pub enum VirtioDeviceType {
    None = 0,
    Net = 1,
    Block = 2,
}

use crate::device::BlkStat;
#[derive(Clone)]
pub enum DevStat {
    BlkStat(BlkStat),
    NetStat(),
    None,
}

#[derive(Clone)]
pub enum DevDesc {
    BlkDesc(BlkDesc),
    None,
}

#[derive(Clone)]
pub enum DevReq {
    BlkReq(VirtioBlkReq),
    None,
}

#[derive(Clone)]
pub struct VirtDev {
    inner: Arc<Mutex<VirtDevInner>>,
}

impl VirtDev {
    pub fn default() -> VirtDev {
        VirtDev {
            inner: Arc::new(Mutex::new(VirtDevInner::default())),
        }
    }

    pub fn init(&self, dev_type: VirtioDeviceType, config: &VmEmulatedDeviceConfig) {
        let mut inner = self.inner.lock();
        inner.init(dev_type, config);
    }

    pub fn features(&self) -> usize {
        let inner = self.inner.lock();
        inner.features
    }

    pub fn generation(&self) -> usize {
        let inner = self.inner.lock();
        inner.generation
    }

    pub fn desc(&self) -> DevDesc {
        let inner = self.inner.lock();
        inner.desc.clone()
    }

    pub fn req(&self) -> DevReq {
        let inner = self.inner.lock();
        inner.req.clone()
    }

    pub fn int_id(&self) -> usize {
        let inner = self.inner.lock();
        inner.int_id
    }

    pub fn cache(&self) -> PageFrame {
        let inner = self.inner.lock();
        return inner.cache.as_ref().unwrap().clone();
    }

    pub fn stat(&self) -> DevStat {
        let inner = self.inner.lock();
        inner.stat.clone()
    }

    pub fn set_activated(&self, activated: bool) {
        let mut inner = self.inner.lock();
        inner.activated = activated;
    }
}

pub struct VirtDevInner {
    activated: bool,
    dev_type: VirtioDeviceType,
    features: usize,
    generation: usize,
    int_id: usize,
    desc: DevDesc,
    req: DevReq,
    cache: Option<PageFrame>,
    stat: DevStat,
}

use crate::kernel::mem_pages_alloc;
impl VirtDevInner {
    pub fn default() -> VirtDevInner {
        VirtDevInner {
            activated: false,
            dev_type: VirtioDeviceType::None,
            features: 0,
            generation: 0,
            int_id: 0,
            desc: DevDesc::None,
            req: DevReq::None,
            cache: None,
            stat: DevStat::None,
        }
    }

    // virtio_dev_init
    pub fn init(&mut self, dev_type: VirtioDeviceType, config: &VmEmulatedDeviceConfig) {
        self.dev_type = dev_type;
        self.int_id = config.irq_id;

        match self.dev_type {
            VirtioDeviceType::Block => {
                let blk_desc = BlkDesc::default();
                blk_desc.cfg_init(config.cfg_list[1]);
                self.desc = DevDesc::BlkDesc(blk_desc);

                // TODO: blk_features_init & cache init
                self.features |= VIRTIO_BLK_F_SIZE_MAX | VIRTIO_BLK_F_SEG_MAX | VIRTIO_F_VERSION_1;

                let blk_req = VirtioBlkReq::default();
                blk_req.set_start(config.cfg_list[0]);
                blk_req.set_size(config.cfg_list[1]);
                self.req = DevReq::BlkReq(blk_req);

                match mem_pages_alloc(BLOCKIF_IOV_MAX) {
                    Ok(PageFrame) => {
                        self.cache = Some(PageFrame);
                    }
                    Err(_) => {
                        println!("VirtDevInner::init(): mem_pages_alloc failed");
                    }
                }

                self.stat = DevStat::BlkStat(BlkStat::default())
            }
            _ => {
                panic!("ERROR: Wrong virtio device type");
            }
        }
    }
}
