use super::*;

impl Ext4FileSystem {
    /// Return whether a path resolves to an inode.
    pub fn file_entries_exist<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        path: &str,
    ) -> Ext4Result<bool> {
        let inode = get_file_inode(self, device, path)?;
        Ok(inode.is_some())
    }

    /// Look up an inode by path.
    pub fn find_file<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        path: &str,
    ) -> Ext4Result<Ext4Inode> {
        let inode = get_file_inode(self, device, path)?;
        let (_ino, inode) = inode.ok_or(Ext4Error::not_found())?;
        debug!("Found inode for path {path}");
        Ok(inode)
    }

    /// Loads the root inode from inode table storage.
    pub fn get_root<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<Ext4Inode> {
        let inode_table_start = match self.group_descs.first() {
            Some(desc) => AbsoluteBN::new(desc.inode_table()),
            None => return Err(Ext4Error::corrupted()),
        };
        let (block_num, offset, _group_idx) = self.inodetable_cahce.calc_inode_location(
            self.root_inode,
            self.superblock.s_inodes_per_group,
            inode_table_start,
            BLOCK_SIZE,
        )?;
        let result =
            self.inodetable_cahce
                .get_or_load(block_dev, self.root_inode, block_num, offset)?;
        debug!("Root inode i_mode: {}", result.inode.i_mode);
        debug!("Root inode detail: {:?}", result.inode);
        Ok(result.inode)
    }
}

/// Return whether a path resolves to an inode.
pub fn file_entry_exisr<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    path: &str,
) -> Ext4Result<bool> {
    fs.file_entries_exist(device, path)
}

/// Look up an inode by path.
pub fn find_file<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    path: &str,
) -> Ext4Result<Ext4Inode> {
    fs.find_file(device, path)
}
