//! Held-buffer block device wrapper.
//!
//! The shared block layer below rsext4 owns reusable physical-block cache
//! state. This wrapper deliberately holds only the one filesystem block
//! needed by rsext4's read-modify-publish sequence. Keeping another private
//! multi-block cache here would give journal replay and checkpointing a second
//! coherence domain.

use alloc::vec;

use super::{FilesystemBlockIo, buffer::BlockBuffer};
use crate::{
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
    io::{BlockIo, DeviceCapabilities, DeviceGeometry, SectorId, WriteFlags},
};

/// The one runtime-sized filesystem block held for read-modify-publish.
struct CacheLine {
    /// The physical block number, or `None` if no block is held.
    block_id: Option<AbsoluteBN>,
    /// Whether the caller still owns an unpublished mutable image.
    dirty: bool,
    buffer: BlockBuffer,
}

impl CacheLine {
    fn new(block_size: usize) -> Self {
        Self {
            block_id: None,
            dirty: false,
            buffer: BlockBuffer::new(block_size),
        }
    }
}

/// One held block used internally by the journal proxy.
pub(super) struct BlockDev<B: BlockIo> {
    dev: B,
    geometry: DeviceGeometry,
    capabilities: DeviceCapabilities,
    filesystem_block_size: usize,
    held: CacheLine,
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
            held: CacheLine::new(crate::config::BLOCK_SIZE),
        }
    }

    pub fn into_inner(self) -> B {
        debug_assert!(
            !self.held.dirty,
            "dropping an unpublished rsext4 metadata edit"
        );
        self.dev
    }

    pub fn set_filesystem_block_size(&mut self, block_size: usize) -> Ext4Result<()> {
        if block_size == self.filesystem_block_size {
            return self.validate_filesystem_geometry(block_size);
        }
        self.validate_filesystem_geometry(block_size)?;
        if self.held.dirty {
            return Err(Ext4Error::busy().with_operation("device:change_dirty_block_geometry"));
        }
        self.held = CacheLine::new(block_size);
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
        let block_size = buffer.len();
        let mut slf = Self::new(dev);
        slf.set_filesystem_block_size(block_size)?;
        slf.held.buffer = buffer;
        Ok(slf)
    }

    /// Reads one block into the held RMW buffer.
    ///
    /// Journaled metadata must be published to the journal before switching
    /// away from a dirty block. A miss never writes dirty metadata directly to
    /// its home block.
    pub fn read_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        if self.held.block_id == Some(block_id) {
            return Ok(());
        }
        self.require_published_edit("device:switch_unpublished_block")?;
        let (sector, sector_count, byte_count) = self.filesystem_io(block_id, 1)?;
        self.dev.read(
            &mut self.held.buffer.as_mut_slice()[..byte_count],
            sector,
            sector_count,
        )?;
        self.held.block_id = Some(block_id);
        self.held.dirty = false;
        Ok(())
    }

    /// Writes the held buffer to the target block and keeps it as the clean
    /// image of that block.
    pub fn write_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        if self.capabilities.read_only {
            return Err(Ext4Error::read_only());
        }
        self.require_held_block(block_id, "device:write_wrong_held_block")?;

        let (sector, sector_count, byte_count) = self.filesystem_io(block_id, 1)?;
        self.dev.write(
            &self.held.buffer.as_slice()[..byte_count],
            sector,
            sector_count,
        )?;
        self.held.block_id = Some(block_id);
        self.held.dirty = false;
        Ok(())
    }

    /// Transfers the held image to JBD2 without writing its home block.
    pub(crate) fn publish_journaled_block(&mut self, block_id: AbsoluteBN) {
        self.held.block_id = Some(block_id);
        self.held.dirty = false;
    }

    /// Discards the held image without writeback.
    pub(crate) fn discard_held(&mut self) {
        self.held.block_id = None;
        self.held.dirty = false;
    }

    /// Returns whether the caller still owns an unpublished mutable image.
    pub(crate) fn has_unpublished_edit(&self) -> bool {
        self.held.dirty
    }

    /// Reads `count` blocks directly into `buffer` (bypasses the cache).
    pub fn read_blocks(
        &mut self,
        buffer: &mut [u8],
        block_id: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<()> {
        let block_size = self.filesystem_block_size;
        let required_size = block_size
            .checked_mul(count as usize)
            .ok_or_else(Ext4Error::overflow)?;

        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }
        if self.held.dirty && self.range_contains(block_id, count, self.held.block_id) {
            return Err(Ext4Error::busy().with_operation("device:read_unpublished_block"));
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
        self.write_blocks_with_flags(buffer, block_id, count, WriteFlags::empty())
    }

    pub(crate) fn write_blocks_with_flags(
        &mut self,
        buffer: &[u8],
        block_id: AbsoluteBN,
        count: u32,
        flags: WriteFlags,
    ) -> Ext4Result<()> {
        if self.capabilities.read_only {
            return Err(Ext4Error::read_only());
        }

        let block_size = self.filesystem_block_size;
        let required_size = block_size
            .checked_mul(count as usize)
            .ok_or_else(Ext4Error::overflow)?;

        if buffer.len() < required_size {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required_size));
        }
        if count > 0 {
            block_id.checked_add(count - 1)?;
        }
        if self.held.dirty {
            return Err(Ext4Error::busy().with_operation("device:write_with_unpublished_block"));
        }

        let (sector, sector_count, _) = self.filesystem_io(block_id, count)?;
        let fallback_flush = if flags.contains(WriteFlags::FUA) && !self.capabilities.fua {
            if !self.capabilities.flush {
                return Err(Ext4Error::unsupported_capability("block_io:fua_or_flush"));
            }
            let mut fallback_flags = flags;
            fallback_flags.remove(WriteFlags::FUA);
            self.dev.write_with_flags(
                &buffer[..required_size],
                sector,
                sector_count,
                fallback_flags,
            )?;
            Some(self.dev.flush())
        } else {
            self.dev
                .write_with_flags(&buffer[..required_size], sector, sector_count, flags)?;
            None
        };

        if let Some(held) = self.held.block_id {
            for off in 0..count {
                if held == block_id.checked_add(off)? {
                    let start = off as usize * block_size;
                    self.held.buffer.as_mut_slice()[..block_size]
                        .copy_from_slice(&buffer[start..start + block_size]);
                    self.held.dirty = false;
                    break;
                }
            }
        }

        if let Some(result) = fallback_flush {
            result?;
        }

        Ok(())
    }

    /// Returns the held buffer.
    pub fn buffer(&self) -> &[u8] {
        &self.held.buffer.as_slice()[..self.filesystem_block_size]
    }

    /// Returns the held image only when it belongs to `block_id`.
    ///
    /// This is the buffer-head identity check at the JBD2 ownership boundary:
    /// an image read for one home block must never be journaled as another.
    pub(crate) fn buffer_for_block(&self, block_id: AbsoluteBN) -> Ext4Result<&[u8]> {
        self.require_held_block(block_id, "device:read_wrong_held_block")?;
        Ok(self.buffer())
    }

    /// Returns a clean held image that can serve as a direct-write before-image.
    pub(crate) fn clean_buffer_for_block(&self, block_id: AbsoluteBN) -> Option<&[u8]> {
        (!self.held.dirty && self.held.block_id == Some(block_id)).then(|| self.buffer())
    }

    /// Returns the held buffer as mutable and marks it unpublished.
    pub(super) fn buffer_mut(&mut self) -> &mut [u8] {
        self.held.dirty = true;
        &mut self.held.buffer.as_mut_slice()[..self.filesystem_block_size]
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
        self.require_published_edit("device:replace_unpublished_block")?;
        self.held.buffer.as_mut_slice()[..self.filesystem_block_size].copy_from_slice(data);
        self.held.block_id = Some(block_id);
        self.held.dirty = false;
        Ok(())
    }

    /// Invalidates one cached block without writing its current contents.
    ///
    /// Filesystem metadata may only use this after the block has been detached
    /// from every durable owner. Writing the old buffer after the allocator
    /// reuses that physical block would corrupt the new owner.
    pub(crate) fn discard_block(&mut self, block_id: AbsoluteBN) {
        if self.held.block_id == Some(block_id) {
            self.discard_held();
        }
    }

    /// Flushes an unjournaled held block and then the lower block layer.
    pub fn flush(&mut self) -> Ext4Result<()> {
        self.flush_held()?;
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

    #[cfg(test)]
    pub fn _device_mut(&mut self) -> &mut B {
        &mut self.dev
    }

    fn require_published_edit(&self, operation: &'static str) -> Ext4Result<()> {
        if self.held.dirty {
            Err(Ext4Error::busy().with_operation(operation))
        } else {
            Ok(())
        }
    }

    fn require_held_block(&self, block_id: AbsoluteBN, operation: &'static str) -> Ext4Result<()> {
        if self.held.block_id == Some(block_id) {
            Ok(())
        } else {
            Err(Ext4Error::busy().with_operation(operation))
        }
    }

    fn range_contains(&self, first: AbsoluteBN, count: u32, candidate: Option<AbsoluteBN>) -> bool {
        let Some(candidate) = candidate else {
            return false;
        };
        let Some(offset) = candidate.raw().checked_sub(first.raw()) else {
            return false;
        };
        offset < u64::from(count)
    }

    fn flush_held(&mut self) -> Ext4Result<()> {
        if !self.held.dirty {
            return Ok(());
        }
        let block_id = self
            .held
            .block_id
            .ok_or_else(|| Ext4Error::corrupted().with_operation("device:dirty_without_block"))?;
        if self.capabilities.read_only {
            return Err(Ext4Error::read_only());
        }
        let (sector, sector_count, byte_count) = self.filesystem_io(block_id, 1)?;
        self.dev.write(
            &self.held.buffer.as_slice()[..byte_count],
            sector,
            sector_count,
        )?;
        self.held.dirty = false;
        Ok(())
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
        let physical_sector_size =
            (self.geometry.physical_block_size as usize).max(logical_sector_size);
        if logical_sector_size == 0
            || !logical_sector_size.is_power_of_two()
            || !physical_sector_size.is_power_of_two()
            || !(crate::config::MIN_BLOCK_SIZE as usize..=crate::config::MAX_BLOCK_SIZE as usize)
                .contains(&block_size)
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
}

impl<B: BlockIo> FilesystemBlockIo for BlockDev<B> {
    fn block_size(&self) -> usize {
        self.filesystem_block_size
    }

    fn read(&mut self, buffer: &mut [u8], block: AbsoluteBN, count: u32) -> Ext4Result<()> {
        self.read_blocks(buffer, block, count)
    }

    fn write(&mut self, buffer: &[u8], block: AbsoluteBN, count: u32) -> Ext4Result<()> {
        self.write_blocks(buffer, block, count)
    }

    fn write_with_flags(
        &mut self,
        buffer: &[u8],
        block: AbsoluteBN,
        count: u32,
        flags: WriteFlags,
    ) -> Ext4Result<()> {
        self.write_blocks_with_flags(buffer, block, count, flags)
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
        physical_block_size: u32,
        reads: usize,
    }

    struct DurabilityDevice {
        data: Vec<u8>,
        capabilities: DeviceCapabilities,
        last_write: Option<(u64, u32, WriteFlags)>,
        flushes: usize,
        fail_flush: bool,
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
            self.reads += 1;
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
                .with_physical_block_size(self.physical_block_size)
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

    impl BlockIo for DurabilityDevice {
        fn write(&mut self, buffer: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
            self.write_with_flags(buffer, sector, count, WriteFlags::empty())
        }

        fn write_with_flags(
            &mut self,
            buffer: &[u8],
            sector: SectorId,
            count: u32,
            flags: WriteFlags,
        ) -> Ext4Result<()> {
            let required = SECTOR_SIZE * count as usize;
            if buffer.len() != required {
                return Err(Ext4Error::invalid_block_size(buffer.len(), required));
            }
            let start = sector.as_usize()? * SECTOR_SIZE;
            self.data[start..start + required].copy_from_slice(buffer);
            self.last_write = Some((sector.raw(), count, flags));
            Ok(())
        }

        fn read(&mut self, buffer: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
            let required = SECTOR_SIZE * count as usize;
            let start = sector.as_usize()? * SECTOR_SIZE;
            buffer.copy_from_slice(&self.data[start..start + required]);
            Ok(())
        }

        fn geometry(&self) -> DeviceGeometry {
            DeviceGeometry::new(SECTOR_SIZE as u32, (self.data.len() / SECTOR_SIZE) as u64)
        }

        fn capabilities(&self) -> DeviceCapabilities {
            self.capabilities
        }

        fn flush(&mut self) -> Ext4Result<()> {
            self.flushes += 1;
            if self.fail_flush {
                Err(Ext4Error::io())
            } else {
                Ok(())
            }
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

        let mut dev = BlockDev::new(StrictSectorDevice {
            data,
            physical_block_size: SECTOR_SIZE as u32,
            reads: 0,
        });
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
    fn filesystem_geometry_accepts_blocks_smaller_than_the_physical_block() {
        let data = vec![0; 16 * crate::config::BLOCK_SIZE];
        let mut dev = BlockDev::new(StrictSectorDevice {
            data,
            physical_block_size: 4096,
            reads: 0,
        });

        dev.set_filesystem_block_size(1024)
            .expect("physical blocks do not change logical-sector addressing");
    }

    #[test]
    fn filesystem_geometry_rejects_invalid_physical_blocks() {
        let data = vec![0; 16 * crate::config::BLOCK_SIZE];
        let mut dev = BlockDev::new(StrictSectorDevice {
            data,
            physical_block_size: 1536,
            reads: 0,
        });

        assert_eq!(
            dev.set_filesystem_block_size(crate::config::BLOCK_SIZE)
                .expect_err("non-power-of-two physical geometry is invalid")
                .kind(),
            crate::Ext4ErrorKind::BadSuperblock
        );
    }

    #[test]
    fn filesystem_geometry_normalizes_an_unreported_physical_block() {
        let data = vec![0; 16 * crate::config::BLOCK_SIZE];
        let mut dev = BlockDev::new(StrictSectorDevice {
            data,
            physical_block_size: 0,
            reads: 0,
        });

        dev.set_filesystem_block_size(1024)
            .expect("an unreported physical size defaults to the logical size");
    }

    #[test]
    fn byte_offset_io_reads_superblock_across_device_sectors() {
        let mut data = vec![0; 16 * crate::config::BLOCK_SIZE];
        for (index, byte) in data[1024..2048].iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(17);
        }
        let expected = data[1024..2048].to_vec();
        let mut dev = BlockDev::new(StrictSectorDevice {
            data,
            physical_block_size: SECTOR_SIZE as u32,
            reads: 0,
        });
        let mut superblock = [0; 1024];

        dev.read_device_bytes(1024, &mut superblock)
            .expect("read the superblock by byte offset");

        assert_eq!(superblock.as_slice(), expected.as_slice());
    }

    #[test]
    fn private_layer_holds_only_the_current_rmw_block() {
        let data = vec![0; 16 * crate::config::BLOCK_SIZE];
        let mut dev = BlockDev::new(StrictSectorDevice {
            data,
            physical_block_size: SECTOR_SIZE as u32,
            reads: 0,
        });

        for block in 1..=5 {
            dev.read_block(AbsoluteBN::new(block))
                .expect("populate the cache working set");
        }
        dev.read_block(AbsoluteBN::new(1))
            .expect("reuse the first cached block");

        assert_eq!(dev._device().reads, 6);
    }

    #[test]
    fn held_image_cannot_be_written_to_a_different_block() {
        let data = vec![0; 16 * crate::config::BLOCK_SIZE];
        let target = AbsoluteBN::new(2);
        let mut dev = BlockDev::new(StrictSectorDevice {
            data,
            physical_block_size: SECTOR_SIZE as u32,
            reads: 0,
        });

        dev.read_block(target).unwrap();
        dev.read_block(AbsoluteBN::new(1)).unwrap();
        dev.buffer_mut().fill(0x5a);
        let error = dev
            .write_block(target)
            .expect_err("a held image has one physical-block identity");

        assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
        assert!(dev.has_unpublished_edit());
        dev.discard_held();
        let inner = dev.into_inner();
        assert!(inner.data.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn invalidating_detached_block_discards_dirty_buffer() {
        let mut data = vec![0; 16 * crate::config::BLOCK_SIZE];
        let target = AbsoluteBN::new(2);
        let start = target.as_usize().unwrap() * crate::config::BLOCK_SIZE;
        data[start..start + crate::config::BLOCK_SIZE].fill(0x11);
        let mut dev = BlockDev::new(StrictSectorDevice {
            data,
            physical_block_size: SECTOR_SIZE as u32,
            reads: 0,
        });

        dev.read_block(target).expect("cache detached metadata");
        dev.buffer_mut().fill(0x22);
        dev.discard_block(target);
        dev.flush().expect("flush remaining cache state");

        let inner = dev.into_inner();
        assert!(
            inner.data[start..start + crate::config::BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == 0x11)
        );
    }

    #[test]
    fn switching_does_not_write_unpublished_metadata_home() {
        let mut data = vec![0; 16 * crate::config::BLOCK_SIZE];
        let target = AbsoluteBN::new(2);
        let start = target.as_usize().unwrap() * crate::config::BLOCK_SIZE;
        data[start..start + crate::config::BLOCK_SIZE].fill(0x11);
        let mut dev = BlockDev::new(StrictSectorDevice {
            data,
            physical_block_size: SECTOR_SIZE as u32,
            reads: 0,
        });

        dev.read_block(target).expect("hold metadata block");
        dev.buffer_mut().fill(0x22);
        let error = dev
            .read_block(AbsoluteBN::new(3))
            .expect_err("JBD2 must publish a metadata edit before switching blocks");

        assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
        assert!(dev.has_unpublished_edit());
        dev.discard_held();
        let inner = dev.into_inner();
        assert!(
            inner.data[start..start + crate::config::BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == 0x11)
        );
    }

    #[test]
    fn filesystem_fua_write_preserves_one_multi_sector_request() {
        let device = DurabilityDevice {
            data: vec![0; 16 * crate::config::BLOCK_SIZE],
            capabilities: DeviceCapabilities {
                flush: true,
                fua: true,
                ..DeviceCapabilities::default()
            },
            last_write: None,
            flushes: 0,
            fail_flush: false,
        };
        let mut dev = BlockDev::new(device);
        let block = AbsoluteBN::new(2);
        let buffer = vec![0x5a; crate::config::BLOCK_SIZE];

        FilesystemBlockIo::write_with_flags(
            &mut dev,
            &buffer,
            block,
            1,
            WriteFlags::METADATA | WriteFlags::FUA,
        )
        .expect("write one durable filesystem block");

        let inner = dev.into_inner();
        assert_eq!(
            inner.last_write,
            Some((
                2 * (crate::config::BLOCK_SIZE / SECTOR_SIZE) as u64,
                (crate::config::BLOCK_SIZE / SECTOR_SIZE) as u32,
                WriteFlags::METADATA | WriteFlags::FUA,
            ))
        );
        assert_eq!(inner.flushes, 0);
    }

    #[test]
    fn filesystem_fua_falls_back_to_write_then_flush() {
        let device = DurabilityDevice {
            data: vec![0; 16 * crate::config::BLOCK_SIZE],
            capabilities: DeviceCapabilities {
                flush: true,
                fua: false,
                ..DeviceCapabilities::default()
            },
            last_write: None,
            flushes: 0,
            fail_flush: false,
        };
        let mut dev = BlockDev::new(device);
        let buffer = vec![0xa5; crate::config::BLOCK_SIZE];

        FilesystemBlockIo::write_with_flags(
            &mut dev,
            &buffer,
            AbsoluteBN::new(1),
            1,
            WriteFlags::METADATA | WriteFlags::FUA,
        )
        .expect("emulate FUA with an explicit flush");

        let inner = dev.into_inner();
        assert_eq!(
            inner.last_write,
            Some((
                (crate::config::BLOCK_SIZE / SECTOR_SIZE) as u64,
                (crate::config::BLOCK_SIZE / SECTOR_SIZE) as u32,
                WriteFlags::METADATA,
            ))
        );
        assert_eq!(inner.flushes, 1);
    }

    #[test]
    fn filesystem_fua_requires_fua_or_flush_capability() {
        let device = DurabilityDevice {
            data: vec![0; 16 * crate::config::BLOCK_SIZE],
            capabilities: DeviceCapabilities::default(),
            last_write: None,
            flushes: 0,
            fail_flush: false,
        };
        let mut dev = BlockDev::new(device);
        let buffer = vec![0xa5; crate::config::BLOCK_SIZE];

        let error = FilesystemBlockIo::write_with_flags(
            &mut dev,
            &buffer,
            AbsoluteBN::new(1),
            1,
            WriteFlags::FUA,
        )
        .expect_err("durability cannot be fabricated without FUA or flush");

        assert_eq!(error.kind(), crate::Ext4ErrorKind::UnsupportedCapability);
        let inner = dev.into_inner();
        assert_eq!(inner.last_write, None);
        assert_eq!(inner.flushes, 0);
    }

    #[test]
    fn filesystem_fua_fallback_flush_error_keeps_cache_coherent() {
        let device = DurabilityDevice {
            data: vec![0; 16 * crate::config::BLOCK_SIZE],
            capabilities: DeviceCapabilities {
                flush: true,
                fua: false,
                ..DeviceCapabilities::default()
            },
            last_write: None,
            flushes: 0,
            fail_flush: true,
        };
        let mut dev = BlockDev::new(device);
        let target = AbsoluteBN::new(1);
        dev.read_block(target).expect("prime the cached target");
        let buffer = vec![0x5a; crate::config::BLOCK_SIZE];

        let error =
            FilesystemBlockIo::write_with_flags(&mut dev, &buffer, target, 1, WriteFlags::FUA)
                .expect_err("fallback flush failure must propagate");

        assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);
        dev.read_block(target)
            .expect("read the coherent cached target");
        assert!(dev.buffer().iter().all(|byte| *byte == 0x5a));
        let inner = dev.into_inner();
        assert_eq!(inner.flushes, 1);
    }
}
