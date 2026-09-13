//! Conversion after successful data I/O, and failure cleanup ownership.

use super::*;

impl CompletedUnwrittenWrite {
    pub(super) fn finish<B: BlockIo>(
        self,
        device: &mut Jbd2Dev<B>,
        fs: &mut Ext4FileSystem,
        write_end: u64,
    ) -> Ext4Result<()> {
        let mut prepared = self.prepared;
        if let Err(error) = self.result {
            let cleanup =
                free_unwritten_finish_reservation(device, &mut prepared.finish_reservation);
            return Err(error_after_cleanup(error, cleanup));
        }
        let inode = prepared.inode;
        prepared.finish_at_inode(device, fs, inode, write_end)
    }
}

impl PreparedUnwrittenWrite {
    pub(in crate::file::write) fn finish_detached<B: BlockIo>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        fs: &mut Ext4FileSystem,
        write_end: u64,
    ) -> Ext4Result<()> {
        // Namespace operations may change links, orphan pointers and times
        // while data I/O owns only content exclusion. Never restore those
        // fields from the inode snapshot captured before data submission.
        let inode = fs.get_inode_by_num(device, self.target.number)?;
        self.finish_at_inode(device, fs, inode, write_end)
    }

    fn finish_at_inode<B: BlockIo>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        fs: &mut Ext4FileSystem,
        mut inode: Ext4Inode,
        write_end: u64,
    ) -> Ext4Result<()> {
        let inode_num = self.target.number;
        let old_size = inode.size();
        let prepared_inode = inode;
        let Some(finish_credits) = self.leaf_snapshots.len().checked_add(1) else {
            let cleanup = free_unwritten_finish_reservation(device, &mut self.finish_reservation);
            return Err(error_after_cleanup(Ext4Error::overflow(), cleanup));
        };
        let prepared = &self.prepared;
        let finish = match self.finish_reservation.take() {
            Some(reserved) => device.with_reserved_transaction(reserved, |device| {
                finish_prepared_unwritten(
                    device, fs, inode_num, &mut inode, prepared, old_size, write_end,
                )
            }),
            None => device.with_transaction_handle(finish_credits, |device| {
                finish_prepared_unwritten(
                    device, fs, inode_num, &mut inode, prepared, old_size, write_end,
                )
            }),
        };
        match finish {
            Ok(()) => Ok(()),
            Err(error) if error.requires_journal_progress() => {
                // Admission failed before the conversion closure ran. The inode
                // and split leaves still describe the valid unwritten mapping;
                // no restore writes are needed (or safe under the same pressure).
                Err(error)
            }
            Err(error) => {
                let restore = restore_prepared_extent_state(
                    device,
                    fs,
                    inode_num,
                    prepared_inode,
                    &self.leaf_snapshots,
                );
                Err(error_after_cleanup(error, restore))
            }
        }
    }
}

pub(super) fn free_unwritten_finish_reservation<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    reservation: &mut Option<ReservedJournalHandle>,
) -> Ext4Result<()> {
    match reservation.take() {
        Some(reserved) => device.free_reserved_transaction(reserved),
        None => Ok(()),
    }
}

fn finish_prepared_unwritten<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    prepared: &[PreparedUnwrittenRun],
    old_size: u64,
    write_end: u64,
) -> Ext4Result<()> {
    for run in prepared {
        ExtentTree::with_filesystem(inode, fs, inode_num).finish_unwritten_write(
            device,
            run.logical_start,
            run.len,
        )?;
    }
    if write_end > old_size {
        inode.i_size_lo = write_end as u32;
        inode.i_size_high = (write_end >> 32) as u32;
    }
    fs.finalize_inode_update(
        device,
        inode_num,
        inode,
        Ext4InodeMetadataUpdate::write_access(),
    )
}

fn restore_prepared_extent_state<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: Ext4Inode,
    snapshots: &[ExtentMetadataSnapshot],
) -> Ext4Result<()> {
    let mut first_error = None;
    for snapshot in snapshots {
        if let Err(error) = device.write_blocks(&snapshot.bytes, snapshot.block, 1, true)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    if let Err(error) = fs.modify_inode(device, inode_num, |on_disk| *on_disk = inode)
        && first_error.is_none()
    {
        first_error = Some(error);
    }
    match first_error {
        Some(error) => Err(error.with_operation("rollback:unwritten_conversion")),
        None => Ok(()),
    }
}
