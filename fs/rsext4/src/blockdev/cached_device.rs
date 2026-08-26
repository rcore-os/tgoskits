//! Held-buffer block device wrapper.
//!
//! Wraps a [`BlockDevice`] with one held 4 KiB block — the buffer that
//! backs the read-modify-write pattern used throughout rsext4, exposed
//! through [`buffer()`] / [`buffer_mut()`].
//!
//! The held block was previously the active entry of a four-entry clock
//! cache. The extra entries were retired once the shared block-layer
//! cache below this crate began serving misses: larger private caches
//! (≥5 entries) were known to keep stale metadata across journal replay
//! (EUCLEAN checksum failures), and 2–4 bought nothing once a miss
//! stopped reaching the device. Metadata capacity now lives in the
//! block-layer cache; only the held buffer remains here.
//!
//! [`buffer()`]: BlockDev::buffer
//! [`buffer_mut()`]: BlockDev::buffer_mut

use super::{buffer::BlockBuffer, traits::BlockDevice};
use crate::{
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
};

/// One held cache line: a 4 KiB data buffer plus housekeeping.
struct CacheLine {
    /// The physical block number, or `None` if no block is held.
    block_id: Option<AbsoluteBN>,
    /// Whether the held data differs from the on-disk copy.
    dirty: bool,
    /// The 4 KiB block buffer.
    buffer: BlockBuffer,
}

impl CacheLine {
    fn new() -> Self {
        Self {
            block_id: None,
            dirty: false,
            buffer: BlockBuffer::new(),
        }
    }
}

/// Held-buffer block device wrapper used internally by the journal proxy.
pub(super) struct BlockDev<B: BlockDevice> {
    dev: B,
    held: CacheLine,
}

impl<B: BlockDevice> BlockDev<B> {
    /// Creates a held-buffer block device wrapper.
    pub fn new(dev: B) -> Self {
        Self {
            dev,
            held: CacheLine::new(),
        }
    }

    pub fn into_inner(self) -> B {
        self.dev
    }

    /// Reads one block into the held buffer.
    ///
    /// On a hit no device I/O happens; the read is served by the
    /// block-layer cache beneath this wrapper. On a miss any pending
    /// modification of the previously held block is written back first.
    pub fn read_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        if self.held.block_id == Some(block_id) {
            return Ok(());
        }
        self.flush_held()?;
        self.dev
            .read(self.held.buffer.as_mut_slice(), block_id, 1)?;
        self.held.block_id = Some(block_id);
        self.held.dirty = false;
        Ok(())
    }

    /// Writes the held buffer to the target block and keeps it held as
    /// the clean copy of that block.
    pub fn write_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        if self.dev.is_readonly() {
            return Err(Ext4Error::read_only());
        }
        self.dev.write(self.held.buffer.as_slice(), block_id, 1)?;
        self.held.block_id = Some(block_id);
        self.held.dirty = false;
        Ok(())
    }

    /// Reads `count` blocks directly into `buffer` (bypasses the held
    /// block).
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

    /// Writes `count` blocks directly from `buffer` (bypasses the held
    /// block, whose contents are refreshed if it overlaps the range).
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

        self.dev.write(buffer, block_id, count)?;

        if let Some(held) = self.held.block_id {
            for off in 0..count {
                if held == block_id.checked_add(off)? {
                    let start = off as usize * block_size;
                    self.held
                        .buffer
                        .as_mut_slice()
                        .copy_from_slice(&buffer[start..start + block_size]);
                    self.held.dirty = false;
                    break;
                }
            }
        }

        Ok(())
    }

    /// Returns the held buffer (read-only view of the last accessed
    /// block).
    pub fn buffer(&self) -> &[u8] {
        self.held.buffer.as_slice()
    }

    /// Returns the held buffer as mutable and marks it dirty.
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        self.held.dirty = true;
        self.held.buffer.as_mut_slice()
    }

    /// Returns whether `block_id` is already backed by the held buffer.
    pub(crate) fn holds_block(&self, block_id: AbsoluteBN) -> bool {
        self.held.block_id == Some(block_id)
    }

    /// Returns the dirty held block so the journal can retain writeback ownership.
    pub(crate) fn dirty_held_block_id(&self) -> Option<AbsoluteBN> {
        if self.held.dirty {
            self.held.block_id
        } else {
            None
        }
    }

    /// Writes back the held block if dirty, then drops it.
    ///
    /// Dirty data is flushed first so modifications made via
    /// [`buffer_mut`] are never silently discarded.
    pub fn invalidate_cache(&mut self) -> Ext4Result<()> {
        self.flush_held()?;
        self.held.block_id = None;
        Ok(())
    }

    /// Replaces the held block contents without writing to the device.
    pub(crate) fn cache_clean_block(
        &mut self,
        block_id: AbsoluteBN,
        data: &[u8; crate::config::BLOCK_SIZE],
    ) -> Ext4Result<()> {
        // A dirty held block reaches the device before it is replaced.
        self.flush_held()?;
        self.held.buffer.as_mut_slice().copy_from_slice(data);
        self.held.block_id = Some(block_id);
        self.held.dirty = false;
        Ok(())
    }

    /// Transfers ownership of a dirty held block to the journal queue.
    ///
    /// The queue owns the pending snapshot of the same buffer, so a later
    /// held-buffer miss must not write the block to its home location before
    /// the journal transaction commits. A later edit transfers ownership again
    /// after the journal refreshes that snapshot.
    pub(crate) fn acknowledge_journaled_block(&mut self, block_id: AbsoluteBN) {
        if self.held.block_id == Some(block_id) {
            self.held.dirty = false;
        }
    }

    /// Refreshes an overlapping held block from a journaled bulk update.
    pub(crate) fn acknowledge_journaled_block_with(
        &mut self,
        block_id: AbsoluteBN,
        data: &[u8; crate::config::BLOCK_SIZE],
    ) {
        if self.held.block_id == Some(block_id) {
            self.held.buffer.as_mut_slice().copy_from_slice(data);
            self.held.dirty = false;
        }
    }

    /// Writes back the held block if dirty, then flushes the device.
    pub fn flush(&mut self) -> Ext4Result<()> {
        self.flush_held()?;
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

    /// Writes the held block back to the device when it carries pending
    /// modifications.
    fn flush_held(&mut self) -> Ext4Result<()> {
        if self.held.dirty {
            let Some(block_id) = self.held.block_id else {
                // A dirty line always has a block: `buffer_mut` is only
                // reachable after `read_block`/`write_block` set one.
                return Ok(());
            };
            if self.dev.is_readonly() {
                return Err(Ext4Error::read_only());
            }
            self.dev.write(self.held.buffer.as_slice(), block_id, 1)?;
            self.held.dirty = false;
        }
        Ok(())
    }
}
