//! Cached single-block block device wrapper.

use super::{buffer::BlockBuffer, traits::BlockDevice};
use crate::{
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
};

/// Cached block device wrapper used internally by the journal proxy.
pub(super) struct BlockDev<B: BlockDevice> {
    dev: B,
    buffer: BlockBuffer,
    is_dirty: bool,
    cached_block: Option<AbsoluteBN>,
}

impl<B: BlockDevice> BlockDev<B> {
    /// Creates a new cached block device wrapper.
    pub fn new(dev: B) -> Self {
        Self {
            dev,
            buffer: BlockBuffer::new(),
            is_dirty: false,
            cached_block: None,
        }
    }

    /// Creates a cached block device wrapper with a caller-provided buffer.
    pub fn _with_buffer(dev: B, buffer: BlockBuffer) -> Ext4Result<Self> {
        if buffer.len() < 512 {
            return Err(Ext4Error::buffer_too_small(buffer.len(), 512));
        }

        Ok(Self {
            dev,
            buffer,
            is_dirty: false,
            cached_block: None,
        })
    }

    /// Opens the underlying device.
    pub fn _open(&mut self) -> Ext4Result<()> {
        self.dev.open()
    }

    /// Flushes pending state and closes the underlying device.
    pub fn _close(&mut self) -> Ext4Result<()> {
        self.flush()?;
        self.dev.close()
    }

    /// Reads one block into the internal buffer.
    pub fn read_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        if self.is_dirty && self.cached_block != Some(block_id) {
            self.flush()?;
        }

        if self.cached_block == Some(block_id) {
            return Ok(());
        }

        self.dev.read(self.buffer.as_mut_slice(), block_id, 1)?;
        self.cached_block = Some(block_id);
        self.is_dirty = false;
        Ok(())
    }

    /// Writes the internal buffer to the target block.
    pub fn write_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        if self.dev.is_readonly() {
            return Err(Ext4Error::read_only());
        }

        self.dev.write(self.buffer.as_slice(), block_id, 1)?;
        self.cached_block = Some(block_id);
        self.is_dirty = false;
        Ok(())
    }

    /// Reads `count` blocks directly into `buffer`.
    pub fn read_blocks(
        &mut self,
        buffer: &mut [u8],
        block_id: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<()> {
        let block_size = self.dev.block_size() as usize;
        let required_size = block_size * count as usize;

        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }

        self.dev.read(buffer, block_id, count)
    }

    /// Writes `count` blocks directly from `buffer`.
    pub fn write_blocks(
        &mut self,
        buffer: &[u8],
        block_id: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<()> {
        if self.dev.is_readonly() {
            return Err(Ext4Error::read_only());
        }

        let block_size = self.dev.block_size() as usize;
        let required_size = block_size * count as usize;

        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }

        self.dev.write(buffer, block_id, count)
    }

    /// Returns the internal buffer.
    pub fn buffer(&self) -> &[u8] {
        self.buffer.as_slice()
    }

    /// Returns the internal buffer as mutable and marks it dirty.
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        self.is_dirty = true;
        self.buffer.as_mut_slice()
    }

    /// Flushes a dirty cached block and the underlying device.
    pub fn flush(&mut self) -> Ext4Result<()> {
        if self.is_dirty
            && let Some(block_id) = self.cached_block
        {
            self.write_block(block_id)?;
        }
        self.dev.flush()
    }

    /// Returns the total number of blocks on the underlying device.
    pub fn total_blocks(&self) -> u64 {
        self.dev.total_blocks()
    }

    /// Returns the underlying device block size.
    pub fn block_size(&self) -> u32 {
        self.dev.block_size()
    }

    /// Returns an immutable reference to the underlying device.
    pub fn _device(&self) -> &B {
        &self.dev
    }

    /// Returns a mutable reference to the underlying device.
    pub fn device_mut(&mut self) -> &mut B {
        &mut self.dev
    }
}
