use super::{mkfs::write_superblock, *};

impl Ext4FileSystem {
    fn clean_state(superblock: &Ext4Superblock) -> u16 {
        (superblock.s_state & Ext4Superblock::EXT4_ERROR_FS) | Ext4Superblock::EXT4_VALID_FS
    }

    /// Flushes all filesystem metadata and caches to the backing device.
    pub fn sync_filesystem<B: BlockIo>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        let mut observer = crate::runtime::NoopObserver;
        self.sync_filesystem_with_observer(block_dev, &mut observer)
    }

    pub fn sync_filesystem_with_observer<B: BlockIo, O: crate::runtime::Observer>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        _observer: &mut O,
    ) -> Ext4Result<()> {
        self.datablock_cache.flush_all(block_dev)?;
        self.inodetable_cache.flush_all(block_dev)?;
        self.bitmap_cache.flush_all(block_dev)?;
        self.sync_group_descriptors(block_dev)?;
        self.sync_superblock_if_dirty(block_dev)?;
        block_dev.commit_for_filesystem_sync()?;
        Ok(())
    }

    /// Unmounts the filesystem after flushing all in-memory metadata.
    pub fn umount<B: BlockIo>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        let mut observer = crate::runtime::NoopObserver;
        self.umount_with_observer(block_dev, &mut observer)
    }

    pub fn umount_with_observer<B: BlockIo, O: crate::runtime::Observer>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        observer: &mut O,
    ) -> Ext4Result<()> {
        use crate::runtime::{Event, JournalEvent, MountEvent};

        if !self.mounted {
            return Ok(());
        }
        if self.superblock.s_last_orphan != 0 {
            return Err(Ext4Error::busy().with_operation("unmount:live_orphans"));
        }

        observer.event(Event::Mount(MountEvent::UnmountStarted));

        let previous_superblock = self.superblock;
        let previous_superblock_dirty = self.superblock_dirty;

        // Mark clean in memory first so that sync_filesystem writes the
        // superblock with s_state = EXT4_VALID_FS through the journal.
        self.superblock.s_state = Self::clean_state(&self.superblock);
        self.superblock.s_feature_incompat &= !Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER;
        self.mark_superblock_dirty();

        let persistence = (|| {
            self.sync_filesystem_with_observer(block_dev, observer)?;

            // Commit the journal transaction so all queued metadata (including
            // the superblock with s_state = VALID_FS) is checkpointed to disk.
            block_dev.umount_commit()
        })();
        if let Err(error) = persistence {
            self.superblock = previous_superblock;
            self.superblock_dirty = previous_superblock_dirty;
            return Err(error);
        }
        observer.event(Event::Journal(JournalEvent::Committed));

        self.mounted = false;
        observer.event(Event::Mount(MountEvent::Unmounted));
        Ok(())
    }

    pub(crate) fn finish_read_only_unmount<O: crate::runtime::Observer>(
        &mut self,
        observer: &mut O,
    ) {
        use crate::runtime::{Event, MountEvent};

        if !self.mounted {
            return;
        }
        observer.event(Event::Mount(MountEvent::UnmountStarted));
        self.mounted = false;
        observer.event(Event::Mount(MountEvent::Unmounted));
    }

    pub fn sync_group_descriptors<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
        if self.dirty_group_descs.len() != self.group_descs.len() {
            return Err(Ext4Error::corrupted().with_operation("sync:group_dirty_count"));
        }
        let desc_size = self.superblock.get_desc_size() as usize;
        let gdt_base = self.superblock.primary_gdt_byte_offset()?;
        let block_size_u64 = self.block_size() as u64;

        let mut search_from = 0;
        while let Some(first_dirty) = self.dirty_group_descs[search_from..]
            .iter()
            .position(|dirty| *dirty)
            .map(|relative| search_from + relative)
        {
            let first_byte = gdt_base
                .checked_add(
                    (first_dirty as u64)
                        .checked_mul(desc_size as u64)
                        .ok_or_else(Ext4Error::overflow)?,
                )
                .ok_or_else(Ext4Error::overflow)?;
            let block_num = AbsoluteBN::new(first_byte / block_size_u64);
            let block_end = block_num
                .raw()
                .checked_add(1)
                .and_then(|block| block.checked_mul(block_size_u64))
                .ok_or_else(Ext4Error::overflow)?;

            let mut end_group = first_dirty + 1;
            while end_group < self.group_descs.len() {
                let byte_offset = gdt_base
                    .checked_add(
                        (end_group as u64)
                            .checked_mul(desc_size as u64)
                            .ok_or_else(Ext4Error::overflow)?,
                    )
                    .ok_or_else(Ext4Error::overflow)?;
                if byte_offset >= block_end {
                    break;
                }
                end_group += 1;
            }

            block_dev.update_block(block_num, true, |buffer| {
                for idx in first_dirty..end_group {
                    if !self.dirty_group_descs[idx] {
                        continue;
                    }
                    let byte_offset = gdt_base
                        .checked_add(
                            (idx as u64)
                                .checked_mul(desc_size as u64)
                                .ok_or_else(Ext4Error::overflow)?,
                        )
                        .ok_or_else(Ext4Error::overflow)?;
                    let in_block = usize::try_from(byte_offset % block_size_u64)
                        .map_err(|_| Ext4Error::overflow())?;
                    let end = in_block
                        .checked_add(desc_size)
                        .ok_or_else(Ext4Error::overflow)?;
                    let destination = buffer
                        .get_mut(in_block..end)
                        .ok_or_else(Ext4Error::corrupted)?;

                    let mut desc = self.group_descs[idx];
                    desc.encode_with_checksum(
                        &self.superblock,
                        idx as u32,
                        destination,
                        None,
                        None,
                    )?;
                    self.group_descs[idx] = desc;
                }
                Ok(())
            })?;

            for dirty in &mut self.dirty_group_descs[first_dirty..end_group] {
                *dirty = false;
            }
            search_from = end_group;
        }

        Ok(())
    }

    pub fn sync_superblock<B: BlockIo>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        // Recompute free-space counters from group descriptors before writing
        // the superblock so the persisted totals match the flushed metadata.
        let mut real_free_blocks: u64 = 0;
        let mut real_free_inodes: u64 = 0;
        for desc in &self.group_descs {
            real_free_blocks += desc.free_blocks_count() as u64;
            real_free_inodes += desc.free_inodes_count() as u64;
        }
        self.superblock.s_free_blocks_count_lo = (real_free_blocks & 0xFFFFFFFF) as u32;
        self.superblock.s_free_blocks_count_hi = (real_free_blocks >> 32) as u32;
        self.superblock.s_free_inodes_count = real_free_inodes as u32;

        self.superblock.update_checksum();
        write_superblock(block_dev, &self.superblock)?;
        self.superblock_dirty = false;
        Ok(())
    }

    fn sync_superblock_if_dirty<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
        if self.superblock_dirty {
            self.sync_superblock(block_dev)?;
        }
        Ok(())
    }

    /// Marks the filesystem clean and writes the superblock.
    ///
    /// Call this during a clean unmount so that Linux sees `s_state =
    /// EXT4_VALID_FS` and skips fsck on the next boot.
    pub fn mark_clean<B: BlockIo>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        self.superblock.s_state = Self::clean_state(&self.superblock);
        self.mark_superblock_dirty();
        self.sync_superblock(block_dev)
    }
}

pub fn umount<B: BlockIo>(fs: Ext4FileSystem, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
    let mut f = fs;
    f.umount(block_dev)?;
    Ok(())
}
