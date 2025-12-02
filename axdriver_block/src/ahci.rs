//! AHCI driver.

use axdriver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use simple_ahci::{AhciDriver as SimpleAhciDriver, Hal};

use crate::BlockDriverOps;

/// AHCI driver based on the `simple_ahci` crate.
pub struct AhciDriver<H: Hal>(SimpleAhciDriver<H>);

impl<H: Hal> AhciDriver<H> {
    /// Try to construct a new AHCI driver from the given MMIO base address.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - `base` is a valid virtual address pointing to the AHCI controller's MMIO register block.
    /// - The memory region starting at `base` is properly mapped and accessible.
    /// - No other code is concurrently accessing the same AHCI controller.
    /// - The AHCI controller hardware is present and functional at the given address.
    pub unsafe fn try_new(base: usize) -> Option<Self> {
        unsafe { SimpleAhciDriver::<H>::try_new(base) }.map(AhciDriver)
    }
}

impl<H: Hal> BaseDriverOps for AhciDriver<H> {
    fn device_name(&self) -> &str {
        "ahci"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Block
    }
}

impl<H: Hal> BlockDriverOps for AhciDriver<H> {
    fn block_size(&self) -> usize {
        self.0.block_size()
    }

    fn num_blocks(&self) -> u64 {
        self.0.capacity()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> DevResult {
        if buf.len() % self.block_size() != 0 {
            return Err(DevError::InvalidParam);
        }
        if buf.as_ptr() as usize % 4 != 0 {
            return Err(DevError::InvalidParam);
        }
        if self.0.read(block_id, buf) {
            Ok(())
        } else {
            Err(DevError::Io)
        }
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> DevResult {
        if buf.len() % self.block_size() != 0 {
            return Err(DevError::InvalidParam);
        }
        if buf.as_ptr() as usize % 4 != 0 {
            return Err(DevError::InvalidParam);
        }
        if self.0.write(block_id, buf) {
            Ok(())
        } else {
            Err(DevError::Io)
        }
    }

    fn flush(&mut self) -> DevResult {
        Ok(())
    }
}
