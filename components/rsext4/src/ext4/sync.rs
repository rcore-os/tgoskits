use super::{mkfs::write_superblock, *};

impl Ext4FileSystem {
    /// Flushes all filesystem metadata and caches to the backing device.
    pub fn sync_filesystem<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
        info!("Syncing filesystem...");
        self.datablock_cache.flush_all(block_dev)?;
        self.inodetable_cahce.flush_all(block_dev)?;
        self.bitmap_cache.flush_all(block_dev)?;
        self.sync_group_descriptors(block_dev)?;
        self.sync_superblock(block_dev)?;
        block_dev.cantflush()?;
        Ok(())
    }

    /// Unmounts the filesystem after flushing all in-memory metadata.
    pub fn umount<B: BlockDevice>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        if !self.mounted {
            return Ok(());
        }

        debug!("Unmounting Ext4 filesystem...");
        self.sync_filesystem(block_dev)?;

        block_dev.umount_commit();

        self.mounted = false;
        info!("Filesystem unmounted cleanly");
        Ok(())
    }

    pub fn sync_group_descriptors<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
        let total_desc_count = self.group_descs.len();
        let desc_size = self.superblock.get_desc_size() as usize;
        let gdt_base: u64 = BLOCK_SIZE as u64;
        let block_size_u64 = BLOCK_SIZE as u64;

        debug!(
            "Writing back group descriptors: {total_desc_count} descriptors, desc_size = \
             {desc_size} bytes"
        );

        let mut current_block: Option<AbsoluteBN> = None;
        let mut buffer_snapshot_block: Option<AbsoluteBN> = None;

        for (idx, desc) in self.group_descs.iter_mut().enumerate() {
            // Stream descriptors back in block order so a GDT block is read and
            // written at most once per contiguous chunk.
            let group_id = idx as u32;
            desc.update_checksum(&self.superblock, group_id, None, None);
            let byte_offset = gdt_base + idx as u64 * desc_size as u64;
            let block_num = AbsoluteBN::new(byte_offset / block_size_u64);
            let in_block = (byte_offset % block_size_u64) as usize;
            let end = in_block + desc_size;

            if current_block != Some(block_num) {
                if let Some(prev_block) = current_block
                    && Some(prev_block) == buffer_snapshot_block
                {
                    block_dev.write_block(prev_block, false)?;
                }

                block_dev.read_block(block_num)?;
                current_block = Some(block_num);
                buffer_snapshot_block = Some(block_num);
            }

            let buffer = block_dev.buffer_mut();
            if end > buffer.len() {
                error!(
                    "GDT out of range: idx={}, in_block={}, desc_size={}, buffer_len={}",
                    idx,
                    in_block,
                    desc_size,
                    buffer.len()
                );
                return Err(Ext4Error::corrupted());
            }

            desc.to_disk_bytes(&mut buffer[in_block..end]);
        }

        if let Some(last_block) = current_block
            && Some(last_block) == buffer_snapshot_block
        {
            block_dev.write_block(last_block, true)?;
        }

        debug!("Group descriptors written back");
        Ok(())
    }

    pub fn sync_superblock<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
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
        write_superblock(block_dev, &self.superblock)
    }
}

pub fn umount<B: BlockDevice>(fs: Ext4FileSystem, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
    let mut f = fs;
    f.umount(block_dev)?;
    Ok(())
}
