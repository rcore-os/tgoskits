//! Serialized legacy indirect writes and allocation rollback.

use super::*;

pub(super) fn write_legacy_inode_data<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    mut inode: Ext4Inode,
    write: WriteSlice<'_>,
) -> Ext4Result<()> {
    let original_inode = inode;
    let old_size = inode.size();
    let block_bytes = fs.block_size() as u64;
    let start_lbn = write.offset / block_bytes;
    let end_lbn = (write.end - 1) / block_bytes;
    let mut allocations = Vec::new();
    let mut inode_update_attempted = false;
    let operation = (|| {
        for lbn in start_lbn..=end_lbn {
            let logical = u32::try_from(lbn).map_err(|_| Ext4Error::file_too_large())?;
            let allocation = crate::indirect::allocate_legacy_inode_block(
                fs, device, inode_num, &mut inode, logical,
            )?;
            let physical = allocation.physical();
            let newly_allocated = allocation.is_new();
            if newly_allocated {
                allocations.push(allocation);
            }
            write_inode_block_data(device, fs, physical, lbn, &write, newly_allocated)?;
        }

        if write.end > old_size {
            inode.i_size_lo = write.end as u32;
            inode.i_size_high = (write.end >> 32) as u32;
        }
        inode_update_attempted = true;
        fs.finalize_inode_update(
            device,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::write_access(),
        )
    })();

    match operation {
        Ok(()) => Ok(()),
        Err(operation_error) => {
            if inode_update_attempted
                && let Err(restore_error) = fs.modify_inode(device, inode_num, |on_disk| {
                    *on_disk = original_inode;
                })
            {
                // The inode cache or pending journal update may still expose
                // the new branch. Retain its blocks unless the old inode image
                // is known to be restored.
                return Err(restore_error.with_operation("rollback:legacy_inode_restore"));
            }
            let cleanup =
                rollback_legacy_allocations(device, fs, inode_num, &mut inode, allocations);
            match cleanup {
                Ok(()) => Err(operation_error),
                Err(cleanup_error) => Err(cleanup_error),
            }
        }
    }
}

fn rollback_legacy_allocations<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    allocations: Vec<crate::indirect::LegacyBlockAllocation>,
) -> Ext4Result<()> {
    let mut first_error = None;
    for allocation in allocations.into_iter().rev() {
        if let Err(error) = allocation.rollback(fs, device, inode_num, inode)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error.with_operation("rollback:legacy_write")),
        None => Ok(()),
    }
}
