//! Coherent inode block views shared by independent file and directory readers.

use alloc::{sync::Arc, vec::Vec};

use super::*;
use crate::{
    ForkBlockIo, SectorId,
    blockdev::MetadataBlockRead,
    bmalloc::AbsoluteBN,
    cache::inode_table::InodeLoadVersion,
    ext4::{BlockMapContext, SystemZoneMap},
    superblock::Ext4Superblock,
};

#[derive(Debug)]
pub(super) struct InodeReadSnapshot {
    mount: Arc<()>,
    version: InodeLoadVersion,
    pub(super) number: InodeNumber,
    pub(super) inode: Ext4Inode,
    pub(super) superblock: Ext4Superblock,
    system_zones: SystemZoneMap,
}

impl InodeReadSnapshot {
    pub(super) fn context(&self) -> BlockMapContext<'_> {
        BlockMapContext {
            superblock: &self.superblock,
            system_zones: &self.system_zones,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum InodeBlockKind {
    Mapping,
    Data,
}

/// Opaque visibility query whose address belongs to a protected inode reader.
/// Pass it unchanged to the originating mount's `inode_read_block_image`.
#[derive(Debug)]
pub struct InodeBlockRequest {
    snapshot: Arc<InodeReadSnapshot>,
    physical: AbsoluteBN,
    kind: InodeBlockKind,
}

/// Bounded contiguous data-image query, constructed only by a read owner.
/// Pass it unchanged to `inode_read_data_images` under short mount exclusion.
#[derive(Debug)]
pub struct InodeDataRequest {
    first: InodeBlockRequest,
    count: u32,
}

/// Short access to authoritative immutable inode block images, without I/O.
///
/// Implementations return the originating mount's result verbatim and release
/// mount exclusion before returning. A miss permits coherent endpoint I/O;
/// errors and dirty/journal images must never be replaced with home bytes.
pub trait InodeReadCache {
    /// Obtains one mapping or data image, or an authoritative cache miss.
    fn visible(&mut self, request: &InodeBlockRequest) -> Ext4Result<Option<Arc<Vec<u8>>>>;

    /// Obtains a bounded run in order. Override with `inode_read_data_images`
    /// to use one mounted critical section rather than one per block.
    fn visible_data(
        &mut self,
        request: &InodeDataRequest,
    ) -> Ext4Result<Vec<Option<Arc<Vec<u8>>>>> {
        request.collect_images(|block| self.visible(block))
    }
}

impl InodeDataRequest {
    fn collect_images(
        &self,
        mut visible: impl FnMut(&InodeBlockRequest) -> Ext4Result<Option<Arc<Vec<u8>>>>,
    ) -> Ext4Result<Vec<Option<Arc<Vec<u8>>>>> {
        let mut images = Vec::new();
        images
            .try_reserve_exact(self.count as usize)
            .map_err(|_| Ext4Error::no_memory())?;
        for index in 0..self.count {
            images.push(visible(&InodeBlockRequest {
                snapshot: self.first.snapshot.clone(),
                physical: self.first.physical.checked_add(index)?,
                kind: InodeBlockKind::Data,
            })?);
        }
        Ok(images)
    }
}

pub(super) struct PreparedBlockRead<D: BlockIo> {
    pub(super) snapshot: Arc<InodeReadSnapshot>,
    endpoint: D,
    sectors_per_block: u32,
    total_blocks: u64,
}

pub(super) struct IndependentBlockRead<'a, D: BlockIo, C> {
    plan: PreparedBlockRead<D>,
    cache: &'a mut C,
}

impl<D: BlockIo> PreparedBlockRead<D> {
    pub(super) fn execute<C: InodeReadCache>(
        self,
        cache: &mut C,
    ) -> IndependentBlockRead<'_, D, C> {
        IndependentBlockRead { plan: self, cache }
    }
}

impl<D: BlockIo, C: InodeReadCache> IndependentBlockRead<'_, D, C> {
    pub(super) fn snapshot(&self) -> &Arc<InodeReadSnapshot> {
        &self.plan.snapshot
    }

    pub(super) fn block_image(
        &mut self,
        physical: AbsoluteBN,
        kind: InodeBlockKind,
    ) -> Ext4Result<Arc<Vec<u8>>> {
        self.validate_range(physical, 1)?;
        let request = InodeBlockRequest {
            snapshot: self.plan.snapshot.clone(),
            physical,
            kind,
        };
        let block_size = self.plan.snapshot.context().block_size();
        if let Some(image) = self.cache.visible(&request)? {
            if image.len() != block_size {
                return Err(Ext4Error::corrupted().with_operation("inode:visible_size"));
            }
            return Ok(image);
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(block_size)
            .map_err(|_| Ext4Error::no_memory())?;
        bytes.resize(block_size, 0);
        self.read_run(physical, &mut bytes)?;
        Ok(Arc::new(bytes))
    }

    pub(super) fn data_images(
        &mut self,
        physical: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<Vec<Option<Arc<Vec<u8>>>>> {
        self.validate_range(physical, count)?;
        let images = self.cache.visible_data(&InodeDataRequest {
            first: InodeBlockRequest {
                snapshot: self.plan.snapshot.clone(),
                physical,
                kind: InodeBlockKind::Data,
            },
            count,
        })?;
        if images.len() != count as usize {
            return Err(Ext4Error::corrupted().with_operation("inode:visible_count"));
        }
        Ok(images)
    }

    pub(super) fn read_run(&mut self, physical: AbsoluteBN, bytes: &mut [u8]) -> Ext4Result<()> {
        let block_size = self.plan.snapshot.context().block_size();
        if !bytes.len().is_multiple_of(block_size) {
            return Err(Ext4Error::invalid_input().with_operation("inode:read_alignment"));
        }
        let blocks = u32::try_from(bytes.len() / block_size).map_err(|_| Ext4Error::overflow())?;
        self.validate_range(physical, blocks)?;
        let sector = physical
            .raw()
            .checked_mul(u64::from(self.plan.sectors_per_block))
            .ok_or_else(Ext4Error::overflow)?;
        let count = blocks
            .checked_mul(self.plan.sectors_per_block)
            .ok_or_else(Ext4Error::overflow)?;
        self.plan.endpoint.read(bytes, SectorId::new(sector), count)
    }

    fn validate_range(&self, physical: AbsoluteBN, count: u32) -> Ext4Result<()> {
        let end = physical
            .raw()
            .checked_add(u64::from(count))
            .ok_or_else(Ext4Error::overflow)?;
        if end > self.plan.total_blocks {
            return Err(Ext4Error::corrupted().with_operation("inode:read_bounds"));
        }
        Ok(())
    }
}

impl<D: BlockIo, C: InodeReadCache> MetadataBlockRead for IndependentBlockRead<'_, D, C> {
    fn total_blocks(&self) -> u64 {
        self.plan.total_blocks
    }

    fn with_block<T>(
        &mut self,
        physical: AbsoluteBN,
        inspect: impl FnOnce(&[u8]) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        let image = self.block_image(physical, InodeBlockKind::Mapping)?;
        inspect(&image)
    }
}

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    pub(super) fn prepare_inode_blocks(
        &mut self,
        number: InodeNumber,
        inode: Ext4Inode,
    ) -> Ext4Result<Option<PreparedBlockRead<D>>>
    where
        D: ForkBlockIo,
    {
        let endpoint = match self.device.fork_read_endpoint() {
            Ok(endpoint) => endpoint,
            Err(error) if error.kind() == Ext4ErrorKind::UnsupportedCapability => return Ok(None),
            Err(error) => return Err(error),
        };
        let geometry = endpoint.geometry();
        let sector_size = geometry.logical_block_size as usize;
        let block_size = self.filesystem.block_size();
        if sector_size == 0 || !block_size.is_multiple_of(sector_size) {
            return Err(Ext4Error::invalid_input().with_operation("inode:read_geometry"));
        }
        let sectors_per_block =
            u32::try_from(block_size / sector_size).map_err(|_| Ext4Error::overflow())?;
        if sectors_per_block == 0 {
            return Err(Ext4Error::invalid_input().with_operation("inode:read_geometry"));
        }
        Ok(Some(PreparedBlockRead {
            snapshot: Arc::new(InodeReadSnapshot {
                mount: self.device.read_mount_identity(),
                version: self.filesystem.inodetable_cache.prepare_load(number),
                number,
                inode,
                superblock: self.filesystem.superblock,
                system_zones: self.filesystem.system_zones.clone(),
            }),
            endpoint,
            sectors_per_block,
            total_blocks: geometry.block_count / u64::from(sectors_per_block),
        }))
    }

    pub(super) fn read_snapshot_is_current(
        &self,
        snapshot: &InodeReadSnapshot,
    ) -> Ext4Result<bool> {
        self.ensure_mounted("inode:validate_read")?;
        if !self.device.owns_read_mount(&snapshot.mount) {
            return Err(Ext4Error::invalid_input().with_operation("inode:foreign_read"));
        }
        self.filesystem
            .inodetable_cache
            .validate_load(&snapshot.version)
    }

    /// Queries one owner-produced mapping/data address without reading disk.
    /// The returned image retains its bytes across cache eviction/checkpoint.
    /// Superseded requests return a private error; the read's final validation
    /// requests retry before exposing that obsolete error to the caller.
    pub fn inode_read_block_image(
        &mut self,
        request: &InodeBlockRequest,
    ) -> Ext4Result<Option<Arc<Vec<u8>>>> {
        if !self.read_snapshot_is_current(&request.snapshot)? {
            return Err(Ext4Error::busy().with_operation("inode:superseded_read"));
        }
        Ok(self.visible_inode_block(request))
    }

    /// Snapshots a whole bounded data run under one mount critical section.
    /// Only immutable image references are collected; copying file data and
    /// reading missing blocks belong to the independent execution phase.
    pub fn inode_read_data_images(
        &mut self,
        request: &InodeDataRequest,
    ) -> Ext4Result<Vec<Option<Arc<Vec<u8>>>>> {
        if !self.read_snapshot_is_current(&request.first.snapshot)? {
            return Err(Ext4Error::busy().with_operation("inode:superseded_read"));
        }
        request.collect_images(|block| Ok(self.visible_inode_block(block)))
    }

    fn visible_inode_block(&mut self, request: &InodeBlockRequest) -> Option<Arc<Vec<u8>>> {
        if matches!(request.kind, InodeBlockKind::Data)
            && let Some(cached) = self.filesystem.datablock_cache.get_mut(request.physical)
        {
            return Some(cached.data);
        }
        self.device
            .visible_block_image(request.physical)
            .map(|bytes| Arc::new(bytes.to_vec()))
    }
}
