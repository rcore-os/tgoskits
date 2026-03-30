use super::*;

impl Ext4FileSystem {
    /// Allocates a contiguous run of data blocks anywhere in the filesystem.
    pub fn alloc_blocks<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        count: u32,
    ) -> Ext4Result<Vec<AbsoluteBN>> {
        if count == 0 {
            return Ok(Vec::new());
        }

        trace!("alloc_blocks: request count={count} (will scan groups for free space)");

        for (idx, desc) in self.group_descs.iter().enumerate() {
            let group_idx =
                BGIndex::new(u32::try_from(idx).map_err(|_| Ext4Error::from(Errno::EOVERFLOW))?);
            let free = desc.free_blocks_count();

            trace!("alloc_blocks: inspect group={group_idx} free_blocks={free} need={count}");
            if free < count {
                continue;
            }

            let bitmap_block = AbsoluteBN::new(desc.block_bitmap());
            let cache_key = CacheKey::new_block(group_idx);
            let mut alloc_res: Result<BlockAlloc, Ext4Error> = Err(Ext4Error::no_space());

            debug!(
                "alloc_blocks: candidate group={group_idx} bitmap_block={bitmap_block} starting \
                 contiguous allocation of {count} blocks"
            );

            if ext4_superblock_has_metadata_csum(&self.superblock) && !desc.is_block_bitmap_uninit()
            {
                // Validate the current bitmap contents before mutating them so
                // checksum-protected filesystems fail fast on corruption.
                let bm = self
                    .bitmap_cache
                    .get_or_load(block_dev, cache_key, bitmap_block)?;
                let expected = ext4_block_bitmap_csum32(&self.superblock, &bm.data);
                let stored = desc.block_bitmap_csum();
                if expected != stored {
                    error!(
                        "alloc_blocks: block bitmap checksum mismatch group={group_idx} \
                         expected={expected:#x} stored={stored:#x}"
                    );
                    return Err(Ext4Error::checksum());
                }
            }

            // The actual allocation happens under the bitmap-cache mutation so
            // the updated bitmap bytes remain coherent with cache state.
            self.bitmap_cache
                .modify(block_dev, cache_key, bitmap_block, |data| {
                    alloc_res = self
                        .block_allocator
                        .alloc_contiguous_blocks(data, group_idx, count);
                })?;

            if ext4_superblock_has_metadata_csum(&self.superblock) {
                let sb = self.superblock;
                let updated_data = self
                    .bitmap_cache
                    .get(&cache_key)
                    .ok_or(Ext4Error::corrupted())?
                    .data
                    .clone();
                let desc_mut = self
                    .get_group_desc_mut(group_idx)
                    .ok_or(Ext4Error::corrupted())?;
                desc_mut.update_checksum(&sb, group_idx.raw(), Some(&updated_data), None);
            }

            let alloc = alloc_res?;

            if let Some(desc_mut) = self.get_group_desc_mut(group_idx) {
                let before = desc_mut.free_blocks_count();
                let new_count = before.saturating_sub(count);
                desc_mut.bg_free_blocks_count_lo = (new_count & 0xFFFF) as u16;
                desc_mut.bg_free_blocks_count_hi = (new_count >> 16) as u16;
                desc_mut.bg_flags &= !Ext4GroupDesc::EXT4_BG_BLOCK_UNINIT;

                debug!(
                    "alloc_blocks: group={} free_blocks_count change {} -> {} (allocated {} \
                     blocks starting at global={})",
                    group_idx, before, new_count, count, alloc.global_block
                );
            }
            self.sync_group_descriptor_if_needed(block_dev, group_idx)?;

            let sb_before = self.superblock.free_blocks_count();
            let sb_after = sb_before.saturating_sub(count as u64);
            self.superblock.s_free_blocks_count_lo = (sb_after & 0xFFFF_FFFF) as u32;
            self.superblock.s_free_blocks_count_hi = (sb_after >> 32) as u32;

            debug!(
                "alloc_blocks: superblock free_blocks_count change {sb_before} -> {sb_after} \
                 (delta=-{count})"
            );

            let mut blocks = Vec::with_capacity(count as usize);
            for off in 0..count {
                blocks.push(alloc.global_block.checked_add(off)?);
            }

            debug!(
                "Allocated blocks: group={}, first_block_in_group={}, first_global_block={}, \
                 count={} [bitmap updated, writeback deferred]",
                alloc.group_idx, alloc.block_in_group, alloc.global_block, count
            );

            return Ok(blocks);
        }

        debug!("alloc_blocks: no group has enough free blocks for request count={count}");
        Err(Ext4Error::no_space())
    }

    pub fn alloc_block<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<AbsoluteBN> {
        self.alloc_blocks(block_dev, 1)?
            .into_iter()
            .next()
            .ok_or(Ext4Error::no_space())
    }

    /// Allocates the requested number of inodes across all groups.
    pub fn alloc_inodes<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        count: u32,
    ) -> Ext4Result<Vec<InodeNumber>> {
        if count == 0 {
            return Ok(Vec::new());
        }

        for (idx, desc) in self.group_descs.iter().enumerate() {
            let group_idx =
                BGIndex::new(u32::try_from(idx).map_err(|_| Ext4Error::from(Errno::EOVERFLOW))?);
            let free = desc.free_inodes_count();
            if free < count {
                continue;
            }

            let bitmap_block = AbsoluteBN::new(desc.inode_bitmap());
            let cache_key = CacheKey::new_inode(group_idx);
            let mut inodes: Vec<InodeNumber> = Vec::with_capacity(count as usize);
            let mut alloc_error: Option<Ext4Error> = None;

            if ext4_superblock_has_metadata_csum(&self.superblock) && !desc.is_inode_bitmap_uninit()
            {
                let bm = self
                    .bitmap_cache
                    .get_or_load(block_dev, cache_key, bitmap_block)?;
                let expected = ext4_inode_bitmap_csum32(&self.superblock, &bm.data);
                let stored = desc.inode_bitmap_csum();
                if expected != stored {
                    return Err(Ext4Error::checksum());
                }
            }

            // Allocate directly from the inode bitmap so the cache owns the
            // only mutable copy while we flip bits.
            self.bitmap_cache
                .modify(block_dev, cache_key, bitmap_block, |data| {
                    for _ in 0..count {
                        match self
                            .inode_allocator
                            .alloc_inode_in_group(data, group_idx, desc)
                        {
                            Ok(InodeAlloc { global_inode, .. }) => inodes.push(global_inode),
                            Err(err) if err.code == Errno::ENOSPC => break,
                            Err(err) => {
                                alloc_error = Some(err);
                                break;
                            }
                        }
                    }
                })?;

            if let Some(err) = alloc_error {
                return Err(err);
            }

            if ext4_superblock_has_metadata_csum(&self.superblock) {
                let sb = self.superblock;
                let updated_data = self
                    .bitmap_cache
                    .get(&cache_key)
                    .ok_or(Ext4Error::corrupted())?
                    .data
                    .clone();
                let desc_mut = self
                    .get_group_desc_mut(group_idx)
                    .ok_or(Ext4Error::corrupted())?;
                desc_mut.update_checksum(&sb, group_idx.raw(), None, Some(&updated_data));
            }

            if inodes.len() as u32 != count {
                return Err(Ext4Error::no_space());
            }

            let ipg = self.superblock.s_inodes_per_group;
            let mut max_ino_in_group = 0u32;
            for global in &inodes {
                let (_g, idx) = self.inode_allocator.global_to_group(*global)?;
                max_ino_in_group = max_ino_in_group.max(idx.raw());
            }

            if let Some(desc_mut) = self.get_group_desc_mut(group_idx) {
                let new_count = desc_mut.free_inodes_count().saturating_sub(count);
                desc_mut.bg_free_inodes_count_lo = (new_count & 0xFFFF) as u16;
                desc_mut.bg_free_inodes_count_hi = (new_count >> 16) as u16;
                desc_mut.bg_flags &= !Ext4GroupDesc::EXT4_BG_INODE_UNINIT;

                let used = ipg.saturating_sub(desc_mut.itable_unused());
                if max_ino_in_group >= used {
                    let new_unused = ipg.saturating_sub(max_ino_in_group + 1);
                    desc_mut.bg_itable_unused_lo = (new_unused & 0xFFFF) as u16;
                    desc_mut.bg_itable_unused_hi = (new_unused >> 16) as u16;
                }
            }
            self.sync_group_descriptor_if_needed(block_dev, group_idx)?;

            self.superblock.s_free_inodes_count =
                self.superblock.s_free_inodes_count.saturating_sub(count);

            // Reset newly allocated inode table entries to a clean reused-inode
            // baseline before higher layers fill in metadata.
            let placeholder = Ext4Inode::empty_for_reuse(self.default_inode_extra_isize());
            for inode_num in &inodes {
                let fresh = placeholder;
                self.modify_inode(block_dev, *inode_num, |inode| {
                    let generation = inode.i_generation;
                    *inode = fresh;
                    inode.i_generation = generation;
                })?;
            }

            debug!(
                "Allocated inodes: group={}, first_global_inode={}, count={} [delayed write]",
                group_idx, inodes[0], count
            );

            return Ok(inodes);
        }

        Err(Ext4Error::no_space())
    }

    pub fn alloc_inode<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<InodeNumber> {
        let ino = self
            .alloc_inodes(block_dev, 1)?
            .into_iter()
            .next()
            .ok_or(Ext4Error::no_space())?;
        let fresh = Ext4Inode::empty_for_reuse(self.default_inode_extra_isize());
        self.modify_inode(block_dev, ino, |inode| {
            let generation = inode.i_generation;
            *inode = fresh;
            inode.i_generation = generation.wrapping_add(1);
        })?;
        Ok(ino)
    }

    /// Frees one data block given its absolute physical block number.
    pub fn free_block<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        global_block: AbsoluteBN,
    ) -> Ext4Result<()> {
        let (group_idx, block_in_group) = self.block_allocator.global_to_group(global_block)?;
        let bitmap_block;
        let cache_key;
        {
            let desc = self
                .get_group_desc_mut(group_idx)
                .ok_or(Ext4Error::corrupted())?;
            bitmap_block = AbsoluteBN::new(desc.block_bitmap());
            cache_key = CacheKey::new_block(group_idx);
        }

        let mut free_ok = Ok(());
        let mut did_free = true;

        if ext4_superblock_has_metadata_csum(&self.superblock) {
            let (uninit, stored) = {
                let gdesc = self
                    .get_group_desc(group_idx)
                    .ok_or(Ext4Error::corrupted())?;
                (gdesc.is_block_bitmap_uninit(), gdesc.block_bitmap_csum())
            };
            if !uninit {
                let bm = self
                    .bitmap_cache
                    .get_or_load(block_dev, cache_key, bitmap_block)?;
                let expected = ext4_block_bitmap_csum32(&self.superblock, &bm.data);
                if expected != stored {
                    return Err(Ext4Error::checksum());
                }
            }
        }
        self.bitmap_cache
            .modify(block_dev, cache_key, bitmap_block, |data| {
                free_ok = match self.block_allocator.free_block(data, block_in_group) {
                    Ok(()) => Ok(()),
                    Err(err) if err.code == Errno::ENOENT => {
                        did_free = false;
                        Ok(())
                    }
                    Err(err) => Err(err),
                };
            })?;

        if ext4_superblock_has_metadata_csum(&self.superblock) {
            let sb = self.superblock;
            let updated_data = self
                .bitmap_cache
                .get(&cache_key)
                .ok_or(Ext4Error::corrupted())?
                .data
                .clone();
            let desc_mut = self
                .get_group_desc_mut(group_idx)
                .ok_or(Ext4Error::corrupted())?;
            desc_mut.update_checksum(&sb, group_idx.raw(), Some(&updated_data), None);
        }
        free_ok?;

        if !did_free {
            return Ok(());
        }

        let desc = self
            .get_group_desc_mut(group_idx)
            .ok_or(Ext4Error::corrupted())?;
        let before = desc.free_blocks_count();
        let new_count = before.saturating_add(1);
        desc.bg_free_blocks_count_lo = (new_count & 0xFFFF) as u16;
        desc.bg_free_blocks_count_hi = (new_count >> 16) as u16;
        self.sync_group_descriptor_if_needed(block_dev, group_idx)?;
        let free_blocks = self.superblock.free_blocks_count().saturating_add(1);
        self.superblock.s_free_blocks_count_lo = (free_blocks & 0xFFFF_FFFF) as u32;
        self.superblock.s_free_blocks_count_hi = (free_blocks >> 32) as u32;
        Ok(())
    }

    /// Frees one inode given its global inode number.
    pub fn free_inode<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<()> {
        let (group_idx, inode_in_group) = self.inode_allocator.global_to_group(inode_num)?;
        let bitmap_block;
        let cache_key;
        {
            let desc = self
                .get_group_desc_mut(group_idx)
                .ok_or(Ext4Error::corrupted())?;
            bitmap_block = AbsoluteBN::new(desc.inode_bitmap());
            cache_key = CacheKey::new_inode(group_idx);
        }

        let mut free_ok = Ok(());
        let mut did_free = true;

        if ext4_superblock_has_metadata_csum(&self.superblock) {
            let (uninit, stored) = {
                let gdesc = self
                    .get_group_desc(group_idx)
                    .ok_or(Ext4Error::corrupted())?;
                (gdesc.is_inode_bitmap_uninit(), gdesc.inode_bitmap_csum())
            };
            if !uninit {
                let bm = self
                    .bitmap_cache
                    .get_or_load(block_dev, cache_key, bitmap_block)?;
                let expected = ext4_inode_bitmap_csum32(&self.superblock, &bm.data);
                if expected != stored {
                    return Err(Ext4Error::checksum());
                }
            }
        }
        self.bitmap_cache
            .modify(block_dev, cache_key, bitmap_block, |data| {
                free_ok = match self.inode_allocator.free_inode(data, inode_in_group) {
                    Ok(()) => Ok(()),
                    Err(err) if err.code == Errno::ENOENT => {
                        did_free = false;
                        Ok(())
                    }
                    Err(err) => Err(err),
                };
            })?;

        if ext4_superblock_has_metadata_csum(&self.superblock) {
            let sb = self.superblock;
            let updated_data = self
                .bitmap_cache
                .get(&cache_key)
                .ok_or(Ext4Error::corrupted())?
                .data
                .clone();
            let desc_mut = self
                .get_group_desc_mut(group_idx)
                .ok_or(Ext4Error::corrupted())?;
            desc_mut.update_checksum(&sb, group_idx.raw(), None, Some(&updated_data));
        }
        free_ok?;

        if !did_free {
            return Ok(());
        }

        let desc = self
            .get_group_desc_mut(group_idx)
            .ok_or(Ext4Error::corrupted())?;
        let before = desc.free_inodes_count();
        let new_count = before.saturating_add(1);
        desc.bg_free_inodes_count_lo = (new_count & 0xFFFF) as u16;
        desc.bg_free_inodes_count_hi = (new_count >> 16) as u16;
        self.sync_group_descriptor_if_needed(block_dev, group_idx)?;
        self.superblock.s_free_inodes_count = self.superblock.s_free_inodes_count.saturating_add(1);
        Ok(())
    }

    pub fn find_group_with_free_blocks(&self) -> Option<BGIndex> {
        for (idx, desc) in self.group_descs.iter().enumerate() {
            if desc.free_blocks_count() > 0 {
                let idx = u32::try_from(idx).ok()?;
                return Some(BGIndex::new(idx));
            }
        }
        None
    }

    pub fn find_group_with_free_inodes(&self) -> Option<BGIndex> {
        for (idx, desc) in self.group_descs.iter().enumerate() {
            if desc.free_inodes_count() > 0 {
                let idx = u32::try_from(idx).ok()?;
                return Some(BGIndex::new(idx));
            }
        }
        None
    }
}
