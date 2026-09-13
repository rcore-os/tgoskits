use super::*;
use crate::blockdev::TransactionCredits;

/// Mounted ext4 filesystem state.
///
/// This aggregates the superblock, group descriptors, allocators, and caches
/// needed after mount has reconstructed the filesystem view.
pub struct Ext4FileSystem {
    /// In-memory copy of the primary superblock.
    pub superblock: Ext4Superblock,
    /// Whether the in-memory primary superblock still needs publication.
    pub(crate) superblock_dirty: bool,
    /// All loaded block-group descriptors.
    pub group_descs: Vec<Ext4GroupDesc>,
    /// Groups whose in-memory descriptors have not yet been published.
    pub(crate) dirty_group_descs: Vec<bool>,
    /// Data-block allocator state.
    pub block_allocator: BlockAllocator,
    /// Inode allocator state.
    pub inode_allocator: InodeAllocator,
    /// Bitmap cache with lazy loading and eviction.
    pub bitmap_cache: BitmapCache,
    /// Inode-table cache.
    pub inodetable_cache: InodeCache,
    /// Data-block cache.
    pub datablock_cache: DataBlockCache,
    /// Root inode number, normally inode 2.
    pub root_inode: InodeNumber,
    /// Total number of block groups.
    pub group_count: u32,
    /// Mount state flag.
    pub mounted: bool,
    /// Multi-mount protection ownership for writable MMP filesystems.
    pub(crate) mmp: super::mmp::MmpState,
    /// Physical block containing the externalized journal superblock.
    pub journal_sb_block_start: Option<AbsoluteBN>,
    /// Immutable index of filesystem metadata blocks protected from file mappings.
    pub(crate) system_zones: SystemZoneMap,
}

/// In-memory filesystem metadata restored when one journal handle aborts.
///
/// File payload writes are not journalled by this owner. Data-cache state is
/// snapshotted because a metadata transition may invalidate blocks after it
/// detaches their mappings; abort must restore the old cache visibility.
/// Operations that change payload bytes still need an ordered-data owner.
struct MetadataTransactionSnapshot {
    superblock: Ext4Superblock,
    superblock_dirty: bool,
    group_descs: Vec<Ext4GroupDesc>,
    dirty_group_descs: Vec<bool>,
    bitmap_cache: BitmapCache,
    inodetable_cache: InodeCache,
    datablock_cache: DataBlockCache,
}

impl MetadataTransactionSnapshot {
    fn capture(filesystem: &Ext4FileSystem) -> Self {
        Self {
            superblock: filesystem.superblock,
            superblock_dirty: filesystem.superblock_dirty,
            group_descs: filesystem.group_descs.clone(),
            dirty_group_descs: filesystem.dirty_group_descs.clone(),
            bitmap_cache: filesystem.bitmap_cache.clone(),
            inodetable_cache: filesystem.inodetable_cache.clone(),
            datablock_cache: filesystem.datablock_cache.clone(),
        }
    }

    fn restore(self, filesystem: &mut Ext4FileSystem) {
        filesystem.superblock = self.superblock;
        filesystem.superblock_dirty = self.superblock_dirty;
        filesystem.group_descs = self.group_descs;
        filesystem.dirty_group_descs = self.dirty_group_descs;
        filesystem.bitmap_cache = self.bitmap_cache;
        filesystem.inodetable_cache = self.inodetable_cache;
        filesystem.datablock_cache = self.datablock_cache;
    }
}

#[derive(Clone, Copy)]
pub(crate) struct GroupCounters {
    free_blocks: u32,
    free_inodes: u32,
    used_dirs: u32,
}

impl Ext4FileSystem {
    /// Runs one metadata state transition under a matching filesystem and
    /// journal transaction owner.
    ///
    /// JBD2 restores its queued block images on error; this layer restores the
    /// corresponding in-memory allocation, descriptor, inode, and superblock
    /// state. The operation must publish every dirty metadata cache entry to
    /// `block_dev` before returning success so all images consume the same
    /// bounded journal handle.
    pub(crate) fn with_metadata_transaction<B: BlockIo, T>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        credits: impl Into<TransactionCredits>,
        operation: impl FnOnce(&mut Self, &mut Jbd2Dev<B>) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        let snapshot = MetadataTransactionSnapshot::capture(self);
        let result = block_dev
            .with_transaction_credits(credits.into(), |block_dev| operation(self, block_dev));
        if result.is_err() {
            snapshot.restore(self);
        }
        result
    }

    /// Ends the previous handle's transaction before running the next
    /// restartable metadata step under a fresh handle.
    ///
    /// The snapshot covers only the new step. Earlier steps may already be
    /// durable and must describe crash-restartable progress on disk; restoring
    /// the state from before the whole multi-transaction operation would make
    /// memory disagree with replayable metadata.
    pub(crate) fn restart_metadata_transaction<B: BlockIo, T>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        credits: impl Into<TransactionCredits>,
        operation: impl FnOnce(&mut Self, &mut Jbd2Dev<B>) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        let snapshot = MetadataTransactionSnapshot::capture(self);
        let result =
            block_dev.restart_transaction(credits.into(), |block_dev| operation(self, block_dev));
        if result.is_err() {
            snapshot.restore(self);
        }
        result
    }

    pub(crate) fn group_counter_snapshot(&self) -> Vec<GroupCounters> {
        self.group_descs
            .iter()
            .map(|descriptor| GroupCounters {
                free_blocks: descriptor.free_blocks_count(),
                free_inodes: descriptor.free_inodes_count(),
                used_dirs: descriptor.used_dirs_count(),
            })
            .collect()
    }

    /// Publishes only allocation-group metadata changed since `before`.
    pub(crate) fn flush_changed_group_metadata<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        before: &[GroupCounters],
    ) -> Ext4Result<()> {
        if before.len() != self.group_descs.len() {
            return Err(Ext4Error::corrupted().with_operation("transaction:group_count"));
        }
        for (index, previous) in before.iter().copied().enumerate() {
            let group = BGIndex::new(u32::try_from(index).map_err(|_| Ext4Error::overflow())?);
            let descriptor = self
                .get_group_desc(group)
                .ok_or_else(Ext4Error::corrupted)?;
            let free_blocks_changed = descriptor.free_blocks_count() != previous.free_blocks;
            let free_inodes_changed = descriptor.free_inodes_count() != previous.free_inodes;
            let used_dirs_changed = descriptor.used_dirs_count() != previous.used_dirs;

            if free_blocks_changed {
                self.bitmap_cache
                    .flush(block_dev, &CacheKey::new_block(group))?;
            }
            if free_inodes_changed {
                self.bitmap_cache
                    .flush(block_dev, &CacheKey::new_inode(group))?;
            }
            if free_blocks_changed || free_inodes_changed || used_dirs_changed {
                self.sync_group_descriptor(block_dev, group)?;
            }
        }
        Ok(())
    }

    /// Publishes allocation metadata for an explicit set of block groups.
    ///
    /// Callers that allocate and free blocks in one transaction cannot infer
    /// dirty bitmaps from the final free-block counters: both changes may
    /// cancel while the bitmap still replaces one physical block with another.
    pub(crate) fn flush_block_allocation_groups<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        groups: &[BGIndex],
    ) -> Ext4Result<()> {
        for group in groups.iter().copied() {
            self.bitmap_cache
                .flush(block_dev, &CacheKey::new_block(group))?;
            self.sync_group_descriptor(block_dev, group)?;
        }
        Ok(())
    }

    /// Returns the validated filesystem block size for this mount.
    pub(crate) fn block_size(&self) -> usize {
        self.superblock.block_size() as usize
    }

    /// Decodes `i_size` using this filesystem's `LARGEDIR` policy.
    pub(crate) fn inode_size(&self, inode: &Ext4Inode) -> u64 {
        inode.size_in_filesystem(
            self.superblock
                .has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_LARGEDIR),
        )
    }

    pub(crate) fn sync_group_descriptor_if_needed<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        group_id: BGIndex,
    ) -> Ext4Result<()> {
        if USE_MULTILEVEL_CACHE {
            return Ok(());
        }

        self.sync_group_descriptor(block_dev, group_id)
    }

    /// Writes the descriptor containing `group_id` regardless of cache mode.
    /// Metadata transactions use this after flushing the matching bitmap so
    /// both images are queued under the same journal handle.
    pub(crate) fn sync_group_descriptor<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        group_id: BGIndex,
    ) -> Ext4Result<()> {
        let idx = group_id.as_usize()?;
        if idx >= self.group_descs.len() {
            return Err(Ext4Error::corrupted());
        }

        let desc_size = self.superblock.get_desc_size() as usize;
        let block_size = self.block_size() as u64;
        let gdt_base = self.superblock.primary_gdt_byte_offset()?;
        let byte_offset = gdt_base + idx as u64 * desc_size as u64;
        let block_num = AbsoluteBN::new(byte_offset / block_size);
        let in_block = (byte_offset % block_size) as usize;
        let end = in_block + desc_size;

        let mut desc = self.group_descs[idx];
        block_dev.update_block(block_num, true, |buffer| {
            if end > buffer.len() {
                return Err(Ext4Error::corrupted());
            }
            desc.encode_with_checksum(
                &self.superblock,
                group_id.raw(),
                &mut buffer[in_block..end],
                None,
                None,
            )
        })?;
        self.group_descs[idx] = desc;
        *self
            .dirty_group_descs
            .get_mut(idx)
            .ok_or_else(Ext4Error::corrupted)? = false;
        Ok(())
    }

    /// Returns whether the given inode number is marked allocated in its bitmap.
    pub fn inode_num_already_allocated<B: BlockIo>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<bool> {
        self.inode_is_allocated_checked(device, inode_num)
    }

    /// Checks the allocation bitmap without collapsing I/O or corruption into
    /// an ordinary "free" answer.
    pub(crate) fn inode_is_allocated_checked<B: BlockIo>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<bool> {
        let (group_idx, inode_in_group) = self.inode_allocator.global_to_group(inode_num)?;
        let desc = group_idx
            .as_usize()
            .ok()
            .and_then(|idx| self.group_descs.get(idx))
            .ok_or_else(Ext4Error::corrupted)?;
        let bitmap_block = AbsoluteBN::new(desc.inode_bitmap());
        let cache_key = CacheKey::new_inode(group_idx);

        let bitmap = self
            .bitmap_cache
            .get_or_load(device, cache_key, bitmap_block)?;

        InodeBitmap::is_allocated_in(
            &bitmap.data,
            self.superblock.s_inodes_per_group,
            inode_in_group.raw(),
        )
        .ok_or_else(|| Ext4Error::corrupted().with_operation("inode:bitmap_range"))
    }

    /// Returns an immutable block-group descriptor by index.
    pub fn get_group_desc(&self, group_idx: BGIndex) -> Option<&Ext4GroupDesc> {
        group_idx
            .as_usize()
            .ok()
            .and_then(|idx| self.group_descs.get(idx))
    }

    /// Returns a mutable block-group descriptor by index.
    pub fn get_group_desc_mut(&mut self, group_idx: BGIndex) -> Option<&mut Ext4GroupDesc> {
        let idx = group_idx.as_usize().ok()?;
        *self.dirty_group_descs.get_mut(idx)? = true;
        self.superblock_dirty = true;
        self.group_descs.get_mut(idx)
    }

    pub(crate) fn mark_superblock_dirty(&mut self) {
        self.superblock_dirty = true;
    }

    /// Modifies one inode via the inode-table cache.
    ///
    /// The helper resolves the inode-table block, loads the cached inode, runs
    /// the caller-supplied closure, and refreshes the inode checksum when the
    /// metadata checksum feature is enabled.
    pub fn modify_inode<B, F>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockIo,
        F: FnOnce(&mut Ext4Inode),
    {
        self.modify_inode_record(block_dev, inode_num, |inode, _raw_inode| {
            f(inode);
            Ok(())
        })
    }

    /// Mutates modeled fields and unmodeled bytes in one raw inode record.
    pub(crate) fn modify_inode_record<B, F>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        f: F,
    ) -> Ext4Result<()>
    where
        B: BlockIo,
        F: FnOnce(&mut Ext4Inode, &mut [u8]) -> Ext4Result<()>,
    {
        // Resolve the owning group first so the inode-table start block can be
        // derived from the matching group descriptor.
        let (group_idx, _idx_in_group) = self.inode_allocator.global_to_group(inode_num)?;

        let inode_table_start = self
            .group_descs
            .get(group_idx.as_usize()?)
            .ok_or(Ext4Error::corrupted())?
            .inode_table();

        let (block_num, offset, _g) = self.inodetable_cache.calc_inode_location(
            inode_num,
            self.superblock.s_inodes_per_group,
            AbsoluteBN::new(inode_table_start),
            self.block_size(),
        )?;

        let sb = self.superblock;
        let has_csum = ext4_superblock_has_metadata_csum(&sb);

        let wrapped_f = move |inode: &mut Ext4Inode, raw_inode: &mut [u8]| {
            f(inode, raw_inode)?;
            if has_csum {
                ext4_update_raw_inode_checksum(&sb, inode_num, inode, raw_inode)?;
            } else {
                inode.to_disk_bytes(raw_inode);
            }
            Ok(())
        };

        self.inodetable_cache
            .modify(block_dev, inode_num, block_num, offset, wrapped_f)
    }

    /// Prepares a newly allocated inode from a logically uninitialized table.
    pub(crate) fn initialize_zeroed_inode_record<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<()> {
        let (group_idx, _idx_in_group) = self.inode_allocator.global_to_group(inode_num)?;
        let inode_table_start = self
            .group_descs
            .get(group_idx.as_usize()?)
            .ok_or_else(Ext4Error::corrupted)?
            .inode_table();
        let (block_num, offset, _group) = self.inodetable_cache.calc_inode_location(
            inode_num,
            self.superblock.s_inodes_per_group,
            AbsoluteBN::new(inode_table_start),
            self.block_size(),
        )?;
        self.inodetable_cache
            .initialize_zeroed(block_dev, inode_num, block_num, offset)
    }

    /// Loads one inode by number through the inode-table cache.
    pub fn get_inode_by_num<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<Ext4Inode> {
        self.get_inode_record(block_dev, inode_num)
            .map(|(inode, _)| inode)
    }

    /// Loads one inode and its complete raw record through the inode-table cache.
    pub(crate) fn get_inode_record<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<(Ext4Inode, Vec<u8>)> {
        let (group_idx, _idx_in_group) = self.inode_allocator.global_to_group(inode_num)?;

        let inode_table_start = self
            .group_descs
            .get(group_idx.as_usize()?)
            .ok_or(Ext4Error::corrupted())?
            .inode_table();

        let (block_num, offset, _g) = self.inodetable_cache.calc_inode_location(
            inode_num,
            self.superblock.s_inodes_per_group,
            AbsoluteBN::new(inode_table_start),
            self.block_size(),
        )?;

        let cached = self
            .inodetable_cache
            .get_or_load(block_dev, inode_num, block_num, offset)?;
        Ok((cached.inode, cached.raw_inode().to_vec()))
    }

    /// Returns an aggregated statfs-style snapshot.
    pub fn statfs(&self) -> FileSystemStats {
        FileSystemStats {
            total_blocks: self.superblock.blocks_count(),
            free_blocks: self.superblock.free_blocks_count(),
            total_inodes: self.superblock.s_inodes_count,
            free_inodes: self.superblock.s_free_inodes_count,
            block_size: self.superblock.block_size(),
            block_groups: self.group_count,
        }
    }

    /// Placeholder for creating the minimal filesystem base layout.
    pub fn make_base_dir(&self) {
        // root, journal, and lost+found initialization is handled elsewhere.
    }
}

/// Filesystem-wide usage counters.
#[derive(Debug, Clone, Copy)]
pub struct FileSystemStats {
    /// Total block count.
    pub total_blocks: u64,
    /// Free block count.
    pub free_blocks: u64,
    /// Total inode count.
    pub total_inodes: u32,
    /// Free inode count.
    pub free_inodes: u32,
    /// Block size in bytes.
    pub block_size: u64,
    /// Number of block groups.
    pub block_groups: u32,
}
