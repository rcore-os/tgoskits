use super::*;
use crate::{
    bmalloc::BGIndex,
    cache::bitmap::CacheKey,
    dir::{FileName, LinkEntryRequest, insert_dir_entry_raw},
};

// Linux reserves EXT4_DATA_TRANS_BLOCKS + EXT4_INDEX_EXTRA_TRANS_BLOCKS + 1
// for ext4_link(). On an extent filesystem without quota this is 24 + 12 + 1.
// Quota is not yet accepted for writable mounts by this core.
const HARD_LINK_TRANSACTION_CREDITS: usize = 37;

/// Creates a hard link below an already resolved parent directory.
pub(crate) fn link_inode_at<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    request: LinkEntryRequest<'_>,
) -> Ext4Result<Ext4Inode> {
    fs.with_metadata_transaction(block_dev, HARD_LINK_TRANSACTION_CREDITS, |fs, block_dev| {
        link_inode_at_in_transaction(fs, block_dev, request)
    })
}

fn link_inode_at_in_transaction<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    request: LinkEntryRequest<'_>,
) -> Ext4Result<Ext4Inode> {
    if request.name.is_reserved() {
        return Err(Ext4Error::invalid_input());
    }
    let target_inode = fs.get_inode_by_num(block_dev, request.target)?;
    if target_inode.i_links_count == 0 {
        return Err(Ext4Error::not_found().with_operation("link:unlinked_inode"));
    }
    if target_inode.is_dir() {
        return Err(Ext4Error::permission_denied());
    }
    let file_type = directory_entry_type_for_mode(target_inode.i_mode)
        .ok_or_else(|| Ext4Error::corrupted().with_operation("link:inode_type"))?;
    let new_links = target_inode.incremented_links_count(false)?;

    let mut parent_inode = fs.get_inode_by_num(block_dev, request.parent)?;
    if !parent_inode.is_dir() {
        return Err(Ext4Error::not_dir());
    }
    match find_named_entry_in_parent(
        fs,
        block_dev,
        request.parent,
        &parent_inode,
        request.name.as_bytes(),
    ) {
        Ok(_) => return Err(Ext4Error::already_exists()),
        Err(error) if error.kind() == Ext4ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let free_blocks_before = fs
        .group_descs
        .iter()
        .map(|descriptor| descriptor.free_blocks_count())
        .collect::<Vec<_>>();

    fs.set_inode_links_count(block_dev, request.target, new_links)?;
    insert_dir_entry_raw(
        fs,
        block_dev,
        request.parent,
        &mut parent_inode,
        request.target,
        request.name,
        file_type,
    )?;

    // Multi-level caches defer inode-table writeback. Publish both inode
    // records before ending the handle so target nlink/ctime, parent times,
    // and the directory entry cannot be split across transactions.
    fs.inodetable_cache.flush(block_dev, request.target)?;
    fs.inodetable_cache.flush(block_dev, request.parent)?;

    let allocated_groups = fs
        .group_descs
        .iter()
        .zip(&free_blocks_before)
        .enumerate()
        .filter_map(|(index, (descriptor, before))| {
            (descriptor.free_blocks_count() < *before).then_some(index)
        })
        .collect::<Vec<_>>();
    for index in &allocated_groups {
        let group = BGIndex::new(u32::try_from(*index).map_err(|_| Ext4Error::overflow())?);
        fs.bitmap_cache
            .flush(block_dev, &CacheKey::new_block(group))?;
        fs.sync_group_descriptor(block_dev, group)?;
    }
    if !allocated_groups.is_empty() {
        fs.sync_superblock(block_dev)?;
    }

    fs.get_inode_by_num(block_dev, request.target)
}

/// Create a hard link through the legacy path API.
pub fn link<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    link_path: &str,
    linked_path: &str,
) -> Ext4Result<()> {
    let link_norm = normalize_path(link_path);
    let linked_norm = normalize_path(linked_path);
    let (target, _) =
        get_file_inode(fs, block_dev, &linked_norm)?.ok_or_else(Ext4Error::not_found)?;

    let (parent_path, child_name) = if let Some(position) = link_norm.rfind('/') {
        let parent = if position == 0 {
            "/".to_string()
        } else {
            link_norm[..position].to_string()
        };
        (parent, link_norm[position + 1..].to_string())
    } else {
        ("/".to_string(), link_norm)
    };
    let name = FileName::new(child_name.as_bytes())?;
    let (parent, _) =
        get_inode_with_num(fs, block_dev, &parent_path)?.ok_or_else(Ext4Error::not_found)?;
    link_inode_at(
        fs,
        block_dev,
        LinkEntryRequest {
            parent,
            name,
            target,
        },
    )?;
    Ok(())
}
