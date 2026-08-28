use super::{
    xattr::{external_store_revoke_records, read_external_store, release_external_store},
    *,
};
use crate::{
    entries::{decode_directory_record_length, encode_directory_record_length},
    hashtree::Ext4InodeHashTreeExt,
};

// Linux uses EXT4_DATA_TRANS_BLOCKS for both unlink and rmdir. Extent
// filesystems without writable quota support reserve 20 + 6 - 2 blocks.
const UNLINK_TRANSACTION_CREDITS: usize = 24;

// ext4_evict_inode starts with ext4_blocks_for_truncate() plus six final
// cleanup credits, subtracting the three bitmap/group/inode credits already
// counted by truncate. Quota is not implemented yet, so the base is 24 + 3.
const REAP_BASE_TRANSACTION_CREDITS: usize = 27;
const REAP_MAX_TRANSACTION_DATA: u64 = 64;
// Once a restarted truncate has removed every mapping, final reap can touch
// at most the target and predecessor inode-table blocks, the inode bitmap,
// one group-descriptor block, and the superblock.
const EMPTY_REAP_TRANSACTION_CREDITS: usize = 5;

/// A directory entry located by a single parent-directory scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParentDirEntry {
    pub ino: InodeNumber,
    pub phys: AbsoluteBN,
    pub offset: usize,
    pub file_type: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DentryReplacement {
    pub inode: InodeNumber,
    pub file_type: u8,
}

/// Result of removing one non-directory name from a parent directory.
///
/// A zero `remaining_links` value means the inode is still allocated on the
/// ext4 orphan chain and must be reaped only after the VFS has released its
/// final live inode reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct UnlinkOutcome {
    pub inode: InodeNumber,
    pub remaining_links: u16,
}

impl UnlinkOutcome {
    pub const fn requires_reap(self) -> bool {
        self.remaining_links == 0
    }
}

fn ensure_inode_free_is_supported(fs: &Ext4FileSystem, inode: &Ext4Inode) -> Ext4Result<()> {
    if crate::indirect::has_legacy_indirect_mapping(fs, inode) {
        return Err(Ext4Error::unsupported().with_operation("indirect:free"));
    }
    Ok(())
}

pub(crate) fn preflight_inode_free<B: BlockIo>(
    fs: &Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    inode_num: InodeNumber,
    inode: &Ext4Inode,
) -> Ext4Result<()> {
    if !inode.uses_extents() {
        crate::indirect::collect_legacy_inode_ownership(fs, block_dev, inode_num, inode)?;
    }
    Ok(())
}

fn truncate_legacy_indirect_mapping_before_free<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
) -> Ext4Result<()> {
    if crate::indirect::has_legacy_indirect_mapping(fs, inode) {
        crate::file::truncate_inode_for_reap(block_dev, fs, inode_num)?;
        *inode = fs.get_inode_by_num(block_dev, inode_num)?;
    }
    Ok(())
}

struct InodeOwnedBlocks {
    data: Vec<AbsoluteBN>,
    metadata: Vec<AbsoluteBN>,
}

impl InodeOwnedBlocks {
    fn revoke_records(&self) -> usize {
        self.metadata.len()
    }
}

fn inode_owned_blocks<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
) -> Ext4Result<InodeOwnedBlocks> {
    ensure_inode_free_is_supported(fs, inode)?;
    let mut mapped_blocks: Vec<AbsoluteBN> = if inode.uses_extents() {
        resolve_inode_blocks(fs, block_dev, inode_num, inode)?
            .into_values()
            .collect()
    } else {
        crate::indirect::collect_legacy_inode_ownership(fs, block_dev, inode_num, inode)?
            .into_data_blocks()
    };
    let mut metadata = if inode.uses_extents() {
        ExtentTree::with_filesystem(inode, fs, inode_num).external_node_blocks(block_dev)?
    } else {
        Vec::new()
    };

    // Linux applies EXT4_FREE_BLOCKS_METADATA | EXT4_FREE_BLOCKS_FORGET to
    // directory and non-inline symlink mappings. Their payload blocks carry
    // filesystem structure and must be revoked just like external extent
    // nodes before allocator reuse.
    let mut data = if inode.is_dir() || inode.is_symlink() {
        metadata.append(&mut mapped_blocks);
        Vec::new()
    } else {
        mapped_blocks
    };
    data.sort_unstable();
    data.dedup();
    metadata.sort_unstable();
    metadata.dedup();
    if metadata
        .iter()
        .any(|block| data.binary_search(block).is_ok())
    {
        return Err(Ext4Error::corrupted().with_operation("inode:owned_block_kind_overlap"));
    }
    Ok(InodeOwnedBlocks { data, metadata })
}

fn release_inode_owned_blocks<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    owned: InodeOwnedBlocks,
) -> Ext4Result<()> {
    for block in owned.data {
        fs.datablock_cache.invalidate(block);
        fs.free_block(block_dev, block)?;
    }
    for block in owned.metadata {
        block_dev.forget_detached_metadata(block)?;
        fs.datablock_cache.invalidate(block);
        fs.free_block(block_dev, block)?;
    }
    Ok(())
}

fn reap_transaction_credits(
    fs: &Ext4FileSystem,
    inode: &Ext4Inode,
    has_external_xattr: bool,
    revoke_records: usize,
) -> Ext4Result<TransactionCredits> {
    let block_size = u32::try_from(fs.superblock.block_size())
        .map_err(|_| Ext4Error::corrupted().with_operation("orphan:reap_block_size"))?;
    let sectors_per_block = u64::from(block_size / 512);
    if sectors_per_block == 0 {
        return Err(Ext4Error::corrupted().with_operation("orphan:reap_block_size"));
    }
    let huge_file = fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
    let owned_blocks = inode.blocks_count(block_size, huge_file);
    if has_external_xattr && owned_blocks < sectors_per_block {
        return Err(Ext4Error::corrupted().with_operation("xattr:i_blocks"));
    }
    if owned_blocks == 0 {
        return Ok(TransactionCredits::metadata_with_revokes(
            EMPTY_REAP_TRANSACTION_CREDITS,
            revoke_records,
        ));
    }
    let data_credits = owned_blocks
        .div_ceil(sectors_per_block)
        .clamp(2, REAP_MAX_TRANSACTION_DATA);
    let metadata_credits = REAP_BASE_TRANSACTION_CREDITS
        .checked_add(usize::try_from(data_credits).map_err(|_| Ext4Error::overflow())?)
        .ok_or_else(Ext4Error::overflow)?;
    Ok(TransactionCredits::metadata_with_revokes(
        metadata_credits,
        revoke_records,
    ))
}

fn flush_reap_metadata<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    counters_before: &[GroupCounters],
    orphan_predecessor: Option<InodeNumber>,
) -> Ext4Result<()> {
    fs.flush_changed_group_metadata(block_dev, counters_before)?;
    if let Some(predecessor) = orphan_predecessor {
        fs.inodetable_cache.flush(block_dev, predecessor)?;
    }
    fs.sync_superblock(block_dev)
}

fn free_inode<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
) -> Ext4Result<()> {
    truncate_legacy_indirect_mapping_before_free(fs, block_dev, inode_num, inode)?;
    let owned_blocks = inode_owned_blocks(fs, block_dev, inode_num, inode)?;

    let updated_inode = fs.apply_inode_dtime(block_dev, inode_num, Ext4DtimeUpdate::SetNow)?;

    release_inode_owned_blocks(fs, block_dev, owned_blocks)?;

    *inode = updated_inode;
    inode.i_links_count = 0;
    inode.i_block = [0; 15];
    inode.i_blocks_lo = 0;
    inode.l_i_blocks_high = 0;
    inode.i_size_lo = 0;
    inode.i_size_high = 0;
    fs.finalize_inode_update(
        block_dev,
        inode_num,
        inode,
        Ext4InodeMetadataUpdate::link_count_change(),
    )?;

    fs.free_inode(block_dev, inode_num)
}

/// Reclaims a zero-link inode after the VFS has dropped its final reference.
pub fn reap_unlinked_inode<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    inode_num: InodeNumber,
) -> Ext4Result<()> {
    if !fs.inode_is_allocated_checked(block_dev, inode_num)? {
        return Err(Ext4Error::not_found().with_operation("orphan:reap_unallocated"));
    }
    let mut inode = fs.get_inode_by_num(block_dev, inode_num)?;
    let was_directory = inode.is_dir();
    if inode.i_links_count != 0 {
        return Err(Ext4Error::invalid_input().with_operation("orphan:reap_linked"));
    }
    if !fs.orphan_contains(block_dev, inode_num)? {
        return Err(Ext4Error::not_found().with_operation("orphan:reap_not_listed"));
    }
    preflight_inode_free(fs, block_dev, inode_num, &inode)?;
    // Legacy indirect trees may require their own bounded transaction. The
    // zero-link inode remains durably orphaned across this boundary, so a
    // crash after mapping removal simply resumes the final reap on mount.
    truncate_legacy_indirect_mapping_before_free(fs, block_dev, inode_num, &mut inode)?;
    let external_xattr = read_external_store(block_dev, fs, &inode)?;
    let owned_blocks = inode_owned_blocks(fs, block_dev, inode_num, &mut inode)?;
    let credits = reap_transaction_credits(
        fs,
        &inode,
        external_xattr.is_some(),
        external_store_revoke_records(external_xattr.as_ref())
            .checked_add(owned_blocks.revoke_records())
            .ok_or_else(Ext4Error::overflow)?,
    )?;
    let counters_before = fs.group_counter_snapshot();

    fs.with_metadata_transaction(block_dev, credits, |fs, block_dev| {
        if let Some(external_xattr) = external_xattr {
            release_external_store(block_dev, fs, external_xattr)?;
            inode.set_file_acl(0)?;
        }
        release_inode_owned_blocks(fs, block_dev, owned_blocks)?;

        // Keep i_dtime intact while the inode is on the orphan chain: it is
        // the next-inode pointer, not a wall-clock deletion time.
        inode.i_block = [0; 15];
        inode.i_blocks_lo = 0;
        inode.l_i_blocks_high = 0;
        inode.i_size_lo = 0;
        inode.i_size_high = 0;
        fs.finalize_inode_update(
            block_dev,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::link_count_change(),
        )?;

        let orphan_predecessor = fs.remove_orphan(block_dev, inode_num)?;
        fs.apply_inode_dtime(block_dev, inode_num, Ext4DtimeUpdate::SetNow)?;
        fs.free_inode(block_dev, inode_num)?;
        if was_directory {
            let (group, _) = fs.inode_allocator.global_to_group(inode_num)?;
            let descriptor = fs
                .get_group_desc_mut(group)
                .ok_or_else(Ext4Error::corrupted)?;
            let used = descriptor.used_dirs_count().saturating_sub(1);
            descriptor.bg_used_dirs_count_lo = (used & 0xffff) as u16;
            descriptor.bg_used_dirs_count_hi = (used >> 16) as u16;
        }
        flush_reap_metadata(fs, block_dev, &counters_before, orphan_predecessor)
    })
}

/// Removes one raw name below an already resolved parent directory.
pub(crate) fn unlink_inode_at<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    parent: InodeNumber,
    name: FileName<'_>,
) -> Ext4Result<UnlinkOutcome> {
    if name.is_reserved() {
        return Err(Ext4Error::invalid_input().with_operation("unlink:reserved_name"));
    }
    let parent_inode = fs.get_inode_by_num(block_dev, parent)?;
    let entry = find_named_entry_in_parent(fs, block_dev, parent, &parent_inode, name.as_bytes())?;
    let target_inode = fs.get_inode_by_num(block_dev, entry.ino)?;
    if target_inode.is_dir() {
        return Err(Ext4Error::is_dir());
    }

    let new_links = target_inode.decremented_links_count()?;
    if new_links == 0 {
        preflight_inode_free(fs, block_dev, entry.ino, &target_inode)?;
    }
    fs.with_metadata_transaction(block_dev, UNLINK_TRANSACTION_CREDITS, |fs, block_dev| {
        remove_named_entry_at(fs, block_dev, parent, &parent_inode, entry, name.as_bytes())?;
        fs.touch_parent_dir_for_entry_change(block_dev, parent)?;
        fs.set_inode_links_count(block_dev, entry.ino, new_links)?;
        if new_links == 0 {
            fs.add_orphan(block_dev, entry.ino)?;
        }
        fs.inodetable_cache.flush(block_dev, parent)?;
        fs.inodetable_cache.flush(block_dev, entry.ino)?;
        if new_links == 0 {
            fs.sync_superblock(block_dev)?;
        }
        Ok(())
    })?;
    Ok(UnlinkOutcome {
        inode: entry.ino,
        remaining_links: new_links,
    })
}

/// Removes an empty directory name while retaining its zero-link inode on the
/// orphan chain until the embedding VFS releases its final live reference.
pub(crate) fn unlink_empty_directory_at<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    parent: InodeNumber,
    name: FileName<'_>,
) -> Ext4Result<UnlinkOutcome> {
    if name.is_reserved() {
        return Err(Ext4Error::invalid_input().with_operation("rmdir:reserved_name"));
    }
    let parent_inode = fs.get_inode_by_num(block_dev, parent)?;
    if !parent_inode.is_dir() {
        return Err(Ext4Error::not_dir());
    }
    let entry = find_named_entry_in_parent(fs, block_dev, parent, &parent_inode, name.as_bytes())?;
    let target_inode = fs.get_inode_by_num(block_dev, entry.ino)?;
    if !target_inode.is_dir() {
        return Err(Ext4Error::not_dir().with_operation("rmdir:target"));
    }
    let mut target_for_scan = target_inode;
    if !is_dir_empty(fs, block_dev, entry.ino, &mut target_for_scan)? {
        return Err(Ext4Error::not_empty());
    }
    preflight_inode_free(fs, block_dev, entry.ino, &target_inode)?;
    let parent_new_links = parent_inode.links_count_after_removing_directories(1)?;

    fs.with_metadata_transaction(block_dev, UNLINK_TRANSACTION_CREDITS, |fs, block_dev| {
        remove_named_entry_at(fs, block_dev, parent, &parent_inode, entry, name.as_bytes())?;
        let mut unlinked_target = target_inode;
        unlinked_target.i_links_count = 0;
        unlinked_target.i_size_lo = 0;
        unlinked_target.i_size_high = 0;
        fs.finalize_inode_update(
            block_dev,
            entry.ino,
            &mut unlinked_target,
            Ext4InodeMetadataUpdate::link_count_change(),
        )?;
        fs.add_orphan(block_dev, entry.ino)?;
        fs.set_inode_links_count(block_dev, parent, parent_new_links)?;
        fs.touch_parent_dir_for_entry_change(block_dev, parent)?;
        fs.inodetable_cache.flush(block_dev, entry.ino)?;
        fs.inodetable_cache.flush(block_dev, parent)?;
        fs.sync_superblock(block_dev)?;
        Ok(())
    })?;
    Ok(UnlinkOutcome {
        inode: entry.ino,
        remaining_links: 0,
    })
}

/// Remove a non-directory link from its parent directory.
pub fn unlink<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    link_path: &str,
) -> Ext4Result<UnlinkOutcome> {
    // Resolve the parent directory and target entry before mutating link
    // counts or directory contents.
    let norm_path = normalize_path(link_path);
    let (parent_path, child_name) = if let Some(pos) = norm_path.rfind('/') {
        let parent = if pos == 0 {
            "/".to_string()
        } else {
            norm_path[..pos].to_string()
        };
        let child = norm_path[pos + 1..].to_string();
        (parent, child)
    } else {
        ("/".to_string(), norm_path)
    };

    let (parent_ino, _) = match get_inode_with_num(fs, block_dev, &parent_path)? {
        Some(v) => v,
        None => return Err(Ext4Error::not_found()),
    };
    unlink_inode_at(
        fs,
        block_dev,
        parent_ino,
        FileName::new(child_name.as_bytes())?,
    )
}

fn find_dentry_in_dir_block(data: &[u8], name_bytes: &[u8]) -> Option<(u32, u8, usize)> {
    let block_bytes = data.len();
    let mut offset: usize = 0;
    while offset + 8 <= block_bytes {
        let inode = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        let rec_len = decode_directory_record_length(
            u16::from_le_bytes([data[offset + 4], data[offset + 5]]),
            block_bytes,
        );
        if rec_len < 8 || !rec_len.is_multiple_of(4) {
            break;
        }
        let name_len = data[offset + 6] as usize;
        let Some(entry_end) = offset.checked_add(rec_len) else {
            break;
        };
        if entry_end > block_bytes {
            break;
        }
        if name_len > 0 && offset + 8 + name_len <= entry_end {
            let name = &data[offset + 8..offset + 8 + name_len];
            if inode != 0 && name == name_bytes {
                return Some((inode, data[offset + 7], offset));
            }
        }
        if entry_end >= block_bytes {
            break;
        }
        offset = entry_end;
    }
    None
}

fn remove_dentry_in_dir_block(
    superblock: &Ext4Superblock,
    parent_ino_num: InodeNumber,
    parent_inode: &Ext4Inode,
    data: &mut [u8],
    entry: ParentDirEntry,
    name_bytes: &[u8],
) -> Ext4Result<bool> {
    let block_bytes = data.len();
    let entries_end = if crate::crc32c::ext4_superblock_has_metadata_csum(superblock) {
        block_bytes
            .checked_sub(usize::from(Ext4DirEntryTail::TAIL_LEN))
            .ok_or_else(|| Ext4Error::corrupted().with_operation("directory:delete_block_size"))?
    } else {
        block_bytes
    };
    let mut offset = 0usize;
    let mut previous = None;

    while offset < entries_end {
        let header = data
            .get(offset..offset + 8)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("directory:delete_header"))?;
        let inode = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let record_len =
            decode_directory_record_length(u16::from_le_bytes([header[4], header[5]]), block_bytes);
        let name_len = usize::from(header[6]);
        let record_end = offset.checked_add(record_len).ok_or_else(|| {
            Ext4Error::corrupted().with_operation("directory:delete_record_overflow")
        })?;
        let name_end = offset
            .checked_add(8)
            .and_then(|start| start.checked_add(name_len))
            .ok_or_else(Ext4Error::overflow)?;
        if record_len < 8
            || !record_len.is_multiple_of(4)
            || record_end > entries_end
            || name_end > record_end
        {
            return Err(Ext4Error::corrupted().with_operation("directory:delete_record"));
        }

        if offset == entry.offset {
            if inode != entry.ino.raw() || &data[offset + 8..name_end] != name_bytes {
                return Ok(false);
            }
            if let Some(previous_offset) = previous {
                let previous_raw_len =
                    u16::from_le_bytes([data[previous_offset + 4], data[previous_offset + 5]]);
                let previous_len = decode_directory_record_length(previous_raw_len, block_bytes);
                let merged_len = previous_len
                    .checked_add(record_len)
                    .ok_or_else(Ext4Error::overflow)?;
                let encoded =
                    encode_directory_record_length(merged_len, block_bytes).ok_or_else(|| {
                        Ext4Error::corrupted().with_operation("directory:delete_merged_rec_len")
                    })?;
                data[previous_offset + 4..previous_offset + 6]
                    .copy_from_slice(&encoded.to_le_bytes());
                data[offset..record_end].fill(0);
            } else {
                data[offset..offset + 4].fill(0);
                data[offset + 6..record_end].fill(0);
            }
            update_ext4_dirblock_csum32(
                superblock,
                parent_ino_num.raw(),
                parent_inode.i_generation,
                data,
            );
            return Ok(true);
        }
        if offset > entry.offset {
            return Ok(false);
        }
        previous = Some(offset);
        offset = record_end;
    }

    if offset != entries_end {
        return Err(Ext4Error::corrupted().with_operation("directory:delete_coverage"));
    }
    Ok(false)
}

fn try_remove_dentry_in_block<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    parent_ino_num: InodeNumber,
    parent_inode: &Ext4Inode,
    entry: ParentDirEntry,
    name_bytes: &[u8],
) -> Ext4Result<bool> {
    let superblock = &fs.superblock;
    let mut remove_result = Ok(false);
    fs.datablock_cache
        .modify_metadata(block_dev, entry.phys, |data| {
            remove_result = remove_dentry_in_dir_block(
                superblock,
                parent_ino_num,
                parent_inode,
                data,
                entry,
                name_bytes,
            );
        })?;
    let removed = remove_result?;
    if removed {
        fs.datablock_cache.flush_metadata(block_dev, entry.phys)?;
    }
    Ok(removed)
}

fn parent_dir_data_blocks<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    parent_ino: InodeNumber,
    parent_inode: &mut Ext4Inode,
) -> Ext4Result<alloc::vec::Vec<AbsoluteBN>> {
    let mut blocks: alloc::vec::Vec<AbsoluteBN> = if parent_inode.uses_extents() {
        resolve_inode_blocks(fs, block_dev, parent_ino, parent_inode)?
            .into_values()
            .collect()
    } else {
        let total_size = usize::try_from(fs.inode_size(parent_inode))
            .map_err(|_| Ext4Error::file_too_large())?;
        let block_bytes = fs.block_size();
        let total_blocks = if total_size == 0 {
            0
        } else {
            total_size.div_ceil(block_bytes)
        };
        let mut collected = alloc::vec::Vec::new();
        for lbn in 0..total_blocks {
            if let Some(phys) =
                resolve_inode_block(fs, block_dev, parent_ino, parent_inode, lbn as u32)?
            {
                collected.push(phys);
            }
        }
        collected
    };
    blocks.sort_unstable();
    blocks.dedup();
    Ok(blocks)
}

/// Finds a child name in `parent_inode` with one directory scan (htree or linear).
pub(crate) fn find_named_entry_in_parent<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    parent_ino: InodeNumber,
    parent_inode: &Ext4Inode,
    name_bytes: &[u8],
) -> Ext4Result<ParentDirEntry> {
    use crate::hashtree::{Ext4InodeHashTreeExt, HashTreeError, lookup_directory_entry};

    if !parent_inode.is_dir() {
        return Err(Ext4Error::not_dir());
    }

    if parent_inode.is_htree_indexed() {
        match lookup_directory_entry(fs, block_dev, parent_ino, parent_inode, name_bytes) {
            Ok(result) => {
                return Ok(ParentDirEntry {
                    ino: result.inode,
                    phys: result.block_num,
                    offset: result.offset,
                    file_type: result.file_type,
                });
            }
            Err(HashTreeError::EntryNotFound) => return Err(Ext4Error::not_found()),
            Err(error) => return Err(error.into_ext4("htree:parent_lookup")),
        }
    }

    let mut parent_inode = *parent_inode;
    for phys in parent_dir_data_blocks(fs, block_dev, parent_ino, &mut parent_inode)? {
        let cached = fs.datablock_cache.get_or_load(block_dev, phys)?;
        let data = &cached.data;
        let checksum_ok = if parent_inode.is_htree_indexed() {
            crate::checksum::verify_ext4_dx_checksum(
                &fs.superblock,
                parent_ino.raw(),
                parent_inode.i_generation,
                data,
            )
            .unwrap_or_else(|| {
                crate::checksum::verify_ext4_dirblock_checksum(
                    &fs.superblock,
                    parent_ino.raw(),
                    parent_inode.i_generation,
                    data,
                )
            })
        } else {
            crate::checksum::verify_ext4_dirblock_checksum(
                &fs.superblock,
                parent_ino.raw(),
                parent_inode.i_generation,
                data,
            )
        };
        if !checksum_ok {
            return Err(Ext4Error::checksum().with_operation("directory:lookup_block"));
        }
        if let Some((inode, file_type, offset)) = find_dentry_in_dir_block(data, name_bytes) {
            let ino = InodeNumber::new(inode).map_err(|_| Ext4Error::corrupted())?;
            return Ok(ParentDirEntry {
                ino,
                phys,
                offset,
                file_type,
            });
        }
    }

    Err(Ext4Error::not_found())
}

/// Removes a dentry on a block returned by [`find_named_entry_in_parent`].
pub(crate) fn remove_named_entry_at<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    parent_ino: InodeNumber,
    parent_inode: &Ext4Inode,
    entry: ParentDirEntry,
    name_bytes: &[u8],
) -> Ext4Result<()> {
    if try_remove_dentry_in_block(fs, block_dev, parent_ino, parent_inode, entry, name_bytes)? {
        Ok(())
    } else {
        Err(Ext4Error::not_found())
    }
}

/// Replaces the inode and file type of an existing, precisely located dentry.
pub(crate) fn replace_named_entry_at<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    parent_ino: InodeNumber,
    parent_inode: &Ext4Inode,
    entry: ParentDirEntry,
    name_bytes: &[u8],
    replacement: DentryReplacement,
) -> Ext4Result<()> {
    let superblock = &fs.superblock;
    let mut replaced = false;
    fs.datablock_cache
        .modify_metadata(block_dev, entry.phys, |data| {
            let offset = entry.offset;
            let Some(header) = data.get(offset..offset + 8) else {
                return;
            };
            let inode = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
            let record_len = decode_directory_record_length(
                u16::from_le_bytes([header[4], header[5]]),
                data.len(),
            );
            let name_len = usize::from(header[6]);
            let Some(record_end) = offset.checked_add(record_len) else {
                return;
            };
            let Some(name_end) = offset.checked_add(8 + name_len) else {
                return;
            };
            if record_len < 8
                || record_end > data.len()
                || name_end > record_end
                || inode != entry.ino.raw()
                || &data[offset + 8..name_end] != name_bytes
            {
                return;
            }

            data[offset..offset + 4].copy_from_slice(&replacement.inode.raw().to_le_bytes());
            data[offset + 7] = replacement.file_type;
            update_ext4_dirblock_csum32(
                superblock,
                parent_ino.raw(),
                parent_inode.i_generation,
                data,
            );
            replaced = true;
        })?;
    if replaced {
        fs.datablock_cache.flush_metadata(block_dev, entry.phys)?;
        Ok(())
    } else {
        Err(Ext4Error::corrupted().with_operation("directory:stale_entry_location"))
    }
}

fn remove_inodeentry_from_parentdir<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    parent_path: &str,
    child_name: &str,
) -> Ext4Result<()> {
    let parent_info = match get_inode_with_num(fs, block_dev, parent_path)? {
        Some(v) => v,
        None => return Err(Ext4Error::not_found()),
    };
    let (parent_ino_num, parent_inode) = parent_info;

    let entry = find_named_entry_in_parent(
        fs,
        block_dev,
        parent_ino_num,
        &parent_inode,
        child_name.as_bytes(),
    )?;
    remove_named_entry_at(
        fs,
        block_dev,
        parent_ino_num,
        &parent_inode,
        entry,
        child_name.as_bytes(),
    )?;
    fs.touch_parent_dir_for_entry_change(block_dev, parent_ino_num)?;
    Ok(())
}

/// Remove a directory tree.
pub fn delete_dir<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    path: &str,
) -> Ext4Result<()> {
    #[derive(Clone)]
    struct DirFrame {
        path: alloc::string::String,
        ino_num: InodeNumber,
        inode: Ext4Inode,
        parent_path: Option<alloc::string::String>,
        name_in_parent: Option<alloc::string::String>,
        stage: u8, // 0=scan, 1=cleanup
    }

    let norm_path = normalize_path(path);
    if norm_path == "/" {
        return Err(Ext4Error::busy());
    }
    let (root_ino_num, root_inode) = match get_file_inode(fs, block_dev, &norm_path) {
        Ok(Some(v)) => v,
        Ok(None) => return Err(Ext4Error::not_found()),
        Err(e) => return Err(e),
    };
    if !root_inode.is_dir() {
        return Err(Ext4Error::not_dir());
    }

    let (parent_path, child_name) = if norm_path == "/" {
        (None, None)
    } else if let Some(pos) = norm_path.rfind('/') {
        let parent = if pos == 0 {
            "/".to_string()
        } else {
            norm_path[..pos].to_string()
        };
        let child = norm_path[pos + 1..].to_string();
        (Some(parent), Some(child))
    } else {
        (Some("/".to_string()), Some(norm_path.clone()))
    };

    let mut stack: Vec<DirFrame> = Vec::new();
    stack.push(DirFrame {
        path: norm_path,
        ino_num: root_ino_num,
        inode: root_inode,
        parent_path,
        name_in_parent: child_name,
        stage: 0,
    });

    // Walk the directory tree with an explicit stack so deep trees do not rely
    // on recursion.
    while let Some(mut frame) = stack.pop() {
        // Stage 0 scans children and pushes subdirectories for a depth-first
        // traversal.
        if frame.stage == 0 {
            let dir_blocks = resolve_inode_blocks(fs, block_dev, frame.ino_num, &mut frame.inode)?;

            let mut to_descend: Vec<(
                alloc::string::String,
                InodeNumber,
                Ext4Inode,
                alloc::string::String,
            )> = Vec::new();
            let mut removed_child_dirs: u32 = 0;

            for &phys in dir_blocks.values() {
                // Collect child entries first to avoid nested mutable borrows of
                // `fs` while the data-block cache entry is live.
                let mut child_entries: Vec<(InodeNumber, alloc::string::String)> = Vec::new();
                {
                    let cached = fs.datablock_cache.get_or_load(block_dev, phys)?;
                    let data = &cached.data;
                    let iter = DirEntryIterator::new(data);
                    for (entry, _) in iter {
                        if entry.is_dot() || entry.is_dotdot() {
                            continue;
                        }
                        let child_name_bytes = entry.name.to_vec();
                        let child_name_str = match core::str::from_utf8(&child_name_bytes) {
                            Ok(s) => s,
                            Err(_) => {
                                continue;
                            }
                        };
                        let child_ino =
                            InodeNumber::new(entry.inode).map_err(|_| Ext4Error::corrupted())?;
                        child_entries.push((child_ino, child_name_str.to_string()));
                    }
                }

                for (child_ino, child_name) in child_entries {
                    let child_path = if frame.path == "/" {
                        alloc::format!("/{child_name}")
                    } else {
                        alloc::format!("{}/{}", frame.path, child_name)
                    };

                    let child_inode = fs.get_inode_by_num(block_dev, child_ino)?;

                    // Delete non-directory children immediately. Directories are
                    // deferred to the DFS stack.
                    if !child_inode.is_dir() {
                        delete_file(fs, block_dev, &child_path)?;
                        continue;
                    }

                    removed_child_dirs = removed_child_dirs
                        .checked_add(1)
                        .ok_or_else(Ext4Error::overflow)?;
                    to_descend.push((child_path, child_ino, child_inode, child_name));
                }
            }

            if removed_child_dirs != 0 {
                let current_inode = fs.get_inode_by_num(block_dev, frame.ino_num)?;
                let new_links =
                    current_inode.links_count_after_removing_directories(removed_child_dirs)?;
                fs.set_inode_links_count(block_dev, frame.ino_num, new_links)?;
            }

            // Push children in reverse so traversal order remains stable.
            let parent_path_for_children = frame.path.clone();

            frame.stage = 1;
            stack.push(frame);

            for (child_path, child_ino, child_inode, child_name) in to_descend.into_iter().rev() {
                stack.push(DirFrame {
                    path: child_path,
                    ino_num: child_ino,
                    inode: child_inode,
                    parent_path: Some(parent_path_for_children.clone()),
                    name_in_parent: Some(child_name),
                    stage: 0,
                });
            }
            continue;
        }

        // Stage 1 runs after all children are removed, so the directory should
        // now contain only `.` and `..`.
        let mut cur_inode = fs.get_inode_by_num(block_dev, frame.ino_num)?;

        // Remove the entry from the parent directory and then fix the parent's
        // directory link count.
        if let (Some(pp), Some(name)) = (&frame.parent_path, &frame.name_in_parent) {
            remove_inodeentry_from_parentdir(fs, block_dev, pp, name)?;

            let (pino, parent_inode) =
                get_inode_with_num(fs, block_dev, pp)?.ok_or(Ext4Error::corrupted())?;
            let parent_new_links = parent_inode.decremented_links_count()?;
            fs.set_inode_links_count(block_dev, pino, parent_new_links)?;
        }

        free_inode(fs, block_dev, frame.ino_num, &mut cur_inode)?;

        // Keep the group-descriptor directory count in sync with the removal.
        let (group_idx, _idx_in_group) = fs.inode_allocator.global_to_group(frame.ino_num)?;
        if let Some(desc) = fs.get_group_desc_mut(group_idx) {
            let before = desc.used_dirs_count();
            let new_count = before.saturating_sub(1);
            desc.bg_used_dirs_count_lo = (new_count & 0xFFFF) as u16;
            desc.bg_used_dirs_count_hi = (new_count >> 16) as u16;
        }
    }

    Ok(())
}

/// Check whether a directory inode is empty (contains only `.` and `..`).
///
/// Returns `Ok(true)` if the directory has no real children, `Ok(false)` otherwise.
fn checked_directory_record(
    data: &[u8],
    offset: usize,
    inode_count: u32,
) -> Ext4Result<(u32, &[u8], usize)> {
    const HEADER_LEN: usize = 8;
    const MIN_RECORD_LEN: usize = 12;

    let header = data
        .get(offset..offset + HEADER_LEN)
        .ok_or_else(|| Ext4Error::corrupted().with_operation("directory:record_header"))?;
    let inode = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let record_len =
        decode_directory_record_length(u16::from_le_bytes([header[4], header[5]]), data.len());
    let name_len = usize::from(header[6]);
    let minimum_len = HEADER_LEN
        .checked_add(name_len)
        .and_then(|length| length.checked_add(3))
        .map(|length| length & !3)
        .ok_or_else(Ext4Error::overflow)?;
    let record_end = offset
        .checked_add(record_len)
        .ok_or_else(Ext4Error::overflow)?;
    let name_end = offset
        .checked_add(HEADER_LEN)
        .and_then(|start| start.checked_add(name_len))
        .ok_or_else(Ext4Error::overflow)?;
    let last_valid_start = data.len().saturating_sub(MIN_RECORD_LEN);
    if record_len < HEADER_LEN
        || !record_len.is_multiple_of(4)
        || record_len < minimum_len
        || record_end > data.len()
        || (record_end > last_valid_start && record_end != data.len())
        || name_end > record_end
        || inode > inode_count
    {
        return Err(Ext4Error::corrupted().with_operation("directory:record"));
    }
    Ok((inode, &data[offset + HEADER_LEN..name_end], record_end))
}

pub fn is_dir_empty<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
) -> Ext4Result<bool> {
    const DOT_RECORD_LEN: u64 = 12;
    const DOTDOT_RECORD_LEN: u64 = 12;

    if !inode.is_dir() {
        return Err(Ext4Error::not_dir());
    }
    let directory_size = fs.inode_size(inode);
    if directory_size < DOT_RECORD_LEN + DOTDOT_RECORD_LEN {
        return Err(Ext4Error::corrupted().with_operation("directory:empty_size"));
    }

    let block_size = fs.block_size();
    let total_blocks = directory_size.div_ceil(block_size as u64);
    let dir_blocks = resolve_inode_blocks(fs, block_dev, inode_num, inode)?;
    if !dir_blocks.contains_key(&0) {
        return Err(Ext4Error::corrupted().with_operation("directory:first_block_hole"));
    }
    let superblock = fs.superblock;
    let inode_count = superblock.s_inodes_count;
    let generation = inode.i_generation;
    let indexed = inode.is_htree_indexed();
    let mut first_block_entries = 0usize;

    for logical in 0..total_blocks {
        let logical = u32::try_from(logical).map_err(|_| Ext4Error::file_too_large())?;
        let Some(&phys) = dir_blocks.get(&logical) else {
            continue;
        };
        let cached = fs.datablock_cache.get_or_load(block_dev, phys)?;
        let data = &cached.data;
        let dx_checksum = if indexed {
            crate::checksum::verify_ext4_dx_checksum(&superblock, inode_num.raw(), generation, data)
        } else {
            None
        };
        let checksum_ok = dx_checksum.unwrap_or_else(|| {
            let has_required_tail = !crate::crc32c::ext4_superblock_has_metadata_csum(&superblock)
                || data.get(data.len().saturating_sub(5)) == Some(&Ext4DirEntryTail::RESERVED_FT);
            has_required_tail
                && crate::checksum::verify_ext4_dirblock_checksum(
                    &superblock,
                    inode_num.raw(),
                    generation,
                    data,
                )
        });
        if !checksum_ok {
            return Err(Ext4Error::checksum().with_operation("directory:block"));
        };

        let mut offset = 0usize;
        while offset < data.len() {
            let (entry_inode, name, next_offset) =
                checked_directory_record(data, offset, inode_count)?;
            if logical == 0 && first_block_entries == 0 {
                if entry_inode != inode_num.raw() || name != b"." {
                    return Err(Ext4Error::corrupted().with_operation("directory:dot"));
                }
                first_block_entries += 1;
            } else if logical == 0 && first_block_entries == 1 {
                if entry_inode == 0 || name != b".." {
                    return Err(Ext4Error::corrupted().with_operation("directory:dotdot"));
                }
                first_block_entries += 1;
            } else if entry_inode != 0 {
                return Ok(false);
            }
            offset = next_offset;
        }
    }
    if first_block_entries != 2 {
        return Err(Ext4Error::corrupted().with_operation("directory:dot_entries"));
    }
    Ok(true)
}

/// Remove a non-directory inode from its parent directory.
pub fn delete_file<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    path: &str,
) -> Ext4Result<()> {
    let outcome = unlink(fs, block_dev, path)?;
    if outcome.requires_reap() {
        reap_unlinked_inode(fs, block_dev, outcome.inode)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entries::{Ext4DirEntry2, decode_directory_record_length};

    fn write_directory_record(
        block: &mut [u8],
        offset: usize,
        inode: u32,
        record_len: u16,
        file_type: u8,
        name: &[u8],
    ) {
        block[offset..offset + 4].copy_from_slice(&inode.to_le_bytes());
        block[offset + 4..offset + 6].copy_from_slice(&record_len.to_le_bytes());
        block[offset + 6] = name.len() as u8;
        block[offset + 7] = file_type;
        block[offset + 8..offset + 8 + name.len()].copy_from_slice(name);
    }

    #[test]
    fn delete_merges_the_previous_record_and_wipes_the_removed_record() {
        let mut block = alloc::vec![0xa5; 4096];
        write_directory_record(
            &mut block,
            0,
            11,
            16,
            Ext4DirEntry2::EXT4_FT_REG_FILE,
            b"before",
        );
        write_directory_record(
            &mut block,
            16,
            12,
            16,
            Ext4DirEntry2::EXT4_FT_REG_FILE,
            b"target",
        );
        write_directory_record(
            &mut block,
            32,
            13,
            4064,
            Ext4DirEntry2::EXT4_FT_REG_FILE,
            b"after",
        );
        let superblock = Ext4Superblock {
            s_feature_ro_compat: 0,
            ..Default::default()
        };
        let parent_ino = InodeNumber::new(2).expect("valid parent inode");
        let parent_inode = Ext4Inode {
            i_generation: 7,
            ..Default::default()
        };
        let entry = ParentDirEntry {
            ino: InodeNumber::new(12).expect("valid target inode"),
            phys: AbsoluteBN::new(1),
            offset: 16,
            file_type: Ext4DirEntry2::EXT4_FT_REG_FILE,
        };

        assert!(
            remove_dentry_in_dir_block(
                &superblock,
                parent_ino,
                &parent_inode,
                &mut block,
                entry,
                b"target",
            )
            .expect("delete target record")
        );
        assert_eq!(
            decode_directory_record_length(u16::from_le_bytes([block[4], block[5]]), block.len()),
            32
        );
        assert_eq!(&block[16..32], &[0; 16]);
        assert_eq!(u32::from_le_bytes(block[32..36].try_into().unwrap()), 13);
    }

    #[test]
    fn delete_first_record_preserves_compact_record_length() {
        let mut block = alloc::vec![0xa5; 65_536];
        write_directory_record(
            &mut block,
            0,
            12,
            0,
            Ext4DirEntry2::EXT4_FT_REG_FILE,
            b"target",
        );
        let superblock = Ext4Superblock {
            s_feature_ro_compat: 0,
            ..Default::default()
        };
        let parent_ino = InodeNumber::new(2).expect("valid parent inode");
        let parent_inode = Ext4Inode::default();
        let entry = ParentDirEntry {
            ino: InodeNumber::new(12).expect("valid target inode"),
            phys: AbsoluteBN::new(1),
            offset: 0,
            file_type: Ext4DirEntry2::EXT4_FT_REG_FILE,
        };

        assert!(
            remove_dentry_in_dir_block(
                &superblock,
                parent_ino,
                &parent_inode,
                &mut block,
                entry,
                b"target",
            )
            .expect("delete first target record")
        );
        assert_eq!(u32::from_le_bytes(block[..4].try_into().unwrap()), 0);
        assert_eq!(u16::from_le_bytes(block[4..6].try_into().unwrap()), 0);
        assert!(block[6..].iter().all(|byte| *byte == 0));
    }
}
