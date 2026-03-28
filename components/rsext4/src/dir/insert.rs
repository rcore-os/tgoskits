//! Directory entry insertion helpers.

use log::error;

use crate::{
    blockdev::*, bmalloc::InodeNumber, checksum::update_ext4_dirblock_csum32, config::*,
    crc32c::ext4_superblock_has_metadata_csum, disknode::*, endian::DiskFormat, entries::*,
    error::*, ext4::*, extents_tree::*, loopfile::*, metadata::Ext4InodeMetadataUpdate,
};

/// Inserts a child entry into a parent directory, extending the directory if needed.
///
/// The flow first scans existing directory blocks for reusable space, then falls
/// back to allocating a new block when no slot can absorb the new entry.
pub fn insert_dir_entry<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    parent_ino_num: InodeNumber,
    parent_inode: &mut Ext4Inode,
    child_ino: InodeNumber,
    child_name: &str,
    file_type: u8,
) -> Ext4Result<()> {
    let has_checksum = ext4_superblock_has_metadata_csum(&fs.superblock);
    let name_bytes = child_name.as_bytes();
    let name_len = core::cmp::min(name_bytes.len(), Ext4DirEntry2::MAX_NAME_LEN as usize);
    let new_rec_len = Ext4DirEntry2::entry_len(name_len as u8) as usize;
    let new_entry = Ext4DirEntry2::new(
        child_ino.raw(),
        Ext4DirEntry2::entry_len(name_len as u8),
        file_type,
        &name_bytes[..name_len],
    );

    let total_size = parent_inode.size() as usize;
    let block_bytes = BLOCK_SIZE;
    let total_blocks = if total_size == 0 {
        0
    } else {
        total_size.div_ceil(block_bytes)
    };

    let mut inserted = false;

    // Try to satisfy the insertion inside already mapped directory blocks first.
    let blocks = resolve_inode_block_allextend(fs, device, parent_inode)?;

    for lbn in 0..total_blocks {
        if inserted {
            break;
        }

        let phys = match blocks.get(&(lbn as u32)) {
            Some(&b) => b,
            None => {
                error!(
                    "insert_dir_entry: missing extent mapping for parent_ino={parent_ino_num} \
                     lbn={lbn} name={child_name:?}"
                );
                return Err(Ext4Error::corrupted());
            }
        };

        let _ = fs.datablock_cache.modify(device, phys, |data| {
            if inserted {
                return;
            }

            let block_bytes = BLOCK_SIZE;

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
        });
    }

    if inserted {
        fs.touch_parent_dir_for_entry_change(device, parent_ino_num)?;
        return Ok(());
    }

    // No existing record could host the child, so append a fresh directory block.
    let new_block = fs.alloc_block(device)?;

    let block_bytes = BLOCK_SIZE;
    let old_blocks = if total_size == 0 {
        0
    } else {
        total_size.div_ceil(block_bytes)
    };
    let new_lbn = old_blocks as u32;

    if fs.superblock.has_extents() && parent_inode.have_extend_header_and_use_extend() {
        let new_ext = Ext4Extent::new(new_lbn, new_block.raw(), 1);
        let mut tree = ExtentTree::new(parent_inode);
        tree.insert_extent(fs, new_ext, device)?;
    } else {
        if old_blocks >= 12 {
            return Err(Ext4Error::unsupported());
        }
        parent_inode.i_block[old_blocks] = new_block.to_u32()?;
    }

    let new_size = total_size + block_bytes;
    parent_inode.i_size_lo = new_size as u32;
    parent_inode.i_size_high = ((new_size as u64) >> 32) as u32;
    let cur = parent_inode.blocks_count();
    let add_sectors = BLOCK_SIZE as u64 / 512;
    let newv = cur.saturating_add(add_sectors);
    parent_inode.i_blocks_lo = (newv & 0xffff_ffff) as u32;
    parent_inode.l_i_blocks_high = ((newv >> 32) & 0xffff) as u16;

    fs.datablock_cache.modify(device, new_block, |data| {
        for b in data.iter_mut() {
            *b = 0;
        }
        // A new block starts with exactly one live record and an optional checksum tail.
        let mut full_entry = new_entry;
        full_entry.rec_len = if has_checksum {
            (BLOCK_SIZE - Ext4DirEntryTail::TAIL_LEN as usize) as u16
        } else {
            BLOCK_SIZE as u16
        };
        full_entry.to_disk_bytes(&mut data[0..8]);
        let nlen = full_entry.name_len as usize;
        data[8..8 + nlen].copy_from_slice(&full_entry.name[..nlen]);
        if has_checksum {
            let tail = Ext4DirEntryTail::new();
            let tail_offset = BLOCK_SIZE - Ext4DirEntryTail::TAIL_LEN as usize;
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

    fs.finalize_inode_update(
        device,
        parent_ino_num,
        parent_inode,
        Ext4InodeMetadataUpdate::parent_dir_change(),
    )?;

    Ok(())
}
