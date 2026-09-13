use super::*;

impl Ext4FileSystem {
    /// Return whether a path resolves to an inode.
    pub fn path_exists<B: BlockIo>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        path: &str,
    ) -> Ext4Result<bool> {
        let inode = get_file_inode(self, device, path)?;
        Ok(inode.is_some())
    }

    /// Look up an inode by path.
    pub fn find_file<B: BlockIo>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        path: &str,
    ) -> Ext4Result<Ext4Inode> {
        let inode = get_file_inode(self, device, path)?;
        let (_ino, inode) = inode.ok_or(Ext4Error::not_found())?;

        Ok(inode)
    }

    /// Loads the root inode from inode table storage.
    pub fn get_root<B: BlockIo>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<Ext4Inode> {
        let inode_table_start = match self.group_descs.first() {
            Some(desc) => AbsoluteBN::new(desc.inode_table()),
            None => return Err(Ext4Error::corrupted()),
        };
        let (block_num, offset, _group_idx) = self.inodetable_cache.calc_inode_location(
            self.root_inode,
            self.superblock.s_inodes_per_group,
            inode_table_start,
            self.block_size(),
        )?;
        let result =
            self.inodetable_cache
                .get_or_load(block_dev, self.root_inode, block_num, offset)?;

        Ok(result.inode)
    }
}
