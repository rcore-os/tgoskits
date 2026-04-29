//! A SD Card driver for cv181x-sd device

use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use sg200x_bsp::sdmmc::Sdmmc;

use crate::BlockDriverOps;

const BLOCK_SIZE: usize = 512;

/// CVSD driver based on SG200x BSP SD/MMC.
pub struct CvsdDriver(Sdmmc);

unsafe impl Send for CvsdDriver {}
unsafe impl Sync for CvsdDriver {}

impl CvsdDriver {
    /// Initializes SD/MMC and creates a new [`CvsdDriver`].
    pub fn new(sdmmc: usize, syscon: usize) -> DevResult<Self> {
        let sdmmc = unsafe { Sdmmc::from_base_addresses(sdmmc, syscon) };
        sdmmc.init().map_err(|_| DevError::Io)?;
        sdmmc.clk_en(true);
        Ok(Self(sdmmc))
    }
}

impl BaseDriverOps for CvsdDriver {
    fn device_type(&self) -> DeviceType {
        DeviceType::Block
    }

    fn device_name(&self) -> &str {
        "cvsd"
    }
}

impl BlockDriverOps for CvsdDriver {
    fn num_blocks(&self) -> u64 {
        // Capacity info is not exposed by sg200x-bsp sdmmc yet.
        67108864 // Fake capacity info: 32G
    }

    fn block_size(&self) -> usize {
        BLOCK_SIZE
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> DevResult {
        let (blocks, remainder) = buf.as_chunks_mut::<{ BLOCK_SIZE }>();

        if !remainder.is_empty() {
            return Err(DevError::InvalidParam);
        }

        for (i, block) in blocks.iter_mut().enumerate() {
            self.0
                .read_block(block_id as u32 + i as u32, block)
                .map_err(|_| DevError::Io)?;
        }

        Ok(())
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> DevResult {
        let (blocks, remainder) = buf.as_chunks::<{ BLOCK_SIZE }>();

        if !remainder.is_empty() {
            return Err(DevError::InvalidParam);
        }

        for (i, block) in blocks.iter().enumerate() {
            self.0
                .write_block(block_id as u32 + i as u32, block)
                .map_err(|_| DevError::Io)?;
        }

        Ok(())
    }

    fn flush(&mut self) -> DevResult {
        Ok(())
    }
}
