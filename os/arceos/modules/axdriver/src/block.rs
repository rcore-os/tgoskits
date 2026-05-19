extern crate alloc;

use alloc::boxed::Box;

use ax_driver_base::{BaseDriverOps, DevError};
use ax_driver_block::BlockDriverOps;
use rd_block::{BlkError, BuffConfig, IQueue, Request, RequestId, RequestKind};
use rdrive::{DriverGeneric, PlatformDevice};

#[cfg(feature = "ahci")]
pub mod ahci;
#[cfg(feature = "bcm2835-sdhci")]
pub mod bcm2835;
#[cfg(feature = "cvsd")]
pub mod cvsd;
#[cfg(feature = "ramdisk")]
pub mod ramdisk;
#[cfg(feature = "sdmmc")]
pub mod sdmmc;

pub fn register_block<D>(plat_dev: PlatformDevice, driver: D)
where
    D: BlockDriverOps + 'static,
{
    let block = rd_block::Block::new(LegacyBlockDevice::new(driver), axklib::dma::op());
    plat_dev.register(block);
}

struct LegacyBlockDevice<D> {
    driver: Option<D>,
    irq_enabled: bool,
}

impl<D> LegacyBlockDevice<D> {
    fn new(driver: D) -> Self {
        Self {
            driver: Some(driver),
            irq_enabled: false,
        }
    }
}

struct LegacyBlockQueue<D> {
    driver: D,
}

impl<D: BlockDriverOps + 'static> DriverGeneric for LegacyBlockDevice<D> {
    fn name(&self) -> &str {
        self.driver
            .as_ref()
            .map(BaseDriverOps::device_name)
            .unwrap_or("legacy-block")
    }
}

impl<D: BlockDriverOps + 'static> rd_block::Interface for LegacyBlockDevice<D> {
    fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
        self.driver
            .take()
            .map(|driver| Box::new(LegacyBlockQueue { driver }) as Box<dyn IQueue>)
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

    fn handle_irq(&mut self) -> rd_block::Event {
        rd_block::Event::none()
    }
}

impl<D: BlockDriverOps + 'static> IQueue for LegacyBlockQueue<D> {
    fn id(&self) -> usize {
        0
    }

    fn num_blocks(&self) -> usize {
        self.driver.num_blocks() as usize
    }

    fn block_size(&self) -> usize {
        self.driver.block_size()
    }

    fn buff_config(&self) -> BuffConfig {
        BuffConfig {
            dma_mask: u64::MAX,
            align: self.block_size().max(1),
            size: self.block_size().max(1),
        }
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        match request.kind {
            RequestKind::Read(mut buffer) => self
                .driver
                .read_block(request.block_id as _, &mut buffer)
                .map_err(map_block_error)?,
            RequestKind::Write(data) => self
                .driver
                .write_block(request.block_id as _, data)
                .map_err(map_block_error)?,
        }
        Ok(RequestId::new(0))
    }

    fn poll_request(&mut self, _request: RequestId) -> Result<(), BlkError> {
        Ok(())
    }
}

fn map_block_error(err: DevError) -> BlkError {
    match err {
        DevError::Again | DevError::ResourceBusy => BlkError::Retry,
        DevError::NoMemory => BlkError::NoMemory,
        DevError::Unsupported => BlkError::NotSupported,
        DevError::InvalidParam => BlkError::InvalidBlockIndex(0),
        _ => BlkError::Other(Box::new(rd_block::KError::Unknown("legacy block error"))),
    }
}
