//! Serialized execution of a prepared unwritten write.

use super::*;

impl PreparedUnwrittenWrite {
    pub(super) fn execute_serialized<B: BlockIo>(
        mut self,
        device: &mut Jbd2Dev<B>,
        fs: &mut Ext4FileSystem,
        write: &WriteSlice<'_>,
    ) -> CompletedUnwrittenWrite {
        let result = self.write_data(device, fs, write);
        CompletedUnwrittenWrite {
            prepared: self,
            result,
        }
    }

    fn write_data<B: BlockIo>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        fs: &mut Ext4FileSystem,
        write: &WriteSlice<'_>,
    ) -> Ext4Result<()> {
        let inode_num = self.target.number;
        let inode = &mut self.inode;
        let prepared = &self.prepared;
        let start_lbn = *self.target.logical.start();
        let end_lbn = *self.target.logical.end();
        let block_bytes = fs.block_size() as u64;
        let mut lbn = start_lbn;
        while lbn <= end_lbn {
            let physical = if let Some(run) = prepared.iter().find(|run| {
                run.logical_start <= lbn && lbn < run.logical_start.saturating_add(run.len)
            }) {
                let run_offset = lbn - run.logical_start;
                let run_blocks = run
                    .len
                    .checked_sub(run_offset)
                    .ok_or_else(Ext4Error::overflow)?
                    .min(end_lbn - lbn + 1);
                let physical = run.physical_start.checked_add(run_offset)?;
                let run_start = u64::from(lbn)
                    .checked_mul(block_bytes)
                    .ok_or_else(Ext4Error::file_too_large)?;
                let run_end = run_start
                    .checked_add(u64::from(run_blocks) * block_bytes)
                    .ok_or_else(Ext4Error::file_too_large)?;
                if write.offset <= run_start && write.end >= run_end {
                    write_full_block_run(
                        device,
                        fs,
                        physical,
                        u64::from(lbn),
                        write.offset,
                        write.data,
                        run_blocks,
                    )?;
                } else {
                    for offset in 0..run_blocks {
                        write_inode_block_data(
                            device,
                            fs,
                            physical.checked_add(offset)?,
                            u64::from(lbn + offset),
                            write,
                            true,
                        )?;
                    }
                }
                lbn += run_blocks;
                continue;
            } else {
                match ExtentTree::with_filesystem(inode, fs, inode_num).map_block(device, lbn)? {
                    ExtentBlockMapping::Initialized(physical) => physical,
                    ExtentBlockMapping::Hole | ExtentBlockMapping::Unwritten(_) => {
                        return Err(Ext4Error::corrupted().with_operation("write:prepared_mapping"));
                    }
                }
            };
            write_inode_block_data(device, fs, physical, u64::from(lbn), write, false)?;
            lbn += 1;
        }
        Ok(())
    }
}
