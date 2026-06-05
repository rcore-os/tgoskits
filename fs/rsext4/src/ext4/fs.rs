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
    pub inodetable_cahce: InodeCache,
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
}

impl Ext4FileSystem {
    pub(crate) fn sync_group_descriptor_if_needed<B: BlockDevice>(
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
        let gdt_base = BLOCK_SIZE as u64;
        let byte_offset = gdt_base + idx as u64 * desc_size as u64;
        let block_num = AbsoluteBN::new(byte_offset / BLOCK_SIZE as u64);
        let in_block = (byte_offset % BLOCK_SIZE as u64) as usize;
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
    pub fn inode_num_already_allocted<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> bool {
        let (group_idx, inode_in_group) = match self.inode_allocator.global_to_group(inode_num) {
            Ok(ids) => ids,
            Err(_) => return false,
        };
        let desc = match group_idx
            .as_usize()
            .ok()
            .and_then(|idx| self.group_descs.get(idx))
        {
            Some(d) => d,
            None => {
                warn!(
                    "inode_num_already_allocted: invalid group_idx {group_idx} for inode \
                     {inode_num}"
                );
                return false;
            }
        };
        let bitmap_block = AbsoluteBN::new(desc.inode_bitmap());
        let cache_key = CacheKey::new_inode(group_idx);

        let bitmap = match self
            .bitmap_cache
            .get_or_load_mut(device, cache_key, bitmap_block)
        {
            Ok(b) => b,
            Err(e) => {
                warn!("inode_num_already_allocted: load inode bitmap failed: {e:?}");
                return false;
            }
        };

        let bm = InodeBitmap::new(&mut bitmap.data, self.superblock.s_inodes_per_group);
        match bm.is_allocated(inode_in_group.raw()) {
            Some(allocated) => allocated,
            None => {
                warn!("inode_num_already_allocted: inode_in_group {inode_in_group} out of range");
                false
            }
        }
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
        B: BlockDevice,
        F: FnOnce(&mut Ext4Inode),
    {
        // Resolve the owning group first so the inode-table start block can be
        // derived from the matching group descriptor.
        let (group_idx, _idx_in_group) = self.inode_allocator.global_to_group(inode_num)?;

        let inode_table_start = self
            .group_descs
            .get(group_idx.as_usize()?)
            .ok_or(Ext4Error::corrupted())?
            .inode_table();

        let (block_num, offset, _g) = self.inodetable_cahce.calc_inode_location(
            inode_num,
            self.superblock.s_inodes_per_group,
            AbsoluteBN::new(inode_table_start),
            BLOCK_SIZE,
        )?;

        let sb = self.superblock;
        let inode_size = self.inode_disk_size() as usize;
        let has_csum = ext4_superblock_has_metadata_csum(&sb);

        let wrapped_f = move |inode: &mut Ext4Inode| {
            f(inode);
            if has_csum {
                ext4_update_inode_checksum(&sb, inode_num, inode.i_generation, inode, inode_size);
            }
        };

        self.inodetable_cahce
            .modify(block_dev, inode_num, block_num, offset, wrapped_f)
    }

    /// Loads one inode by number through the inode-table cache.
    pub fn get_inode_by_num<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<Ext4Inode> {
        let (group_idx, _idx_in_group) = self.inode_allocator.global_to_group(inode_num)?;

        let inode_table_start = self
            .group_descs
            .get(group_idx.as_usize()?)
            .ok_or(Ext4Error::corrupted())?
            .inode_table();

        let (block_num, offset, _g) = self.inodetable_cahce.calc_inode_location(
            inode_num,
            self.superblock.s_inodes_per_group,
            AbsoluteBN::new(inode_table_start),
            BLOCK_SIZE,
        )?;

        let cached = self
            .inodetable_cahce
            .get_or_load(block_dev, inode_num, block_num, offset)?;
        Ok(cached.inode)
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
