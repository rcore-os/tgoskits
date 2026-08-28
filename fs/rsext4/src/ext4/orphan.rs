use ::alloc::collections::BTreeSet;

use super::*;

impl Ext4FileSystem {
    fn checked_orphan_number(&self, raw: u32) -> Ext4Result<Option<InodeNumber>> {
        if raw == 0 {
            return Ok(None);
        }
        if raw < self.superblock.s_first_ino || raw > self.superblock.s_inodes_count {
            return Err(Ext4Error::corrupted().with_operation("orphan:inode_range"));
        }
        InodeNumber::new(raw)
            .map(Some)
            .map_err(|_| Ext4Error::corrupted().with_operation("orphan:inode_number"))
    }

    fn orphan_next<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<Option<InodeNumber>> {
        if !self.inode_is_allocated_checked(block_dev, inode_num)? {
            return Err(Ext4Error::corrupted().with_operation("orphan:inode_not_allocated"));
        }
        let inode = self.get_inode_by_num(block_dev, inode_num)?;
        self.checked_orphan_number(inode.i_dtime)
    }

    pub(crate) fn orphan_contains<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        target: InodeNumber,
    ) -> Ext4Result<bool> {
        let mut current = self.checked_orphan_number(self.superblock.s_last_orphan)?;
        let mut visited = BTreeSet::new();
        let mut found = false;
        while let Some(inode_num) = current {
            if !visited.insert(inode_num) {
                return Err(Ext4Error::corrupted().with_operation("orphan:cycle"));
            }
            if inode_num == target {
                found = true;
            }
            current = self.orphan_next(block_dev, inode_num)?;
        }
        Ok(found)
    }

    pub(crate) fn validate_orphan_chain<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
        let mut current = self.checked_orphan_number(self.superblock.s_last_orphan)?;
        let mut visited = BTreeSet::new();
        while let Some(inode_num) = current {
            if !visited.insert(inode_num) {
                return Err(Ext4Error::corrupted().with_operation("orphan:cycle"));
            }
            let inode = self.get_inode_by_num(block_dev, inode_num)?;
            if inode.i_mode == 0 {
                return Err(Ext4Error::corrupted().with_operation("orphan:empty_inode"));
            }
            current = self.orphan_next(block_dev, inode_num)?;
        }
        Ok(())
    }

    /// Adds an inode to the classic ext4 orphan chain.
    ///
    /// The caller owns the surrounding filesystem transaction. This helper
    /// only performs the checked in-memory metadata transition; JBD2 must
    /// persist the inode-table and superblock updates atomically.
    pub(crate) fn add_orphan<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
    ) -> Ext4Result<()> {
        if self.orphan_contains(block_dev, inode_num)? {
            return Ok(());
        }
        if !self.inode_is_allocated_checked(block_dev, inode_num)? {
            return Err(Ext4Error::corrupted().with_operation("orphan:add_unallocated"));
        }

        let head = self.checked_orphan_number(self.superblock.s_last_orphan)?;
        let inode = self.get_inode_by_num(block_dev, inode_num)?;
        if inode.i_dtime != 0 {
            return Err(Ext4Error::corrupted().with_operation("orphan:add_stale_dtime"));
        }
        self.modify_inode(block_dev, inode_num, |inode| {
            inode.i_dtime = head.map_or(0, InodeNumber::raw);
        })?;
        self.superblock.s_last_orphan = inode_num.raw();
        self.mark_superblock_dirty();
        Ok(())
    }

    /// Removes an inode from the classic ext4 orphan chain.
    pub(crate) fn remove_orphan<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        target: InodeNumber,
    ) -> Ext4Result<Option<InodeNumber>> {
        self.validate_orphan_chain(block_dev)?;
        let mut previous = None;
        let mut current = self.checked_orphan_number(self.superblock.s_last_orphan)?;
        let mut visited = BTreeSet::new();

        while let Some(inode_num) = current {
            if !visited.insert(inode_num) {
                return Err(Ext4Error::corrupted().with_operation("orphan:cycle"));
            }
            let next = self.orphan_next(block_dev, inode_num)?;
            if inode_num == target {
                if let Some(previous_inode) = previous {
                    self.modify_inode(block_dev, previous_inode, |inode| {
                        inode.i_dtime = next.map_or(0, InodeNumber::raw);
                    })?;
                } else {
                    self.superblock.s_last_orphan = next.map_or(0, InodeNumber::raw);
                    self.mark_superblock_dirty();
                }
                self.modify_inode(block_dev, target, |inode| inode.i_dtime = 0)?;
                return Ok(previous);
            }
            previous = Some(inode_num);
            current = next;
        }

        Err(Ext4Error::not_found().with_operation("orphan:remove_missing"))
    }

    pub(crate) fn recover_orphans<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
        self.validate_orphan_chain(block_dev)?;
        let mut visited = BTreeSet::new();
        loop {
            let Some(head) = self.checked_orphan_number(self.superblock.s_last_orphan)? else {
                return Ok(());
            };
            if !visited.insert(head) {
                return Err(Ext4Error::corrupted().with_operation("orphan:recovery_cycle"));
            }
            let inode = self.get_inode_by_num(block_dev, head)?;
            if inode.i_links_count != 0 {
                crate::file::recover_linked_truncate_inode(block_dev, self, head, inode.size())?;
                continue;
            }
            crate::file::reap_unlinked_inode(self, block_dev, head)?;
        }
    }
}
