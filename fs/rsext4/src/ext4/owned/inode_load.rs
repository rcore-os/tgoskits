//! Demand reads of pinned inode records, with validated cache publication.

use alloc::sync::Arc;

use super::*;
use crate::{
    ForkBlockIo, SectorId,
    bmalloc::AbsoluteBN,
    cache::inode_table::{CachedInode, InodeLoadVersion},
};

/// A live inode is already cached, or owns a lock-external table read.
#[derive(Debug)]
pub enum LiveInodeRead<D: BlockIo> {
    /// Complete canonical metadata acquired while holding mount exclusion.
    Cached(InodeInfo),
    /// Execute without mount exclusion, then validate on the same mount.
    Pending(PreparedLiveInodeRead<D>),
}

#[derive(Debug)]
struct InodeLoadIdentity {
    mount: Arc<()>,
    version: InodeLoadVersion,
    number: InodeNumber,
    block: AbsoluteBN,
    offset: usize,
    inode_size: usize,
}

enum InodeTableSource<D> {
    Visible(Vec<u8>),
    Home {
        endpoint: D,
        sector: SectorId,
        count: u32,
        block_size: usize,
    },
}

/// One pinned inode's pending metadata read, not a second inode cache.
/// Keep allocation lifetime and mount admission until completion or cancellation.
#[must_use = "execute outside mount exclusion and validate before publication"]
pub struct PreparedLiveInodeRead<D: BlockIo> {
    identity: InodeLoadIdentity,
    source: InodeTableSource<D>,
}

/// Private table bytes or their I/O error, still subject to version validation.
#[derive(Debug)]
#[must_use = "validate on the originating mount before consuming this result"]
pub struct CompletedLiveInodeRead {
    identity: InodeLoadIdentity,
    bytes: Ext4Result<Vec<u8>>,
}

impl<D: BlockIo> PreparedLiveInodeRead<D> {
    /// Reads a single block. Errors remain private until the mount checks that
    /// no mutation or rollback superseded the operation they belong to.
    pub fn execute(self) -> CompletedLiveInodeRead {
        let bytes = match self.source {
            InodeTableSource::Visible(bytes) => Ok(bytes),
            InodeTableSource::Home {
                mut endpoint,
                sector,
                count,
                block_size,
            } => (|| {
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(block_size)
                    .map_err(|_| Ext4Error::no_memory())?;
                bytes.resize(block_size, 0);
                endpoint.read(&mut bytes, sector, count)?;
                Ok(bytes)
            })(),
        };
        CompletedLiveInodeRead {
            identity: self.identity,
            bytes,
        }
    }
}

impl<D: BlockIo> core::fmt::Debug for PreparedLiveInodeRead<D> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PreparedLiveInodeRead")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    /// Prepares inspection of an inode whose allocation the caller already
    /// retains. Unlike `inode`, this does not check an arbitrary inode number's
    /// allocation bitmap. Acquire the reference with authoritative lookup and
    /// retain it across preparation, I/O and completion, including all retries.
    ///
    /// Returns `None` only when the device lacks an independent read endpoint;
    /// the caller may use serialized inspection in that compatibility case.
    /// Geometry, mount, endpoint and allocation failures remain explicit errors.
    pub fn prepare_live_inode_read(
        &mut self,
        number: InodeNumber,
    ) -> Ext4Result<Option<LiveInodeRead<D>>>
    where
        D: ForkBlockIo,
    {
        self.ensure_mounted("inode:prepare_live_read")?;
        if let Some(cached) = self.filesystem.inodetable_cache.get(number) {
            return self
                .inspect_inode(number, cached.inode)
                .map(|inode| Some(LiveInodeRead::Cached(inode)));
        }
        self.prepare_uncached_live_inode_read(number)
            .map(|prepared| prepared.map(LiveInodeRead::Pending))
    }

    pub(super) fn prepare_uncached_live_inode_read(
        &mut self,
        number: InodeNumber,
    ) -> Ext4Result<Option<PreparedLiveInodeRead<D>>>
    where
        D: ForkBlockIo,
    {
        let (block, offset) = self.filesystem.inode_table_location(number)?;
        let source = if let Some(bytes) = self.device.visible_block_image(block) {
            InodeTableSource::Visible(bytes.to_vec())
        } else {
            let endpoint = match self.device.fork_read_endpoint() {
                Ok(endpoint) => endpoint,
                Err(error) if error.kind() == Ext4ErrorKind::UnsupportedCapability => {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            let block_size = self.filesystem.block_size();
            let geometry = endpoint.geometry();
            let sector_size = geometry.logical_block_size as usize;
            if sector_size == 0 || !block_size.is_multiple_of(sector_size) {
                return Err(Ext4Error::invalid_input().with_operation("inode:load_geometry"));
            }
            let count =
                u32::try_from(block_size / sector_size).map_err(|_| Ext4Error::overflow())?;
            let sector = block
                .raw()
                .checked_mul(u64::from(count))
                .ok_or_else(Ext4Error::overflow)?;
            let end = sector
                .checked_add(u64::from(count))
                .ok_or_else(Ext4Error::overflow)?;
            if end > geometry.block_count {
                return Err(Ext4Error::corrupted().with_operation("inode:load_bounds"));
            }
            InodeTableSource::Home {
                endpoint,
                sector: SectorId::new(sector),
                count,
                block_size,
            }
        };
        let identity = InodeLoadIdentity {
            mount: self.device.read_mount_identity(),
            version: self.filesystem.inodetable_cache.prepare_load(number),
            number,
            block,
            offset,
            inode_size: self.filesystem.inode_disk_size() as usize,
        };
        Ok(Some(PreparedLiveInodeRead { identity, source }))
    }

    /// Validates identity and concurrent mutation before exposing metadata or
    /// a read error. `None` requests a fresh preparation on this same pinned
    /// inode. No dirty cache eviction, bitmap read or device I/O occurs here.
    pub fn finish_live_inode_read(
        &mut self,
        completed: CompletedLiveInodeRead,
    ) -> Ext4Result<Option<InodeInfo>> {
        self.finish_live_inode_record(completed)?
            .map(|(number, inode)| self.inspect_inode(number, inode))
            .transpose()
    }

    pub(super) fn finish_live_inode_record(
        &mut self,
        completed: CompletedLiveInodeRead,
    ) -> Ext4Result<Option<(InodeNumber, Ext4Inode)>> {
        self.ensure_mounted("inode:finish_live_read")?;
        let identity = completed.identity;
        if !self.device.owns_read_mount(&identity.mount) {
            return Err(Ext4Error::invalid_input().with_operation("inode:foreign_live_read"));
        }
        let valid = self
            .filesystem
            .inodetable_cache
            .validate_load(&identity.version)?;
        if let Some(cached) = self.filesystem.inodetable_cache.get(identity.number) {
            return Ok(Some((identity.number, cached.inode)));
        }
        if !valid {
            return Ok(None);
        }
        let bytes = completed.bytes?;
        let end = identity
            .offset
            .checked_add(identity.inode_size)
            .ok_or_else(Ext4Error::overflow)?;
        let raw = bytes
            .get(identity.offset..end)
            .ok_or_else(Ext4Error::corrupted)?
            .to_vec();
        let inode = Ext4Inode::decode_checked(&raw)?;
        // Validate the same metadata conversion before inserting a new cache
        // record. In particular malformed device numbers cannot be published.
        self.inspect_inode(identity.number, inode)?;
        let record = CachedInode::new(inode, raw, identity.number, identity.block, identity.offset);
        Ok(self
            .filesystem
            .inodetable_cache
            .publish_load(&identity.version, record)?
            .map(|inode| (identity.number, inode)))
    }
}
