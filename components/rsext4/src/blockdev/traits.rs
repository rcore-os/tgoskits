//! Public block device traits.

use crate::{bmalloc::AbsoluteBN, disknode::Ext4Timestamp, error::Ext4Result};

/// Marker trait for call sites that are expected to trigger block device writes.
pub trait INeedBlockdevToWrite {}

/// Low-level block device interface used by the filesystem.
pub trait BlockDevice {
    /// Writes `count` blocks from `buffer` starting at `block_id`.
    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()>;

    /// Reads `count` blocks into `buffer` starting at `block_id`.
    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()>;

    /// Opens the underlying device.
    fn open(&mut self) -> Ext4Result<()>;

    /// Closes the underlying device.
    fn close(&mut self) -> Ext4Result<()>;

    /// Returns the total number of device blocks.
    fn total_blocks(&self) -> u64;

    /// Returns the device block size in bytes.
    fn block_size(&self) -> u32 {
        512
    }

    /// Flushes device state to stable storage.
    fn flush(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    /// Returns whether the device is currently open.
    fn is_open(&self) -> bool {
        true
    }

    /// Returns whether the device is read-only.
    fn is_readonly(&self) -> bool {
        false
    }

    /// Returns the current timestamp used for inode metadata updates.
    fn current_time(&self) -> Ext4Result<Ext4Timestamp>;
}
