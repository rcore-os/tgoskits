//! Physical block adaptation for partition metadata access.

#[cfg(feature = "cache")]
use super::BlockWriter;
use super::{BlockReader, Error, Result};
use crate::BlockDevice;

/// Borrows an already authorized block endpoint for volume discovery or mutation.
/// Bounds, permission and lifetime remain enforced by the underlying owner.
pub struct DeviceReader<'a, T: BlockDevice + ?Sized> {
    /// The caller's leased endpoint; this adapter cannot acquire another owner.
    inner: &'a mut T,
}

impl<'a, T: BlockDevice + ?Sized> DeviceReader<'a, T> {
    /// Borrows `inner` without changing its region or access permissions.
    pub fn new(inner: &'a mut T) -> Self {
        Self { inner }
    }
}

impl<T: BlockDevice + ?Sized> BlockReader for DeviceReader<'_, T> {
    fn block_size(&self) -> usize {
        self.inner.block_size()
    }
    fn num_blocks(&self) -> u64 {
        self.inner.num_blocks()
    }
    fn read_block(&mut self, block: u64, buffer: &mut [u8]) -> Result<()> {
        // Keep the established boot-time tolerance for transient metadata reads.
        // Writes and durability barriers are never retried after uncertain I/O.
        for attempt in 0..4 {
            if self.inner.read_block(block, buffer).is_ok() {
                return Ok(());
            }
            if attempt < 3 {
                log::warn!(
                    "volume metadata read on {} block {block} failed; retrying ({}/3)",
                    self.inner.name(),
                    attempt + 1
                );
            }
        }
        Err(Error::Reader)
    }
}

#[cfg(feature = "cache")]
impl<T: BlockDevice + ?Sized> BlockWriter for DeviceReader<'_, T> {
    fn write_block(&mut self, block: u64, buffer: &[u8]) -> Result<()> {
        self.inner
            .write_block(block, buffer)
            .map_err(|_| Error::Writer)
    }
    fn flush(&mut self) -> Result<()> {
        self.inner.flush().map_err(|_| Error::Writer)
    }
}
