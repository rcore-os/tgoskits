//! Inode-table block assembly and I/O, independent of cache exclusion.

use super::*;

impl InodeCache {
    pub(super) fn write_inode_bytes_static<B: BlockIo>(
        block_dev: &mut Jbd2Dev<B>,
        block_num: AbsoluteBN,
        offset: usize,
        data: &[u8],
    ) -> Ext4Result<()> {
        let mut buffer = alloc::vec![0u8; block_dev.block_size() as usize];
        block_dev.read_blocks(&mut buffer, block_num, 1)?;
        let end = offset
            .checked_add(data.len())
            .ok_or(Ext4Error::corrupted())?;
        let dst = buffer.get_mut(offset..end).ok_or(Ext4Error::corrupted())?;
        dst.copy_from_slice(data);
        block_dev.write_blocks(&buffer, block_num, 1, true)
    }

    pub(super) fn write_dirty_inode_blocks<B: BlockIo>(
        block_dev: &mut Jbd2Dev<B>,
        dirty: &[(InodeNumber, AbsoluteBN, usize, Arc<Vec<u8>>)],
        pre_read: Option<&[(AbsoluteBN, Vec<u8>)]>,
    ) -> Ext4Result<()> {
        let mut index = 0;
        while index < dirty.len() {
            let block_num = dirty[index].1;
            let mut buffer = alloc::vec![0u8; block_dev.block_size() as usize];
            if let Some(blocks) = pre_read {
                // A concurrent operation may have journaled a neighbor in
                // this same table block while the home read was in flight.
                let index = blocks
                    .binary_search_by_key(&block_num, |(block, _)| *block)
                    .map_err(|_| Ext4Error::corrupted())?;
                let image = block_dev
                    .visible_block_image(block_num)
                    .unwrap_or(&blocks[index].1);
                if image.len() != buffer.len() {
                    return Err(Ext4Error::corrupted().with_operation("inode_table:read_size"));
                }
                buffer.copy_from_slice(image);
            } else {
                block_dev.read_blocks(&mut buffer, block_num, 1)?;
            }

            while index < dirty.len() && dirty[index].1 == block_num {
                let (_, _, offset, data) = &dirty[index];
                let end = offset
                    .checked_add(data.len())
                    .ok_or(Ext4Error::corrupted())?;
                let dst = buffer.get_mut(*offset..end).ok_or(Ext4Error::corrupted())?;
                dst.copy_from_slice(data);
                index += 1;
            }
            block_dev.write_blocks(&buffer, block_num, 1, true)?;
        }
        Ok(())
    }
}
