use super::*;

/// Mounted ext4 filesystem state.
///
/// This aggregates the superblock, group descriptors, allocators, and caches
/// needed after mount has reconstructed the filesystem view.
pub struct Ext4FileSystem {
    /// In-memory copy of the primary superblock.
    pub superblock: Ext4Superblock,
    /// All loaded block-group descriptors.
    pub group_descs: Vec<Ext4GroupDesc>,
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
    /// Physical block containing the externalized journal superblock.
    pub journal_sb_block_start: Option<AbsoluteBN>,
    /// Immutable index of filesystem metadata blocks protected from file mappings.
    pub(crate) system_zones: SystemZoneMap,
}

impl Ext4FileSystem {
    /// Returns the validated filesystem block size for this mount.
    pub(crate) fn block_size(&self) -> usize {
        self.superblock.block_size() as usize
    }

    pub(crate) fn sync_group_descriptor_if_needed<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        group_id: BGIndex,
    ) -> Ext4Result<()> {
        if USE_MULTILEVEL_CACHE {
            return Ok(());
        }

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
        desc.update_checksum(&self.superblock, group_id.raw(), None, None);
        self.group_descs[idx] = desc;

        let mut raw_desc_bytes = [0u8; Ext4GroupDesc::EXT4_DESC_SIZE_64BIT];
        desc.to_disk_bytes(&mut raw_desc_bytes);

        block_dev.read_block(block_num)?;
        let buffer = block_dev.buffer_mut();
        if end > buffer.len() {
            return Err(Ext4Error::corrupted());
        }

        buffer[in_block..end].copy_from_slice(&raw_desc_bytes[..desc_size]);
        block_dev.write_block(block_num, true)?;
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

        let mut bitmap = self
            .bitmap_cache
            .get_or_load(device, cache_key, bitmap_block)?;

        let bm = InodeBitmap::new(&mut bitmap.data, self.superblock.s_inodes_per_group);
        bm.is_allocated(inode_in_group.raw())
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
        group_idx
            .as_usize()
            .ok()
            .and_then(|idx| self.group_descs.get_mut(idx))
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
