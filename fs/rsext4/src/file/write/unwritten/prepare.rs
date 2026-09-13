//! Allocation and split metadata required before unwritten data submission.

use super::*;

pub(super) fn prepare_unwritten_write<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    target: WriteTarget,
) -> Ext4Result<PreparedUnwrittenWrite> {
    let inode_num = target.number;
    let start_lbn = *target.logical.start();
    let end_lbn = *target.logical.end();
    let block_size = fs.block_size();
    let block_bytes = block_size as u64;
    let aligned_offset = u64::from(start_lbn)
        .checked_mul(block_bytes)
        .ok_or_else(Ext4Error::file_too_large)?;
    let block_count = u64::from(end_lbn)
        .checked_sub(u64::from(start_lbn))
        .and_then(|blocks| blocks.checked_add(1))
        .ok_or_else(Ext4Error::file_too_large)?;
    let aligned_len = block_count
        .checked_mul(block_bytes)
        .ok_or_else(Ext4Error::file_too_large)?;

    // Fill every hole with an unwritten mapping first. A later data error can
    // therefore leave a reachable reservation, but can never expose stale
    // disk contents as initialized file data.
    preallocate_inode(
        device,
        fs,
        inode_num,
        aligned_offset,
        aligned_len,
        PreallocationOptions::KEEP_SIZE,
    )?;
    let mut inode = fs.get_inode_by_num(device, inode_num)?;
    let mut planned = Vec::new();
    let end_exclusive = end_lbn
        .checked_add(1)
        .ok_or_else(Ext4Error::file_too_large)?;
    let mut logical = start_lbn;
    while logical < end_exclusive {
        let extent = ExtentTree::with_filesystem(&mut inode, fs, inode_num)
            .find_extent_at_or_after(device, logical)?
            .ok_or_else(|| Ext4Error::corrupted().with_operation("write:preallocation_hole"))?;
        if extent.ee_block > logical {
            return Err(Ext4Error::corrupted().with_operation("write:preallocation_hole"));
        }
        let extent_end = extent
            .ee_block
            .checked_add(extent.len())
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:logical_overflow"))?;
        let run_end = core::cmp::min(extent_end, end_exclusive);
        if extent.is_unwritten() {
            let len = run_end - logical;
            let physical_start =
                AbsoluteBN::new(extent.start_block()).checked_add(logical - extent.ee_block)?;
            planned.push(PreparedUnwrittenRun {
                logical_start: logical,
                physical_start,
                len,
            });
        }
        logical = run_end;
    }

    let reserved_finish_credits = planned
        .len()
        .checked_add(1)
        .ok_or_else(Ext4Error::overflow)?;
    let transaction_credit_limit = device.transaction_credit_limit()?;
    let mut finish_reservation = None;
    let mut prepared = Vec::with_capacity(planned.len());
    for (index, run) in planned.iter().enumerate() {
        let tree_depth = ExtentTree::with_filesystem(&mut inode, fs, inode_num)
            .load_root_from_inode()?
            .header()
            .eh_depth;
        let prepare_credits = usize::from(tree_depth)
            .checked_mul(2)
            .and_then(|credits| credits.checked_add(8))
            .ok_or_else(Ext4Error::overflow)?;
        let is_last = index + 1 == planned.len();
        let reserve_finish = is_last
            && transaction_credit_limit.is_some_and(|limit| {
                reserved_finish_credits <= limit / 2
                    && prepare_credits
                        .checked_add(reserved_finish_credits)
                        .is_some_and(|total| total <= limit)
            });
        let prepare = |device: &mut Jbd2Dev<B>| {
            {
                let mut tree = ExtentTree::with_filesystem(&mut inode, fs, inode_num);
                tree.prepare_unwritten_write(fs, device, run.logical_start, run.len)?;
            }
            // Publish the still-unwritten split in the same journal operation
            // as its external extent-node updates.
            fs.modify_inode(device, inode_num, |on_disk| *on_disk = inode)
        };
        if reserve_finish {
            let ((), reserved) = device.with_transaction_reservation(
                TransactionCredits::metadata(prepare_credits),
                TransactionCredits::metadata(reserved_finish_credits),
                prepare,
            )?;
            finish_reservation = Some(reserved);
        } else {
            device.with_transaction_handle(prepare_credits, prepare)?;
        }
        prepared.push(*run);
    }

    let leaf_snapshots =
        match snapshot_prepared_extent_leaves(device, fs, inode_num, &mut inode, &prepared) {
            Ok(snapshots) => snapshots,
            Err(error) => {
                let cleanup = free_unwritten_finish_reservation(device, &mut finish_reservation);
                return Err(error_after_cleanup(error, cleanup));
            }
        };

    Ok(PreparedUnwrittenWrite {
        target,
        inode,
        prepared,
        finish_reservation,
        leaf_snapshots,
    })
}

fn snapshot_prepared_extent_leaves<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    prepared: &[PreparedUnwrittenRun],
) -> Ext4Result<Vec<ExtentMetadataSnapshot>> {
    let mut snapshots = Vec::new();
    for run in prepared {
        let block = ExtentTree::with_filesystem(inode, fs, inode_num)
            .external_leaf_block(device, run.logical_start)?;
        let Some(block) = block else {
            continue;
        };
        if snapshots
            .iter()
            .any(|snapshot: &ExtentMetadataSnapshot| snapshot.block == block)
        {
            continue;
        }
        device.read_block(block)?;
        snapshots.push(ExtentMetadataSnapshot {
            block,
            bytes: device.buffer().to_vec(),
        });
    }
    Ok(snapshots)
}

pub(in crate::file::write) fn extent_write_needs_preparation<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    start_lbn: u32,
    end_lbn: u32,
) -> Ext4Result<bool> {
    let mut logical = start_lbn;
    loop {
        let next = ExtentTree::with_filesystem(inode, fs, inode_num)
            .find_extent_at_or_after(device, logical)?;
        let Some(extent) = next else {
            return Ok(true);
        };
        if extent.ee_block > logical || extent.is_unwritten() {
            return Ok(true);
        }
        let extent_end = extent
            .ee_block
            .checked_add(extent.len())
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:logical_overflow"))?;
        if extent_end > end_lbn {
            return Ok(false);
        }
        logical = extent_end;
    }
}
