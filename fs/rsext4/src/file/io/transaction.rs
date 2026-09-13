//! Shared transaction admission and final metadata publication for removal.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum MetadataTransactionStart {
    Join,
    Restart,
}

pub(super) struct MetadataTransactionStep<T> {
    pub(super) start: MetadataTransactionStart,
    pub(super) payload: T,
}

pub(super) fn finalize_restarted_inode_update<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    metadata_update: Ext4InodeMetadataUpdate,
) -> Ext4Result<()> {
    let original_inode = *inode;
    let updated = fs.with_metadata_transaction(device, 1, |fs, device| {
        let mut updated = original_inode;
        fs.finalize_inode_update(device, inode_num, &mut updated, metadata_update)?;
        fs.inodetable_cache.flush(device, inode_num)?;
        Ok(updated)
    })?;
    *inode = updated;
    Ok(())
}
