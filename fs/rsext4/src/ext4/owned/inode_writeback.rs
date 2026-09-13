//! Bounded inode-table home reads outside mounted filesystem exclusion.

use super::*;
use crate::{ForkBlockIo, SectorId, blockdev::MetadataReadVersion, bmalloc::AbsoluteBN};

/// Read-only I/O for cold table blocks selected from the current dirty inodes.
///
/// Keep the mount's commit owner across preparation, execution and staging.
/// Other inode operations may proceed. Dropping a plan never clears dirty
/// records or publishes a journal update.
#[must_use = "execute outside filesystem exclusion, then stage on the same mount"]
pub struct PreparedInodeTableRead<D: BlockIo> {
    version: MetadataReadVersion,
    endpoint: D,
    blocks: Vec<AbsoluteBN>,
    block_size: usize,
    sectors_per_block: u32,
}

/// Temporary home bytes, not a cache or an authority over current inode data.
#[derive(Debug)]
#[must_use = "stage only on the originating mount before any commit or checkpoint"]
pub struct CompletedInodeTableRead {
    version: MetadataReadVersion,
    blocks: Vec<(AbsoluteBN, Vec<u8>)>,
}

impl<D: BlockIo> core::fmt::Debug for PreparedInodeTableRead<D> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PreparedInodeTableRead")
            .field("version", &self.version)
            .field("blocks", &self.blocks)
            .finish_non_exhaustive()
    }
}

impl<D: BlockIo> PreparedInodeTableRead<D> {
    /// Reads the selected home blocks without modifying mount state. Device
    /// and allocation failures retain every dirty inode for a later retry.
    pub fn execute(mut self) -> Ext4Result<CompletedInodeTableRead> {
        let mut blocks = Vec::new();
        blocks
            .try_reserve_exact(self.blocks.len())
            .map_err(|_| Ext4Error::no_memory())?;
        for block in self.blocks {
            let sector = block
                .raw()
                .checked_mul(u64::from(self.sectors_per_block))
                .ok_or_else(Ext4Error::overflow)?;
            let end = sector
                .checked_add(u64::from(self.sectors_per_block))
                .ok_or_else(Ext4Error::overflow)?;
            if end > self.endpoint.geometry().block_count {
                return Err(Ext4Error::invalid_input().with_operation("inode_table:read_bounds"));
            }
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(self.block_size)
                .map_err(|_| Ext4Error::no_memory())?;
            bytes.resize(self.block_size, 0);
            self.endpoint
                .read(&mut bytes, SectorId::new(sector), self.sectors_per_block)?;
            blocks.push((block, bytes));
        }
        Ok(CompletedInodeTableRead {
            version: self.version,
            blocks,
        })
    }
}

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    /// Selects cold inode-table blocks without reading device contents.
    /// Returns `None` for synchronous mode, no cold dirty blocks, or an
    /// unsupported independent endpoint. Other failures remain errors.
    pub fn prepare_inode_table_read(&self) -> Ext4Result<Option<PreparedInodeTableRead<D>>>
    where
        D: ForkBlockIo,
    {
        self.ensure_writable("inode_table:prepare_read")?;
        let Some(version) = self.device.metadata_read_version()? else {
            return Ok(None);
        };
        let mut blocks = self.filesystem.inodetable_cache.dirty_blocks();
        blocks.retain(|block| self.device.visible_block_image(*block).is_none());
        if blocks.is_empty() {
            return Ok(None);
        }
        let endpoint = match self.device.fork_read_endpoint() {
            Ok(endpoint) => endpoint,
            Err(error) if error.kind() == Ext4ErrorKind::UnsupportedCapability => return Ok(None),
            Err(error) => return Err(error),
        };
        let block_size = self.filesystem.block_size();
        let sector_size = endpoint.geometry().logical_block_size as usize;
        if sector_size == 0 || !block_size.is_multiple_of(sector_size) {
            return Err(Ext4Error::invalid_input().with_operation("inode_table:read_geometry"));
        }
        let sectors_per_block =
            u32::try_from(block_size / sector_size).map_err(|_| Ext4Error::overflow())?;
        Ok(Some(PreparedInodeTableRead {
            version,
            endpoint,
            blocks,
            block_size,
            sectors_per_block,
        }))
    }

    /// Merges current dirty records into valid pre-read blocks, preferring the
    /// journal's latest whole-block image. This performs no home read or
    /// commit I/O. Continue sync preparation under the same mount exclusion.
    ///
    /// # Errors
    /// Rejects foreign sessions and obsolete commit epochs before mutation.
    /// Journal pressure may stage a valid prefix; drive progress and prepare
    /// fresh reads before retrying. Unstaged inodes remain dirty on failure.
    pub fn stage_inode_table_read(
        &mut self,
        completed: &CompletedInodeTableRead,
    ) -> Ext4Result<()> {
        self.ensure_writable("inode_table:stage_read")?;
        self.device.validate_metadata_read(&completed.version)?;
        self.filesystem
            .inodetable_cache
            .flush_pre_read(&mut self.device, &completed.blocks)
    }
}
