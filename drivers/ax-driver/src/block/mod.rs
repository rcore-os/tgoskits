mod binding;
#[cfg(any(
    feature = "irq",
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
    BlkError, BuffConfig, DriverGeneric, IReadQueue, IWriteQueue, Interface, QueueInfo, RequestId,
    RequestRead, RequestStatus, RequestWrite,
};
#[cfg(any(
    feature = "irq",
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
    read_queue_created: bool,
    write_queue_created: bool,
}

#[cfg(sync_block_dev)]
impl<D> SyncBlockDevice<D> {
    fn new(driver: D) -> Self {
        Self {
            inner: Arc::new(Mutex::new(driver)),
            read_queue_created: false,
            write_queue_created: false,
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
    fn create_read_queue(&mut self) -> Option<Box<dyn IReadQueue>> {
        if self.read_queue_created {
            return None;
        }
        self.read_queue_created = true;
        Some(Box::new(SyncBlockReadQueue {
            id: 0,
            inner: Arc::clone(&self.inner),
        }))
    }

    fn create_write_queue(&mut self) -> Option<Box<dyn IWriteQueue>> {
        if self.write_queue_created {
            return None;
        }
        self.write_queue_created = true;
        Some(Box::new(SyncBlockWriteQueue {
            id: 0,
            inner: Arc::clone(&self.inner),
        }))
    }
}

#[cfg(sync_block_dev)]
struct SyncBlockReadQueue<D> {
    id: usize,
    inner: Arc<Mutex<D>>,
}

#[cfg(sync_block_dev)]
struct SyncBlockWriteQueue<D> {
    id: usize,
    inner: Arc<Mutex<D>>,
}

#[cfg(sync_block_dev)]
impl<D: SyncBlockOps> QueueInfo for SyncBlockReadQueue<D> {
    fn id(&self) -> usize {
        self.id
    }

    fn num_blocks(&self) -> usize {
        self.inner.lock().num_blocks() as usize
    }

    fn block_size(&self) -> usize {
        self.inner.lock().block_size()
    }

    fn buffer_config(&self) -> BuffConfig {
        BuffConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            size: self.block_size(),
        }
    }
}

#[cfg(sync_block_dev)]
impl<D: SyncBlockOps> IReadQueue for SyncBlockReadQueue<D> {
    fn submit_read(&mut self, mut request: RequestRead<'_>) -> Result<RequestId, BlkError> {
        self.inner
            .lock()
            .read_blocks(request.block_id as u64, &mut request.buffer)?;
        Ok(RequestId::new(0))
    }

    fn poll_read(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
        Ok(RequestStatus::Complete)
    }
}

#[cfg(sync_block_dev)]
impl<D: SyncBlockOps> QueueInfo for SyncBlockWriteQueue<D> {
    fn id(&self) -> usize {
        self.id
    }

    fn num_blocks(&self) -> usize {
        self.inner.lock().num_blocks() as usize
    }

    fn block_size(&self) -> usize {
        self.inner.lock().block_size()
    }

    fn buffer_config(&self) -> BuffConfig {
        BuffConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            size: self.block_size(),
        }
    }
}

#[cfg(sync_block_dev)]
impl<D: SyncBlockOps> IWriteQueue for SyncBlockWriteQueue<D> {
    fn submit_write(&mut self, request: RequestWrite<'_>) -> Result<RequestId, BlkError> {
        self.inner
            .lock()
            .write_blocks(request.block_id as u64, &request.buffer)?;
        Ok(RequestId::new(0))
    }

    fn poll_write(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
        Ok(RequestStatus::Complete)
    }
}
