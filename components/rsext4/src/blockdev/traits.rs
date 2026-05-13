//! Public block device traits.

use core::fmt;

use crate::{
    disknode::Ext4Timestamp,
    error::{Errno, Ext4Error, Ext4Result},
};

fn overflow_error() -> Ext4Error {
    Ext4Error::from(Errno::EOVERFLOW)
}

/// Absolute block number in the physical backing device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DevBN(u64);

impl DevBN {
    /// Creates a new backing-device block number.
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the underlying raw value.
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Converts this block number into `usize`.
    pub fn as_usize(self) -> Ext4Result<usize> {
        usize::try_from(self.0).map_err(|_| overflow_error())
    }

    /// Converts this block number into `u32`, failing on overflow.
    pub fn to_u32(self) -> Ext4Result<u32> {
        u32::try_from(self.0).map_err(|_| overflow_error())
    }
}

impl From<u32> for DevBN {
    fn from(value: u32) -> Self {
        Self(u64::from(value))
    }
}

impl fmt::Display for DevBN {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Marker trait for call sites that are expected to trigger block device writes.
pub trait INeedBlockdevToWrite {}

/// Low-level physical block device interface used by the filesystem.
///
/// Contract:
/// - `block_id: DevBN` is counted in backing-device block units, not ext4
///   logical blocks.
/// - `count` is also measured in backing-device block units.
/// - `buffer.len()` is therefore expected to cover `count * dev_block_size()`
///   bytes.
/// - ext4 logical block numbers must be translated by the filesystem layer
///   before calling into this trait.
pub trait BlockDevice {
    /// Writes `count` device blocks from `buffer` starting at `block_id`.
    ///
    /// Parameters:
    /// - `buffer`: source bytes to be written; it should contain at least
    ///   `count * dev_block_size()` bytes.
    /// - `block_id`: starting physical device block number in backing-device block
    ///   units.
    /// - `count`: number of backing-device blocks to write.
    fn write(&mut self, buffer: &[u8], block_id: DevBN, count: u32) -> Ext4Result<()>;

    /// Reads `count` device blocks into `buffer` starting at `block_id`.
    ///
    /// Parameters:
    /// - `buffer`: destination bytes for the read result; it should have room
    ///   for at least `count * dev_block_size()` bytes.
    /// - `block_id`: starting physical device block number in backing-device block
    ///   units.
    /// - `count`: number of backing-device blocks to read.
    fn read(&mut self, buffer: &mut [u8], block_id: DevBN, count: u32) -> Ext4Result<()>;

    /// Opens the underlying device.
    fn open(&mut self) -> Ext4Result<()>;

    /// Closes the underlying device.
    fn close(&mut self) -> Ext4Result<()>;

    /// Returns the total number of device blocks.
    fn total_blocks(&self) -> u64;

    /// Returns the backing-device block size in bytes.
    fn dev_block_size(&self) -> u32;

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
