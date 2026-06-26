mod binding;

#[allow(unused)]
mod shared;

#[cfg(feature = "ahci")]
pub mod ahci;
#[cfg(feature = "bcm2835-sdhci")]
pub mod bcm2835;
#[cfg(feature = "cvsd")]
pub mod cvsd;
#[cfg(feature = "k230-sdhci")]
pub mod k230_sdhci;
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
#[cfg(feature = "starfive-jh7110-dwmmc")]
pub mod starfive_mmc;

#[cfg(sync_block_dev)]
use alloc::{boxed::Box, sync::Arc};

pub use binding::*;
#[cfg(sync_block_dev)]
use rdif_block::{
    BlkError, DeviceInfo, DriverGeneric, IQueue, Interface, QueueInfo, QueueLimits, Request,
    RequestId, RequestOp, RequestStatus, validate_request,
};
#[allow(unused)]
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

    fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
        if self.queue_created {
            return None;
        }
        self.queue_created = true;
        Some(Box::new(SyncBlockQueue {
            id: 0,
            inner: Arc::clone(&self.inner),
        }))
    }
}

#[cfg(sync_block_dev)]
struct SyncBlockQueue<D> {
    id: usize,
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
// SAFETY: SyncBlockQueue forwards data to a synchronous block driver and does
// not retain request segment pointers after `submit_request` returns.
unsafe impl<D: SyncBlockOps> IQueue for SyncBlockQueue<D> {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> QueueInfo {
        QueueInfo {
            id: self.id,
            device: self.device_info(),
            limits: self.limits(),
        }
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        validate_request(self.info(), &request)?;
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
