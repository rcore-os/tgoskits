//! A RAM disk driver backed by a static slice.

use core::ops::{Deref, DerefMut};

use axdriver_base::{BaseDriverOps, DevError, DevResult, DeviceType};

use crate::BlockDriverOps;

const BLOCK_SIZE: usize = 512;

/// A RAM disk backed by a static slice.
#[derive(Default)]
pub struct RamDisk(&'static mut [u8]);

impl RamDisk {
    /// Creates a new RAM disk from the given static buffer.
    ///
    /// # Panics
    /// Panics if the buffer is not aligned to block size or its size is not
    /// a multiple of block size.
    pub fn new(buf: &'static mut [u8]) -> Self {
        assert!(buf.as_ptr().addr() & (BLOCK_SIZE - 1) == 0);
        assert!(buf.len() % BLOCK_SIZE == 0);
        RamDisk(buf)
    }
}

impl Deref for RamDisk {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl DerefMut for RamDisk {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0
    }
}

impl BaseDriverOps for RamDisk {
    fn device_type(&self) -> DeviceType {
        DeviceType::Block
    }

    fn device_name(&self) -> &str {
        "ramdisk"
    }
}

impl BlockDriverOps for RamDisk {
    #[inline]
    fn num_blocks(&self) -> u64 {
        (self.len() / BLOCK_SIZE) as u64
    }

    #[inline]
    fn block_size(&self) -> usize {
        BLOCK_SIZE
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> DevResult {
        if buf.len() % BLOCK_SIZE != 0 {
            return Err(DevError::InvalidParam);
        }
        let offset = block_id as usize * BLOCK_SIZE;
        if offset + buf.len() > self.len() {
            return Err(DevError::Io);
        }
        buf.copy_from_slice(&self[offset..offset + buf.len()]);
        Ok(())
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> DevResult {
        if buf.len() % BLOCK_SIZE != 0 {
            return Err(DevError::InvalidParam);
        }
        let offset = block_id as usize * BLOCK_SIZE;
        if offset + buf.len() > self.len() {
            return Err(DevError::Io);
        }
        self[offset..offset + buf.len()].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> DevResult {
        Ok(())
    }
}
