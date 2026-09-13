//! File-write orchestration and shared input ranges.

use super::*;

mod data;
mod detached;
mod legacy;
mod path;
mod unwritten;

use data::{existing_full_block_run, write_full_block_run, write_inode_block_data};
pub(crate) use detached::{CompletedFileWrite, PreparedFileWrite};
use legacy::write_legacy_inode_data;
pub use path::write_file;
use unwritten::{extent_write_needs_preparation, write_inode_data_through_unwritten};

struct WriteSlice<'a> {
    offset: u64,
    end: u64,
    data: &'a [u8],
}

/// Inode and logical range validated before allocation or data submission.
struct WriteTarget {
    number: InodeNumber,
    logical: core::ops::RangeInclusive<u32>,
}

pub fn write_inode_data<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    offset: u64,
    data: &[u8],
) -> Ext4Result<()> {
    if data.is_empty() {
        return Ok(());
    }

    let mut inode = fs.get_inode_by_num(device, inode_num)?;

    let old_size = inode.size();
    let block_bytes = fs.block_size() as u64;

    let data_len = u64::try_from(data.len()).map_err(|_| Ext4Error::overflow())?;
    let end = offset
        .checked_add(data_len)
        .ok_or_else(Ext4Error::file_too_large)?;

    let start_lbn = offset / block_bytes;
    let end_lbn = (end - 1) / block_bytes;
    if end_lbn > u32::MAX as u64 {
        return Err(Ext4Error::file_too_large());
    }

    let write = WriteSlice { offset, end, data };
    if !inode.uses_extents() {
        return write_legacy_inode_data(device, fs, inode_num, inode, write);
    }

    if extent_write_needs_preparation(
        device,
        fs,
        inode_num,
        &mut inode,
        start_lbn as u32,
        end_lbn as u32,
    )? {
        return write_inode_data_through_unwritten(
            device,
            fs,
            WriteTarget {
                number: inode_num,
                logical: start_lbn as u32..=end_lbn as u32,
            },
            &write,
        );
    }

    let use_existing_run_map = end <= old_size
        && offset.is_multiple_of(block_bytes)
        && end.is_multiple_of(block_bytes)
        && start_lbn < end_lbn;
    let existing_runs = if use_existing_run_map {
        let mut tree = ExtentTree::with_filesystem(&mut inode, fs, inode_num);
        Some(tree.initialized_runs_in_range(device, start_lbn as u32, end_lbn as u32)?)
    } else {
        None
    };

    let mut lbn = start_lbn;
    while lbn <= end_lbn {
        if let Some(runs) = existing_runs.as_ref()
            && let Some((start_phys, run_len)) =
                existing_full_block_run(runs, lbn, offset, end, block_bytes)
            && run_len > 1
        {
            write_full_block_run(device, fs, start_phys, lbn, offset, data, run_len)?;
            lbn += u64::from(run_len);
            continue;
        }

        let mapping =
            ExtentTree::with_filesystem(&mut inode, fs, inode_num).map_block(device, lbn as u32)?;
        let phys = match mapping {
            ExtentBlockMapping::Initialized(block) => block,
            ExtentBlockMapping::Hole | ExtentBlockMapping::Unwritten(_) => {
                return Err(Ext4Error::corrupted().with_operation("write:unprepared_mapping"));
            }
        };

        write_inode_block_data(device, fs, phys, lbn, &write, false)?;
        lbn += 1;
    }

    if end > old_size {
        inode.i_size_lo = (end & 0xffff_ffff) as u32;
        inode.i_size_high = (end >> 32) as u32;
    }

    fs.finalize_inode_update(
        device,
        inode_num,
        &mut inode,
        Ext4InodeMetadataUpdate::write_access(),
    )?;

    Ok(())
}
