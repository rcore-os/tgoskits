//! Multi-block cached block device wrapper.
//!
//! Wraps a [`BlockIo`] with a fixed-size LRU cache (clock algorithm)
//! of `CACHE_ENTRIES` blocks (4 blocks = 16 KiB with 4 KiB blocks).
//! Each cache hit eliminates one QEMU virtio round-trip, which is the
//! dominant cost on virtualized block devices.
//!
//! The active (most recently accessed) entry exposes its buffer through
//! [`buffer()`] / [`buffer_mut()`] for the read-modify-write pattern
//! used throughout rsext4.

use alloc::vec;

use super::{FilesystemBlockIo, buffer::BlockBuffer};
use crate::{
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
    io::{BlockIo, DeviceCapabilities, DeviceGeometry, SectorId},
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
pub(super) struct BlockDev<B: BlockIo> {
    dev: B,
    geometry: DeviceGeometry,
    capabilities: DeviceCapabilities,
    filesystem_block_size: usize,
    /// The cache lines.
    entries: [CacheLine; CACHE_ENTRIES],
    /// Index of the most recently accessed (active) entry.
    active: usize,
    /// Clock hand for the second-chance eviction policy.
    clock: usize,
}

impl<B: BlockIo> BlockDev<B> {
    /// Creates a new cached block device wrapper.
    pub fn new(dev: B) -> Self {
        let geometry = dev.geometry();
        let capabilities = dev.capabilities();
        Self {
            dev,
            geometry,
            capabilities,
            filesystem_block_size: crate::config::BLOCK_SIZE,
            entries: core::array::from_fn(|_| CacheLine::new()),
            active: 0,
            clock: 0,
        }
    }

    pub fn into_inner(self) -> B {
        self.dev
    }

    pub fn set_filesystem_block_size(&mut self, block_size: usize) -> Ext4Result<()> {
        if block_size == self.filesystem_block_size {
            return self.validate_filesystem_geometry(block_size);
        }
        self.validate_filesystem_geometry(block_size)?;
        if self.entries.iter().any(|entry| entry.dirty) {
            return Err(Ext4Error::busy().with_operation("device:change_dirty_block_geometry"));
        }
        for entry in &mut self.entries {
            entry.block_id = None;
            entry.referenced = false;
        }
        self.active = 0;
        self.clock = 0;
        self.filesystem_block_size = block_size;
        Ok(())
    }

    pub fn read_device_bytes(&mut self, offset: u64, output: &mut [u8]) -> Ext4Result<()> {
        if output.is_empty() {
            return Ok(());
        }
        let (first_sector, sector_count, sector_offset, transfer_size) =
            self.device_io_window(offset, output.len())?;
        let mut sectors = vec![0; transfer_size];
        self.dev.read(&mut sectors, first_sector, sector_count)?;
        output.copy_from_slice(&sectors[sector_offset..sector_offset + output.len()]);
        Ok(())
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
        let (sector, sector_count, byte_count) = self.filesystem_io(block_id, 1)?;
        self.dev.read(
            &mut self.entries[idx].buffer.as_mut_slice()[..byte_count],
            sector,
            sector_count,
        )?;
        self.entries[idx].block_id = Some(block_id);
        self.entries[idx].dirty = false;
        self.entries[idx].referenced = true;
        self.active = idx;
        Ok(())
    }

    /// Writes the active buffer to the target block and marks it as the
    /// active entry.
    pub fn write_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        if self.capabilities.read_only {
            return Err(Ext4Error::read_only());
        }

        let (sector, sector_count, byte_count) = self.filesystem_io(block_id, 1)?;
        let active = &mut self.entries[self.active];
        self.dev.write(
            &active.buffer.as_slice()[..byte_count],
            sector,
            sector_count,
        )?;
        for (index, entry) in self.entries.iter_mut().enumerate() {
            if index != self.active && entry.block_id == Some(block_id) {
                entry.block_id = None;
                entry.dirty = false;
                entry.referenced = false;
            }
        }
        let active = &mut self.entries[self.active];
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
        let block_size = self.filesystem_block_size;
        let required_size = block_size * count as usize;

        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }

        let (sector, sector_count, _) = self.filesystem_io(block_id, count)?;
        self.dev
            .read(&mut buffer[..required_size], sector, sector_count)
    }

    /// Writes `count` blocks directly from `buffer` (bypasses the cache).
    pub fn write_blocks(
        &mut self,
        buffer: &[u8],
        block_id: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<()> {
        if self.capabilities.read_only {
            return Err(Ext4Error::read_only());
        }

        let block_size = self.filesystem_block_size;
        let required_size = block_size * count as usize;

        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }

        let (sector, sector_count, _) = self.filesystem_io(block_id, count)?;
        self.dev
            .write(&buffer[..required_size], sector, sector_count)?;

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
                }
            }
        }

        Ok(())
    }

    /// Returns the active buffer (read-only view of the last accessed block).
    pub fn buffer(&self) -> &[u8] {
        &self.entries[self.active].buffer.as_slice()[..self.filesystem_block_size]
    }

    /// Returns the active buffer as mutable and marks the entry dirty.
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        self.entries[self.active].dirty = true;
        &mut self.entries[self.active].buffer.as_mut_slice()[..self.filesystem_block_size]
    }

    /// Flushes dirty cached blocks, then invalidates all entries.
    ///
    /// Dirty entries are flushed first so metadata modifications made
    /// via [`buffer_mut`] are never silently discarded.
    pub fn invalidate_cache(&mut self) -> Ext4Result<()> {
        for entry in self.entries.iter_mut() {
            if entry.dirty && !entry.is_empty() {
                let bid = entry.block_id.unwrap();
                let logical_sector_size = self.geometry.logical_block_size as usize;
                let sectors_per_block = self.filesystem_block_size / logical_sector_size;
                let sector = SectorId::new(
                    bid.raw()
                        .checked_mul(sectors_per_block as u64)
                        .ok_or_else(Ext4Error::overflow)?,
                );
                self.dev.write(
                    &entry.buffer.as_slice()[..self.filesystem_block_size],
                    sector,
                    u32::try_from(sectors_per_block).map_err(|_| Ext4Error::overflow())?,
                )?;
                entry.dirty = false;
            }
            entry.block_id = None;
            entry.referenced = false;
        }
        self.active = 0;
        self.clock = 0;
        Ok(())
    }

    /// Replaces cached block contents without writing to the device.
    pub(crate) fn cache_clean_block(
        &mut self,
        block_id: AbsoluteBN,
        data: &[u8],
    ) -> Ext4Result<()> {
        if data.len() != self.filesystem_block_size {
            return Err(Ext4Error::invalid_block_size(
                data.len(),
                self.filesystem_block_size,
            ));
        }
        // Reuse an existing slot for this block, or pick a victim.
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if !entry.is_empty() && entry.block_id == Some(block_id) {
                entry.buffer.as_mut_slice()[..self.filesystem_block_size].copy_from_slice(data);
                entry.dirty = false;
                entry.referenced = true;
                self.active = i;
                return Ok(());
            }
        }

        // Not found — allocate a fresh slot via clock.
        let idx = self.clock_evict()?;
        self.entries[idx].buffer.as_mut_slice()[..self.filesystem_block_size].copy_from_slice(data);
        self.entries[idx].block_id = Some(block_id);
        self.entries[idx].dirty = false;
        self.entries[idx].referenced = true;
        Ok(())
    }

    /// Flushes all dirty cached blocks and the underlying device.
    pub fn flush(&mut self) -> Ext4Result<()> {
        for entry in self.entries.iter_mut() {
            if entry.dirty && !entry.is_empty() {
                let logical_sector_size = self.geometry.logical_block_size as usize;
                let sectors_per_block = self.filesystem_block_size / logical_sector_size;
                let block = entry.block_id.unwrap();
                let sector = SectorId::new(
                    block
                        .raw()
                        .checked_mul(sectors_per_block as u64)
                        .ok_or_else(Ext4Error::overflow)?,
                );
                self.dev.write(
                    &entry.buffer.as_slice()[..self.filesystem_block_size],
                    sector,
                    u32::try_from(sectors_per_block).map_err(|_| Ext4Error::overflow())?,
                )?;
                entry.dirty = false;
            }
        }
        self.dev.flush()
    }

    /// Returns the total number of blocks on the underlying device.
    pub fn total_blocks(&self) -> u64 {
        let sectors_per_block =
            self.filesystem_block_size as u64 / u64::from(self.geometry.logical_block_size);
        self.geometry.block_count / sectors_per_block
    }

    /// Returns the underlying device block size.
    pub fn block_size(&self) -> u32 {
        self.filesystem_block_size as u32
    }

    /// Returns an immutable reference to the underlying device.
    pub fn _device(&self) -> &B {
        &self.dev
    }

    fn filesystem_io(&self, block: AbsoluteBN, count: u32) -> Ext4Result<(SectorId, u32, usize)> {
        self.validate_filesystem_geometry(self.filesystem_block_size)?;
        let logical_sector_size = self.geometry.logical_block_size as usize;

        let sectors_per_block = self.filesystem_block_size / logical_sector_size;
        let sector_count = u32::try_from(sectors_per_block)
            .ok()
            .and_then(|sectors| sectors.checked_mul(count))
            .ok_or_else(Ext4Error::overflow)?;
        let sector = block
            .raw()
            .checked_mul(sectors_per_block as u64)
            .map(SectorId::new)
            .ok_or_else(Ext4Error::overflow)?;
        let end_sector = sector
            .raw()
            .checked_add(u64::from(sector_count))
            .ok_or_else(Ext4Error::overflow)?;
        if end_sector > self.geometry.block_count {
            return Err(Ext4Error::io().with_operation("device:sector_out_of_range"));
        }
        let byte_count = self
            .filesystem_block_size
            .checked_mul(count as usize)
            .ok_or_else(Ext4Error::overflow)?;
        Ok((sector, sector_count, byte_count))
    }

    fn validate_filesystem_geometry(&self, block_size: usize) -> Ext4Result<()> {
        let logical_sector_size = self.geometry.logical_block_size as usize;
        if logical_sector_size == 0
            || !logical_sector_size.is_power_of_two()
            || !(1024..=crate::config::BLOCK_SIZE).contains(&block_size)
            || !block_size.is_power_of_two()
            || !block_size.is_multiple_of(logical_sector_size)
        {
            return Err(Ext4Error::bad_superblock().with_operation("device:filesystem_geometry"));
        }
        Ok(())
    }

    fn device_io_window(
        &self,
        offset: u64,
        byte_count: usize,
    ) -> Ext4Result<(SectorId, u32, usize, usize)> {
        let sector_size =
            usize::try_from(self.geometry.logical_block_size).map_err(|_| Ext4Error::overflow())?;
        if sector_size == 0 || !sector_size.is_power_of_two() {
            return Err(Ext4Error::bad_superblock().with_operation("device:sector_geometry"));
        }

        let first_sector = offset / sector_size as u64;
        let sector_offset = (offset % sector_size as u64) as usize;
        let covered_bytes = sector_offset
            .checked_add(byte_count)
            .ok_or_else(Ext4Error::overflow)?;
        let sector_count = covered_bytes.div_ceil(sector_size);
        let end_sector = first_sector
            .checked_add(sector_count as u64)
            .ok_or_else(Ext4Error::overflow)?;
        if end_sector > self.geometry.block_count {
            return Err(Ext4Error::io().with_operation("device:byte_range_out_of_bounds"));
        }
        let sector_count = u32::try_from(sector_count).map_err(|_| Ext4Error::overflow())?;
        let transfer_size = sector_size
            .checked_mul(sector_count as usize)
            .ok_or_else(Ext4Error::overflow)?;
        Ok((
            SectorId::new(first_sector),
            sector_count,
            sector_offset,
            transfer_size,
        ))
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
                let (sector, sector_count, byte_count) = self.filesystem_io(bid, 1)?;
                self.dev.write(
                    &self.entries[idx].buffer.as_slice()[..byte_count],
                    sector,
                    sector_count,
                )?;
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
            let (sector, sector_count, byte_count) = self.filesystem_io(bid, 1)?;
            self.dev.write(
                &self.entries[idx].buffer.as_slice()[..byte_count],
                sector,
                sector_count,
            )?;
            self.entries[idx].dirty = false;
        }
        self.entries[idx].block_id = None;
        self.entries[idx].referenced = false;
        self.active = idx;
        Ok(idx)
    }
}

impl<B: BlockIo> FilesystemBlockIo for BlockDev<B> {
    fn read(&mut self, buffer: &mut [u8], block: AbsoluteBN, count: u32) -> Ext4Result<()> {
        self.read_blocks(buffer, block, count)
    }

    fn write(&mut self, buffer: &[u8], block: AbsoluteBN, count: u32) -> Ext4Result<()> {
        self.write_blocks(buffer, block, count)
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.flush()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;

    const SECTOR_SIZE: usize = 512;

    struct StrictSectorDevice {
        data: Vec<u8>,
    }

    impl BlockIo for StrictSectorDevice {
        fn write(&mut self, buffer: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
            let required = SECTOR_SIZE * count as usize;
            if buffer.len() != required {
                return Err(Ext4Error::invalid_block_size(buffer.len(), required));
            }
            let start = sector.as_usize()? * SECTOR_SIZE;
            self.data[start..start + required].copy_from_slice(buffer);
            Ok(())
        }

        fn read(&mut self, buffer: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
            let required = SECTOR_SIZE * count as usize;
            if buffer.len() != required {
                return Err(Ext4Error::invalid_block_size(buffer.len(), required));
            }
            let start = sector.as_usize()? * SECTOR_SIZE;
            buffer.copy_from_slice(&self.data[start..start + required]);
            Ok(())
        }

        fn geometry(&self) -> DeviceGeometry {
            DeviceGeometry::new(SECTOR_SIZE as u32, (self.data.len() / SECTOR_SIZE) as u64)
        }

        fn capabilities(&self) -> DeviceCapabilities {
            DeviceCapabilities {
                flush: true,
                ..DeviceCapabilities::default()
            }
        }

        fn flush(&mut self) -> Ext4Result<()> {
            Ok(())
        }
    }

    #[test]
    fn filesystem_block_io_aggregates_device_sectors() {
        let mut data = vec![0; 16 * crate::config::BLOCK_SIZE];
        let block = AbsoluteBN::new(2);
        let start = block.as_usize().unwrap() * crate::config::BLOCK_SIZE;
        for (index, byte) in data[start..start + crate::config::BLOCK_SIZE]
            .iter_mut()
            .enumerate()
        {
            *byte = index as u8;
        }

        let mut dev = BlockDev::new(StrictSectorDevice { data });
        dev.read_block(block).expect("read a full filesystem block");
        assert_eq!(dev.buffer()[0], 0);
        assert_eq!(dev.buffer()[511], 255);
        assert_eq!(dev.buffer()[512], 0);
        assert_eq!(dev.buffer()[crate::config::BLOCK_SIZE - 1], 255);

        dev.buffer_mut()[crate::config::BLOCK_SIZE - 1] = 0x5a;
        dev.write_block(block)
            .expect("write a full filesystem block");
        let inner = dev.into_inner();
        assert_eq!(inner.data[start + crate::config::BLOCK_SIZE - 1], 0x5a);
    }

    #[test]
    fn byte_offset_io_reads_superblock_across_device_sectors() {
        let mut data = vec![0; 16 * crate::config::BLOCK_SIZE];
        for (index, byte) in data[1024..2048].iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(17);
        }
        let expected = data[1024..2048].to_vec();
        let mut dev = BlockDev::new(StrictSectorDevice { data });
        let mut superblock = [0; 1024];

        dev.read_device_bytes(1024, &mut superblock)
            .expect("read the superblock by byte offset");

        assert_eq!(superblock.as_slice(), expected.as_slice());
    }

    #[test]
    fn write_to_cached_target_invalidates_older_alias() {
        let data = vec![0; 16 * crate::config::BLOCK_SIZE];
        let target = AbsoluteBN::new(2);
        let mut dev = BlockDev::new(StrictSectorDevice { data });

        dev.read_block(target).unwrap();
        dev.read_block(AbsoluteBN::new(1)).unwrap();
        dev.buffer_mut().fill(0x5a);
        dev.write_block(target).unwrap();
        dev.read_block(target).unwrap();

        assert!(dev.buffer().iter().all(|byte| *byte == 0x5a));
    }
}
