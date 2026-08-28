//! Directory entry insertion helpers.
use super::FileName;
use crate::{
    blockdev::*,
    bmalloc::{AbsoluteBN, InodeNumber},
    checksum::update_ext4_dirblock_csum32,
    crc32c::ext4_superblock_has_metadata_csum,
    disknode::*,
    endian::DiskFormat,
    entries::*,
    error::*,
    ext4::*,
    extents_tree::*,
    hashtree::{insert_indexed_directory_entry, make_indexed_directory},
    loopfile::*,
    metadata::Ext4InodeMetadataUpdate,
    superblock::Ext4Superblock,
};

/// Inserts a child entry into a parent directory, extending the directory if needed.
///
/// The flow first scans existing directory blocks for reusable space, then falls
/// back to allocating a new block when no slot can absorb the new entry.
pub fn insert_dir_entry<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    parent_ino_num: InodeNumber,
    parent_inode: &mut Ext4Inode,
    child_ino: InodeNumber,
    child_name: &str,
    file_type: u8,
) -> Ext4Result<()> {
    insert_dir_entry_raw(
        fs,
        device,
        parent_ino_num,
        parent_inode,
        child_ino,
        FileName::new(child_name.as_bytes())?,
        file_type,
    )
}

/// Inserts one validated raw child name into a parent directory.
pub(crate) fn insert_dir_entry_raw<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    parent_ino_num: InodeNumber,
    parent_inode: &mut Ext4Inode,
    child_ino: InodeNumber,
    child_name: FileName<'_>,
    file_type: u8,
) -> Ext4Result<()> {
    if parent_inode.i_flags & Ext4Inode::EXT4_INDEX_FL != 0 {
        return insert_indexed_directory_entry(
            fs,
            device,
            parent_ino_num,
            parent_inode,
            child_ino,
            child_name,
            file_type,
        );
    }

    let has_checksum = ext4_superblock_has_metadata_csum(&fs.superblock);
    let name_bytes = child_name.as_bytes();
    let name_len = name_bytes.len();
    let new_rec_len = Ext4DirEntry2::entry_len(name_len as u8) as usize;
    let new_entry = Ext4DirEntry2::new(
        child_ino.raw(),
        Ext4DirEntry2::entry_len(name_len as u8),
        file_type,
        name_bytes,
    );

    let total_size =
        usize::try_from(fs.inode_size(parent_inode)).map_err(|_| Ext4Error::file_too_large())?;
    let block_bytes = fs.block_size();
    let total_blocks = if total_size == 0 {
        0
    } else {
        total_size.div_ceil(block_bytes)
    };

    let mut inserted = false;
    let mut modified_phys: Option<AbsoluteBN> = None;

    // Try to satisfy the insertion inside already mapped directory blocks first.
    let blocks = resolve_inode_blocks(fs, device, parent_ino_num, parent_inode)?;

    for lbn in 0..total_blocks {
        if modified_phys.is_some() {
            break;
        }

        let phys = match blocks.get(&(lbn as u32)) {
            Some(&b) => b,
            None => {
                return Err(Ext4Error::corrupted());
            }
        };

        fs.datablock_cache.modify_metadata(device, phys, |data| {
            if inserted {
                return;
            }

            let block_bytes = data.len();

            // Walk the block linearly and either reuse a free record or split an
            // oversized live record to create room for the new entry.
            let mut offset = 0usize;
            while offset + 8 <= block_bytes {
                let inode = u32::from_le_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]);
                let rec_len = u16::from_le_bytes([data[offset + 4], data[offset + 5]]) as usize;
                let rec_type = data[offset + 7];
                if rec_len < 8 {
                    return;
                }
                let entry_end = offset + rec_len;
                if entry_end > block_bytes {
                    return;
                }
                if rec_type == Ext4DirEntryTail::RESERVED_FT {
                    return;
                }

                if inode == 0 {
                    if rec_len >= new_rec_len {
                        let mut full_entry = new_entry;
                        full_entry.rec_len = rec_len as u16;
                        full_entry.to_disk_bytes(&mut data[offset..offset + 8]);
                        let nlen = full_entry.name_len as usize;
                        data[offset + 8..offset + 8 + nlen]
                            .copy_from_slice(&full_entry.name[..nlen]);
                        inserted = true;
                        modified_phys = Some(phys);
                        update_ext4_dirblock_csum32(
                            &fs.superblock,
                            parent_ino_num.raw(),
                            parent_inode.i_generation,
                            data,
                        );
                    }
                    return;
                }

                let cur_name_len = data[offset + 6] as usize;
                let mut ideal = 8 + cur_name_len;
                ideal = (ideal + 3) & !3;
                if ideal <= rec_len {
                    let tail = rec_len - ideal;
                    if tail >= new_rec_len {
                        let ideal_bytes = (ideal as u16).to_le_bytes();
                        data[offset + 4] = ideal_bytes[0];
                        data[offset + 5] = ideal_bytes[1];

                        let new_off = offset + ideal;
                        let mut full_entry = new_entry;
                        full_entry.rec_len = tail as u16;
                        full_entry.to_disk_bytes(&mut data[new_off..new_off + 8]);
                        let nlen = full_entry.name_len as usize;
                        data[new_off + 8..new_off + 8 + nlen]
                            .copy_from_slice(&full_entry.name[..nlen]);
                        inserted = true;
                        modified_phys = Some(phys);
                        update_ext4_dirblock_csum32(
                            &fs.superblock,
                            parent_ino_num.raw(),
                            parent_inode.i_generation,
                            data,
                        );
                        return;
                    }
                }

                if entry_end == block_bytes {
                    return;
                }
                offset = entry_end;
            }
        })?;
    }

    if let Some(modified_block) = modified_phys {
        // Publish the modified directory block before subsequent lookup.
        fs.datablock_cache.flush_metadata(device, modified_block)?;
        fs.touch_parent_dir_for_entry_change(device, parent_ino_num)?;
        return Ok(());
    }

    if total_blocks == 1
        && fs
            .superblock
            .has_feature_compat(Ext4Superblock::EXT4_FEATURE_COMPAT_DIR_INDEX)
    {
        return make_indexed_directory(
            fs,
            device,
            parent_ino_num,
            parent_inode,
            child_ino,
            child_name,
            file_type,
        );
    }

    let block_bytes = fs.block_size();
    let old_blocks = if total_size == 0 {
        0
    } else {
        total_size.div_ceil(block_bytes)
    };
    let new_lbn = old_blocks as u32;
    if (!fs.superblock.has_extents() || !parent_inode.uses_extents()) && old_blocks >= 12 {
        return Err(Ext4Error::unsupported());
    }

    let new_size = total_size
        .checked_add(block_bytes)
        .ok_or_else(Ext4Error::overflow)?;
    let huge_file_feature = fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
    let cur = parent_inode.blocks_count(block_bytes as u32, huge_file_feature);
    let newv = cur
        .checked_add(block_bytes as u64 / 512)
        .ok_or_else(Ext4Error::overflow)?;
    let mut accounting_check = *parent_inode;
    accounting_check.set_blocks_count(newv, block_bytes as u32, huge_file_feature)?;

    // No existing record could host the child, so append a fresh directory block.
    let new_block = fs.alloc_block(device)?;
    parent_inode.set_size(new_size as u64);
    parent_inode.set_blocks_count(newv, block_bytes as u32, huge_file_feature)?;

    if fs.superblock.has_extents() && parent_inode.uses_extents() {
        let new_ext = Ext4Extent::new(new_lbn, new_block.raw(), 1);
        let mut tree = ExtentTree::with_filesystem(parent_inode, fs, parent_ino_num);
        tree.insert_extent(fs, new_ext, device)?;
    } else {
        parent_inode.i_block[old_blocks] = new_block.to_u32()?;
    }

    fs.datablock_cache
        .modify_new_metadata(device, new_block, |data| {
            for b in data.iter_mut() {
                *b = 0;
            }
            // A new block starts with exactly one live record and an optional checksum tail.
            let block_size = data.len();
            let mut full_entry = new_entry;
            full_entry.rec_len = if has_checksum {
                (block_size - Ext4DirEntryTail::TAIL_LEN as usize) as u16
            } else {
                block_size as u16
            };
            full_entry.to_disk_bytes(&mut data[0..8]);
            let nlen = full_entry.name_len as usize;
            data[8..8 + nlen].copy_from_slice(&full_entry.name[..nlen]);
            if has_checksum {
                let tail = Ext4DirEntryTail::new();
                let tail_offset = block_size - Ext4DirEntryTail::TAIL_LEN as usize;
                tail.to_disk_bytes(
                    &mut data[tail_offset..tail_offset + Ext4DirEntryTail::TAIL_LEN as usize],
                );
                update_ext4_dirblock_csum32(
                    &fs.superblock,
                    parent_ino_num.raw(),
                    parent_inode.i_generation,
                    data,
                );
            }
        })?;

    // Immediately write the new directory block to disk so it is visible
    // to subsequent lookups.
    fs.datablock_cache.flush_metadata(device, new_block)?;

    fs.finalize_inode_update(
        device,
        parent_ino_num,
        parent_inode,
        Ext4InodeMetadataUpdate::parent_dir_change(),
    )?;

    Ok(())
}
