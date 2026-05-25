mod binding;

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
use rd_block::{BlkError, BuffConfig, DriverGeneric, Event, IQueue, Interface, Request, RequestId};
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
    irq_enabled: bool,
}

#[cfg(sync_block_dev)]
impl<D> SyncBlockDevice<D> {
    fn new(driver: D) -> Self {
        Self {
            inner: Arc::new(Mutex::new(driver)),
            queue_created: false,
            irq_enabled: false,
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

    fn enable_irq(&mut self) {
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        Event::none()
    }
}

#[cfg(sync_block_dev)]
struct SyncBlockQueue<D> {
    id: usize,
    inner: Arc<Mutex<D>>,
}

#[cfg(sync_block_dev)]
impl<D: SyncBlockOps> IQueue for SyncBlockQueue<D> {
    fn id(&self) -> usize {
        self.id
    }

    fn num_blocks(&self) -> usize {
        self.inner.lock().num_blocks() as usize
    }

    fn block_size(&self) -> usize {
        self.inner.lock().block_size()
    }

    fn buff_config(&self) -> BuffConfig {
        BuffConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            size: self.block_size(),
        }
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        match request.kind {
            rd_block::RequestKind::Read(mut buffer) => self
                .inner
                .lock()
                .read_blocks(request.block_id as u64, &mut buffer)?,
            rd_block::RequestKind::Write(items) => self
                .inner
                .lock()
                .write_blocks(request.block_id as u64, items)?,
        }
        Ok(RequestId::new(0))
    }

    fn poll_request(&mut self, _request: RequestId) -> Result<(), BlkError> {
        Ok(())
    }
}
