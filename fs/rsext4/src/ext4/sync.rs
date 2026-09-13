use super::{mkfs::write_superblock, *};

impl Ext4FileSystem {
    pub(crate) fn clean_state(superblock: &Ext4Superblock) -> u16 {
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
        self.stage_sync_metadata(block_dev)?;
        block_dev.commit_for_filesystem_sync()?;
        Ok(())
    }

    /// Publishes cached metadata before sealing a durability target.
    /// Journal commit/checkpoint I/O belongs to the separate commit owner.
    pub(crate) fn stage_sync_metadata<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
        self.datablock_cache.flush_all(block_dev)?;
        self.inodetable_cache.flush_all(block_dev)?;
        self.bitmap_cache.flush_all(block_dev)?;
        self.sync_group_descriptors(block_dev)?;
        self.sync_superblock_if_dirty(block_dev)?;
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

        // Keep RECOVER set while any home write or journal-tail update can
        // still fail. A clean superblock must never be an early checkpoint
        // member: a crash there could suppress replay of the remaining homes.
        self.sync_filesystem_with_observer(block_dev, observer)?;
        block_dev.umount_commit()?;
        observer.event(Event::Journal(JournalEvent::Committed));

        // From the first clean-publication attempt onwards this mount is
        // terminal, even if the device reports an uncertain write failure.
        self.mounted = false;
        let clean = clean_superblock(self.superblock);
        if let Err(error) = write_clean_superblock(block_dev, &clean) {
            self.mmp.mark_failed(error);
            return Err(error);
        }
        self.superblock = clean;
        self.superblock_dirty = false;
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

pub(crate) fn clean_superblock(mut superblock: Ext4Superblock) -> Ext4Superblock {
    superblock.s_state = Ext4FileSystem::clean_state(&superblock);
    superblock.s_feature_incompat &= !Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER;
    superblock.update_checksum();
    superblock
}

/// Publishes only after every journal transaction has checkpointed. Preserve
/// the rest of the filesystem block around the 1024-byte primary superblock.
pub(crate) fn write_clean_superblock<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    superblock: &Ext4Superblock,
) -> Ext4Result<()> {
    device.ensure_clean_publication_ready()?;
    let block_size = device.block_size() as usize;
    let offset = Ext4Superblock::SUPERBLOCK_OFFSET as usize;
    let block = AbsoluteBN::new((offset / block_size) as u64);
    let in_block = offset % block_size;
    let end = in_block
        .checked_add(Ext4Superblock::SUPERBLOCK_SIZE)
        .ok_or_else(Ext4Error::overflow)?;
    if end > block_size {
        return Err(Ext4Error::bad_superblock().with_operation("unmount:superblock_geometry"));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(block_size)
        .map_err(|_| Ext4Error::no_memory())?;
    bytes.resize(block_size, 0);
    device.read_blocks_uncached(&mut bytes, block, 1)?;
    superblock.to_disk_bytes(&mut bytes[in_block..end]);
    device.write_blocks_durable(&bytes, block, 1)
}

pub fn umount<B: BlockIo>(fs: Ext4FileSystem, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
    let mut f = fs;
    f.umount(block_dev)?;
    Ok(())
}
