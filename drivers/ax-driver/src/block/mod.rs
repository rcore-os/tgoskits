mod binding;
#[cfg(any(
    feature = "virtio-blk",
    feature = "phytium-mci",
    feature = "rockchip-dwmmc",
    feature = "rockchip-sdhci"
))]
mod shared;

#[cfg(feature = "ahci")]
pub mod ahci;
#[cfg(feature = "bcm2835-sdhci")]
pub mod bcm2835;
#[cfg(feature = "cvsd")]
pub mod cvsd;
#[cfg(feature = "nvme")]
pub mod nvme;
#[cfg(feature = "phytium-mci")]
pub mod phytium_mci;
#[cfg(feature = "ramdisk")]
pub mod ramdisk;
#[cfg(feature = "rockchip-sdhci")]
mod rockchip;
#[cfg(feature = "rockchip-sdhci")]
pub mod rockchip_mmc;
#[cfg(feature = "rockchip-dwmmc")]
pub mod rockchip_sd;

#[cfg(sync_block_dev)]
use alloc::{boxed::Box, sync::Arc};

pub use binding::*;
#[cfg(sync_block_dev)]
use rdif_block::{
    BlkError, DeviceInfo, DriverGeneric, IQueue, Interface, QueueConfig, QueueInfo, QueueLimits,
    QueueMode, QueueTopology, Request, RequestId, RequestOp, RequestStatus, validate_request_shape,
};
#[cfg(any(
    feature = "virtio-blk",
    feature = "phytium-mci",
    feature = "rockchip-dwmmc",
    feature = "rockchip-sdhci"
))]
pub(crate) use shared::SharedDriver;
#[cfg(sync_block_dev)]
use spin::Mutex;

#[cfg(sync_block_dev)]
pub(crate) trait SyncBlockOps: Send + 'static {
    fn name(&self) -> &'static str;
    fn num_blocks(&self) -> u64;
    fn block_size(&self) -> usize;
    fn read_blocks(&mut self, block_id: u64, buf: &mut [u8]) -> Result<(), BlkError>;
    fn write_blocks(&mut self, block_id: u64, buf: &[u8]) -> Result<(), BlkError>;
}

#[cfg(sync_block_dev)]
pub(crate) fn register_sync_block<D: SyncBlockOps>(plat_dev: rdrive::PlatformDevice, driver: D) {
    plat_dev.register_block(SyncBlockDevice::new(driver));
}

#[cfg(sync_block_dev)]
struct SyncBlockDevice<D> {
    inner: Arc<Mutex<D>>,
    queue_created: bool,
}

#[cfg(sync_block_dev)]
impl<D> SyncBlockDevice<D> {
    fn new(driver: D) -> Self {
        Self {
            inner: Arc::new(Mutex::new(driver)),
            queue_created: false,
        }
    }
}

#[cfg(sync_block_dev)]
impl<D: SyncBlockOps> DriverGeneric for SyncBlockDevice<D> {
    fn name(&self) -> &str {
        self.inner.lock().name()
    }
}

#[cfg(sync_block_dev)]
impl<D: SyncBlockOps> Interface for SyncBlockDevice<D> {
    fn device_info(&self) -> DeviceInfo {
        let guard = self.inner.lock();
        DeviceInfo {
            name: Some(guard.name()),
            ..DeviceInfo::new(guard.num_blocks(), guard.block_size())
        }
    }

    fn queue_limits(&self) -> QueueLimits {
        QueueLimits::simple(self.inner.lock().block_size(), u64::MAX)
    }

    fn queue_topology(&self) -> QueueTopology {
        QueueTopology::single(1)
    }

    fn create_queue(&mut self, config: QueueConfig) -> Option<Box<dyn IQueue>> {
        if self.queue_created {
            return None;
        }
        self.queue_created = true;
        Some(Box::new(SyncBlockQueue {
            id: config.id_hint.unwrap_or(0),
            depth: config.depth.max(1),
            mode: config.mode,
            inner: Arc::clone(&self.inner),
        }))
    }
}

#[cfg(sync_block_dev)]
struct SyncBlockQueue<D> {
    id: usize,
    depth: usize,
    mode: QueueMode,
    inner: Arc<Mutex<D>>,
}

#[cfg(sync_block_dev)]
impl<D: SyncBlockOps> SyncBlockQueue<D> {
    fn device_info(&self) -> DeviceInfo {
        let guard = self.inner.lock();
        DeviceInfo {
            name: Some(guard.name()),
            ..DeviceInfo::new(guard.num_blocks(), guard.block_size())
        }
    }

    fn limits(&self) -> QueueLimits {
        QueueLimits::simple(self.inner.lock().block_size(), u64::MAX)
    }
}

#[cfg(sync_block_dev)]
impl<D: SyncBlockOps> IQueue for SyncBlockQueue<D> {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> QueueInfo {
        QueueInfo {
            id: self.id,
            depth: self.depth,
            mode: self.mode,
            device: self.device_info(),
            limits: self.limits(),
        }
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        let info = self.device_info();
        validate_request_shape(info, self.limits(), &request)?;
        match request.op {
            RequestOp::Read => {
                let segment = request
                    .segments
                    .first_mut()
                    .ok_or(BlkError::InvalidRequest)?;
                self.inner.lock().read_blocks(request.lba, segment)?;
            }
            RequestOp::Write => {
                let segment = request.segments.first().ok_or(BlkError::InvalidRequest)?;
                self.inner.lock().write_blocks(request.lba, segment)?;
            }
            RequestOp::Flush => {}
            RequestOp::Discard | RequestOp::WriteZeroes => return Err(BlkError::NotSupported),
        }
        Ok(RequestId::new(0))
    }

    fn poll_request(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
        Ok(RequestStatus::Complete)
    }
}
