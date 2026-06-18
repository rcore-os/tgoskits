//! Multi-block cached block device wrapper.
//!
//! Wraps a [`BlockDevice`] with a fixed-size LRU cache (clock algorithm)
//! of `CACHE_ENTRIES` blocks (4 blocks = 16 KiB with 4 KiB blocks).
//! Each cache hit eliminates one QEMU virtio round-trip, which is the
//! dominant cost on virtualized block devices.
//!
//! The active (most recently accessed) entry exposes its buffer through
//! [`buffer()`] / [`buffer_mut()`] for the read-modify-write pattern
//! used throughout rsext4.

use super::{buffer::BlockBuffer, traits::BlockDevice};
use crate::{
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
};

/// Number of cached blocks. 4 blocks × 4 KiB = 16 KiB cache.
///
/// Limited to 4 entries: larger caches (≥5) cause stale metadata blocks to
/// persist across journal replay and mount operations, triggering EUCLEAN
/// checksum failures and subtract-overflow panics in CRC integrity tests.
const CACHE_ENTRIES: usize = 4;

/// One cache line: a 4 KiB data buffer plus housekeeping.
struct CacheLine {
    /// The physical block number, or `None` if the slot is unused.
    block_id: Option<AbsoluteBN>,
    /// Whether the in-cache data differs from the on-disk copy.
    dirty: bool,
    /// Clock eviction reference bit.
    referenced: bool,
    /// The 4 KiB block buffer.
    buffer: BlockBuffer,
}

impl CacheLine {
    fn new() -> Self {
        Self {
            block_id: None,
            dirty: false,
            referenced: false,
            buffer: BlockBuffer::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.block_id.is_none()
    }
}

/// Multi-block cached block device wrapper used internally by the journal proxy.
pub(super) struct BlockDev<B: BlockDevice> {
    dev: B,
    /// The cache lines.
    entries: [CacheLine; CACHE_ENTRIES],
    /// Index of the most recently accessed (active) entry.
    active: usize,
    /// Clock hand for the second-chance eviction policy.
    clock: usize,
}

impl<B: BlockDevice> BlockDev<B> {
    /// Creates a new cached block device wrapper.
    pub fn new(dev: B) -> Self {
        Self {
            dev,
            entries: core::array::from_fn(|_| CacheLine::new()),
            active: 0,
            clock: 0,
        }
    }

    pub fn into_inner(self) -> B {
        self.dev
    }

    /// Creates a cached block device wrapper with a caller-provided buffer.
    pub fn _with_buffer(dev: B, buffer: BlockBuffer) -> Ext4Result<Self> {
        if buffer.len() < 512 {
            return Err(Ext4Error::buffer_too_small(buffer.len(), 512));
        }

        let mut slf = Self::new(dev);
        slf.entries[0].buffer = buffer;
        Ok(slf)
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

    /// Reads one block into the cache and makes it the active entry.
    ///
    /// On a cache hit the active entry is updated with no device I/O.
    /// On a miss the least recently used (clock) entry is recycled;
    /// if it is dirty it is flushed first.
    pub fn read_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        // Cache hit — just mark referenced and make active.
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if !entry.is_empty() && entry.block_id == Some(block_id) {
                entry.referenced = true;
                self.active = i;
                return Ok(());
            }
        }

        // Cache miss — find a victim via clock.
        let idx = self.clock_evict()?;

        // Read into the victim slot and make it the active entry.
        self.dev
            .read(self.entries[idx].buffer.as_mut_slice(), block_id, 1)?;
        self.entries[idx].block_id = Some(block_id);
        self.entries[idx].dirty = false;
        self.entries[idx].referenced = true;
        self.active = idx;
        Ok(())
    }

    /// Writes the active buffer to the target block and marks it as the
    /// active entry.
    pub fn write_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        if self.dev.is_readonly() {
            return Err(Ext4Error::read_only());
        }

        let active = &mut self.entries[self.active];
        self.dev.write(active.buffer.as_slice(), block_id, 1)?;
        active.block_id = Some(block_id);
        active.dirty = false;
        active.referenced = true;
        Ok(())
    }

    /// Reads `count` blocks directly into `buffer` (bypasses the cache).
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

    /// Writes `count` blocks directly from `buffer` (bypasses the cache).
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

        for off in 0..count {
            let target = block_id.checked_add(off)?;
            for entry in self.entries.iter_mut() {
                if !entry.is_empty() && entry.block_id == Some(target) {
                    let start = off as usize * block_size;
                    entry
                        .buffer
                        .as_mut_slice()
                        .copy_from_slice(&buffer[start..start + block_size]);
                    entry.dirty = false;
                    entry.referenced = true;
                    break;
                }
            }
        }

        Ok(())
    }

    /// Returns the active buffer (read-only view of the last accessed block).
    pub fn buffer(&self) -> &[u8] {
        self.entries[self.active].buffer.as_slice()
    }

    /// Returns the active buffer as mutable and marks the entry dirty.
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        self.entries[self.active].dirty = true;
        self.entries[self.active].buffer.as_mut_slice()
    }

    /// Invalidates the cache without flushing.
    /// Used after journal commit to prevent stale cached data
    /// from shadowing newly-committed blocks.
    pub fn invalidate_cache(&mut self) {
        for entry in self.entries.iter_mut() {
            entry.block_id = None;
            entry.dirty = false;
            entry.referenced = false;
        }
    }

    /// Replaces cached block contents without writing to the device.
    pub(crate) fn cache_clean_block(
        &mut self,
        block_id: AbsoluteBN,
        data: &[u8; crate::config::BLOCK_SIZE],
    ) -> Ext4Result<()> {
        // Reuse an existing slot for this block, or pick a victim.
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if !entry.is_empty() && entry.block_id == Some(block_id) {
                entry.buffer.as_mut_slice().copy_from_slice(data);
                entry.dirty = false;
                entry.referenced = true;
                self.active = i;
                return Ok(());
            }
        }

        // Not found — allocate a fresh slot via clock.
        let idx = self.clock_evict()?;
        self.entries[idx]
            .buffer
            .as_mut_slice()
            .copy_from_slice(data);
        self.entries[idx].block_id = Some(block_id);
        self.entries[idx].dirty = false;
        self.entries[idx].referenced = true;
        Ok(())
    }

    /// Flushes all dirty cached blocks and the underlying device.
    pub fn flush(&mut self) -> Ext4Result<()> {
        for entry in self.entries.iter_mut() {
            if entry.dirty && !entry.is_empty() {
                self.dev
                    .write(entry.buffer.as_slice(), entry.block_id.unwrap(), 1)?;
                entry.dirty = false;
            }
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

    // ─── clock eviction ────────────────────────────────────────────

    /// Finds a cache slot to reuse via the clock (second-chance) algorithm.
    ///
    /// Dirty victims are flushed before the slot is returned.  Returns
    /// the index of the newly-allocated slot (which is also set as the
    /// active entry).  The caller must fill the buffer.
    fn clock_evict(&mut self) -> Ext4Result<usize> {
        for _ in 0..(CACHE_ENTRIES * 2) {
            let idx = self.clock;
            self.clock = (self.clock + 1) % CACHE_ENTRIES;

            // Borrow entries[idx] via index to avoid holding a ref across
            // the potential write below.
            if self.entries[idx].is_empty() {
                self.active = idx;
                return Ok(idx);
            }

            if self.entries[idx].referenced {
                self.entries[idx].referenced = false;
                continue;
            }

            // Unreferenced — flush if dirty, then recycle.
            if self.entries[idx].dirty {
                let bid = self.entries[idx].block_id.unwrap();
                self.dev
                    .write(self.entries[idx].buffer.as_slice(), bid, 1)?;
                self.entries[idx].dirty = false;
            }

            self.entries[idx].block_id = None;
            self.entries[idx].referenced = false;
            self.active = idx;
            return Ok(idx);
        }

        // All entries referenced — fall back to the current clock slot.
        let idx = self.clock;
        self.clock = (self.clock + 1) % CACHE_ENTRIES;
        if self.entries[idx].dirty {
            let bid = self.entries[idx].block_id.unwrap();
            self.dev
                .write(self.entries[idx].buffer.as_slice(), bid, 1)?;
            self.entries[idx].dirty = false;
        }
        self.entries[idx].block_id = None;
        self.entries[idx].referenced = false;
        self.active = idx;
        Ok(idx)
    }
}
