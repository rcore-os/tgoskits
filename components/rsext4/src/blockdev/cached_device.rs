//! Cached single-block block device wrapper.

use alloc::vec;

use super::{
    buffer::BlockBuffer,
    traits::{BlockDevice, DevBN},
};
use crate::{
    bmalloc::AbsoluteBN,
    config::runtime_block_size,
    error::{Ext4Error, Ext4Result},
};

/// Translates one ext4 logical-block request into backing-device block units.
#[inline]
fn ext4_to_device_request<B: BlockDevice>(
    dev: &B,
    block_id: AbsoluteBN,
    count: u32,
) -> Ext4Result<(DevBN, u32, usize)> {
    let ext4_block_size = runtime_block_size();
    let dev_block_size = dev.dev_block_size() as usize;

    if dev_block_size == 0 {
        return Err(Ext4Error::invalid_block_size(dev_block_size, 1));
    }
    if ext4_block_size < dev_block_size || !ext4_block_size.is_multiple_of(dev_block_size) {
        return Err(Ext4Error::invalid_block_size(
            ext4_block_size,
            dev_block_size,
        ));
    }

    let dev_blocks_per_ext4 = ext4_block_size / dev_block_size;
    let dev_block_id = block_id
        .raw()
        .checked_mul(dev_blocks_per_ext4 as u64)
        .ok_or_else(Ext4Error::invalid_input)?;
    let dev_count = (count as usize)
        .checked_mul(dev_blocks_per_ext4)
        .ok_or_else(Ext4Error::invalid_input)?;
    let required_size = (count as usize)
        .checked_mul(ext4_block_size)
        .ok_or_else(Ext4Error::invalid_input)?;

    Ok((
        DevBN::new(dev_block_id),
        u32::try_from(dev_count).map_err(|_| Ext4Error::invalid_input())?,
        required_size,
    ))
}

/// Reads ext4 logical blocks by converting them to backing-device block IO.
pub(crate) fn read_ext4_blocks<B: BlockDevice>(
    dev: &mut B,
    buffer: &mut [u8],
    block_id: AbsoluteBN,
    count: u32,
) -> Ext4Result<()> {
    let (dev_block_id, dev_count, required_size) = ext4_to_device_request(dev, block_id, count)?;
    if buffer.len() < required_size {
        return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
    }
    dev.read(&mut buffer[..required_size], dev_block_id, dev_count)
}

/// Writes ext4 logical blocks by converting them to backing-device block IO.
pub(crate) fn write_ext4_blocks<B: BlockDevice>(
    dev: &mut B,
    buffer: &[u8],
    block_id: AbsoluteBN,
    count: u32,
) -> Ext4Result<()> {
    let (dev_block_id, dev_count, required_size) = ext4_to_device_request(dev, block_id, count)?;
    if buffer.len() < required_size {
        return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
    }
    dev.write(&buffer[..required_size], dev_block_id, dev_count)
}

/// Reads arbitrary bytes from the backing device without ext4 block semantics.
pub(crate) fn read_bytes_from_device<B: BlockDevice>(
    dev: &mut B,
    byte_offset: u64,
    buffer: &mut [u8],
) -> Ext4Result<()> {
    let dev_block_size = dev.dev_block_size() as usize;
    if dev_block_size == 0 {
        return Err(Ext4Error::invalid_block_size(dev_block_size, 1));
    }
    if buffer.is_empty() {
        return Ok(());
    }

    let start_block = byte_offset / dev_block_size as u64;
    let offset_in_block = (byte_offset % dev_block_size as u64) as usize;
    let covered = offset_in_block
        .checked_add(buffer.len())
        .ok_or_else(Ext4Error::invalid_input)?;
    let dev_count = covered.div_ceil(dev_block_size);
    let mut temp = vec![0u8; dev_count * dev_block_size];

    dev.read(
        &mut temp,
        DevBN::new(start_block),
        u32::try_from(dev_count).map_err(|_| Ext4Error::invalid_input())?,
    )?;
    buffer.copy_from_slice(&temp[offset_in_block..offset_in_block + buffer.len()]);
    Ok(())
}

/// Cached block device wrapper used internally by the journal proxy.
pub(super) struct BlockDev<B: BlockDevice> {
    dev: B,
    buffer: BlockBuffer,
    is_dirty: bool,
    cached_block: Option<AbsoluteBN>,
}

impl<B: BlockDevice> BlockDev<B> {
    /// Rebuilds the one-block scratch buffer if the runtime ext4 block size changed.
    #[inline]
    fn ensure_buffer_size(&mut self) {
        let block_size = runtime_block_size();
        if self.buffer.len() == block_size {
            return;
        }
        debug_assert!(
            !self.is_dirty,
            "runtime block size changed while cache buffer was dirty"
        );
        self.buffer = BlockBuffer::new(block_size);
        self.cached_block = None;
        self.is_dirty = false;
    }

    /// Creates a new cached block device wrapper.
    #[inline]
    pub fn new(dev: B) -> Self {
        let block_size = runtime_block_size();
        Self {
            dev,
            buffer: BlockBuffer::new(block_size),
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
    #[inline(always)]
    pub fn _open(&mut self) -> Ext4Result<()> {
        self.dev.open()
    }

    /// Flushes pending state and closes the underlying device.
    #[inline]
    pub fn _close(&mut self) -> Ext4Result<()> {
        self.flush()?;
        self.dev.close()
    }

    /// Reads one ext4 logical block into the internal buffer.
    pub fn read_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        self.ensure_buffer_size();
        if self.is_dirty && self.cached_block != Some(block_id) {
            self.flush()?;
        }

        if self.cached_block == Some(block_id) {
            return Ok(());
        }

        read_ext4_blocks(&mut self.dev, self.buffer.as_mut_slice(), block_id, 1)?;
        self.cached_block = Some(block_id);
        self.is_dirty = false;
        Ok(())
    }

    /// Writes the internal ext4 logical block buffer to the target block.
    pub fn write_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        self.ensure_buffer_size();
        if self.dev.is_readonly() {
            return Err(Ext4Error::read_only());
        }

        write_ext4_blocks(&mut self.dev, self.buffer.as_slice(), block_id, 1)?;
        self.cached_block = Some(block_id);
        self.is_dirty = false;
        Ok(())
    }

    /// Reads `count` ext4 logical blocks directly into `buffer`.
    pub fn read_blocks(
        &mut self,
        buffer: &mut [u8],
        block_id: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<()> {
        self.ensure_buffer_size();
        read_ext4_blocks(&mut self.dev, buffer, block_id, count)
    }

    /// Writes `count` ext4 logical blocks directly from `buffer`.
    pub fn write_blocks(
        &mut self,
        buffer: &[u8],
        block_id: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<()> {
        self.ensure_buffer_size();
        if self.dev.is_readonly() {
            return Err(Ext4Error::read_only());
        }

        write_ext4_blocks(&mut self.dev, buffer, block_id, count)
    }

    /// Returns the internal buffer.
    #[inline(always)]
    pub fn buffer(&self) -> &[u8] {
        self.buffer.as_slice()
    }

    /// Returns the internal buffer as mutable and marks it dirty.
    #[inline(always)]
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        self.ensure_buffer_size();
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

    /// Returns the total number of ext4 logical blocks on the underlying device.
    #[inline]
    pub fn total_blocks(&self) -> u64 {
        let dev_block_size = self.dev.dev_block_size() as u64;
        let ext4_block_size = runtime_block_size() as u64;
        self.dev.total_blocks().saturating_mul(dev_block_size) / ext4_block_size
    }

    /// Returns an immutable reference to the underlying device.
    #[inline(always)]
    pub fn _device(&self) -> &B {
        &self.dev
    }

    /// Returns a mutable reference to the underlying device.
    #[inline(always)]
    pub fn device_mut(&mut self) -> &mut B {
        &mut self.dev
    }
}
