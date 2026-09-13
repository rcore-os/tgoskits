//! Serialized file reads and path-based symlink traversal.

use super::*;

/// Read the whole file at `path`.
pub fn read_file<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> Ext4Result<Vec<u8>> {
    read_file_follow(device, fs, path, 0)
}

pub fn read_inode_data_into<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    offset: u64,
    dst: &mut [u8],
) -> Ext4Result<usize> {
    if dst.is_empty() {
        return Ok(0);
    }

    let mut inode = fs.get_inode_by_num(device, inode_num)?;
    let file_size = inode.size();
    if offset >= file_size {
        return Ok(0);
    }

    if inode.is_symlink() {
        let target = read_symlink_target(device, fs, inode_num, &mut inode)?;
        let start = offset as usize;
        let available = target.len().saturating_sub(start);
        let to_read = core::cmp::min(dst.len(), available);
        dst[..to_read].copy_from_slice(&target[start..start + to_read]);
        return Ok(to_read);
    }

    if !inode.is_file() {
        return Err(if inode.is_dir() {
            Ext4Error::is_dir()
        } else {
            Ext4Error::unsupported()
        });
    }

    let to_read = core::cmp::min(dst.len() as u64, file_size - offset) as usize;
    let block_size = fs.block_size();
    let block_bytes = block_size as u64;
    let end = offset + to_read as u64;
    let start_lbn = offset / block_bytes;
    let end_lbn = (end - 1) / block_bytes;

    let mut copied = 0usize;
    if inode.uses_extents() {
        let mut tree = ExtentTree::with_filesystem(&mut inode, fs, inode_num);
        let runs = tree.initialized_runs_in_range(device, start_lbn as u32, end_lbn as u32)?;
        let mut lbn = start_lbn;
        let max_run_blocks = (MAX_RUN_IO_BYTES / block_size).max(1) as u32;
        for run in runs {
            let run_lbn = u64::from(run.logical_start);
            while lbn < run_lbn {
                let zero_len = copy_len_for_lbn(offset, end, lbn, block_bytes)?;
                dst[copied..copied + zero_len].fill(0);
                copied += zero_len;
                lbn += 1;
            }

            let mut run_block_offset = 0u32;
            while run_block_offset < run.len {
                let part_blocks = (run.len - run_block_offset).min(max_run_blocks);
                let phys = run.physical_start.checked_add(run_block_offset)?;
                let run_bytes = block_size
                    .checked_mul(part_blocks as usize)
                    .ok_or_else(Ext4Error::overflow)?;
                let mut run_buf = alloc::vec![0; run_bytes];
                fs.datablock_cache
                    .read_run(device, phys, part_blocks, &mut run_buf)?;

                for off in 0..part_blocks {
                    let current_lbn = run_lbn + u64::from(run_block_offset + off);
                    let src_len = copy_len_for_lbn(offset, end, current_lbn, block_bytes)?;
                    let lbn_start = current_lbn * block_bytes;
                    let src_off = (core::cmp::max(offset, lbn_start) - lbn_start) as usize;
                    let run_off = off as usize * block_size + src_off;
                    dst[copied..copied + src_len]
                        .copy_from_slice(&run_buf[run_off..run_off + src_len]);
                    copied += src_len;
                    lbn = current_lbn + 1;
                }
                run_block_offset += part_blocks;
            }
        }
        while lbn <= end_lbn {
            let zero_len = copy_len_for_lbn(offset, end, lbn, block_bytes)?;
            dst[copied..copied + zero_len].fill(0);
            copied += zero_len;
            lbn += 1;
        }
    } else {
        let mut lbn = start_lbn;
        while lbn <= end_lbn {
            let copy_len = copy_len_for_lbn(offset, end, lbn, block_bytes)?;
            if let Some(phys) = resolve_inode_block(fs, device, inode_num, &mut inode, lbn as u32)?
            {
                let cached = fs.datablock_cache.get_or_load(device, phys)?;
                let lbn_start = lbn * block_bytes;
                let src_off = (core::cmp::max(offset, lbn_start) - lbn_start) as usize;
                dst[copied..copied + copy_len]
                    .copy_from_slice(&cached.data[src_off..src_off + copy_len]);
            } else {
                dst[copied..copied + copy_len].fill(0);
            }
            copied += copy_len;
            lbn += 1;
        }
    }

    Ok(copied)
}

fn copy_len_for_lbn(offset: u64, end: u64, lbn: u64, block_bytes: u64) -> Ext4Result<usize> {
    let lbn_start = lbn.saturating_mul(block_bytes);
    let lbn_end = lbn_start.saturating_add(block_bytes);
    usize::try_from(core::cmp::min(end, lbn_end) - core::cmp::max(offset, lbn_start))
        .map_err(|_| Ext4Error::overflow())
}

fn read_file_follow<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    depth: usize,
) -> Ext4Result<Vec<u8>> {
    if depth > 8 {
        return Err(Ext4Error::invalid_input());
    }

    let (inode_num, mut inode) = match get_file_inode(fs, device, path) {
        Ok(Some((ino_num, ino))) => (ino_num, ino),
        Ok(None) => return Err(Ext4Error::not_found()),
        Err(e) => return Err(e),
    };

    if inode.is_symlink() {
        let target_bytes = read_symlink_target(device, fs, inode_num, &mut inode)?;
        let target = match core::str::from_utf8(&target_bytes) {
            Ok(s) => s,
            Err(_) => return Err(Ext4Error::corrupted()),
        };
        let resolved = resolve_symlink_path(path, target);
        return read_file_follow(device, fs, &resolved, depth + 1);
    }

    if !inode.is_file() {
        return Err(if inode.is_dir() {
            Ext4Error::is_dir()
        } else {
            Ext4Error::unsupported()
        });
    }

    let size = inode.size() as usize;
    if size == 0 {
        fs.touch_inode_atime_if_needed(device, inode_num)?;
        return Ok(Vec::new());
    }

    let block_bytes = fs.block_size();
    let total_blocks = size.div_ceil(block_bytes);

    let mut buf = Vec::with_capacity(size);

    if inode.uses_extents() {
        let blocks = resolve_inode_blocks(fs, device, inode_num, &mut inode)?;
        append_extent_logical_blocks(device, fs, &blocks, total_blocks, &mut buf)?;
    } else {
        for lbn in 0..total_blocks {
            let logical = u32::try_from(lbn).map_err(|_| Ext4Error::file_too_large())?;
            match resolve_inode_block(fs, device, inode_num, &mut inode, logical)? {
                Some(phys) => {
                    let cached = fs.datablock_cache.get_or_load(device, phys)?;
                    buf.extend_from_slice(&cached.data);
                }
                None => append_zero_block(&mut buf, block_bytes)?,
            }
        }
    }

    buf.truncate(size);

    fs.touch_inode_atime_if_needed(device, inode_num)?;

    Ok(buf)
}

fn read_symlink_target<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
) -> Ext4Result<Vec<u8>> {
    let size = inode.size() as usize;
    if size == 0 {
        return Ok(Vec::new());
    }

    // Fast symlinks consume no data blocks. Length alone is insufficient:
    // e2fsprogs stores a 60-byte target in a regular data block.
    let huge_file_feature = fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
    if size <= 60 && inode.blocks_count(fs.block_size() as u32, huge_file_feature) == 0 {
        let mut raw = [0u8; 60];
        for (i, word) in inode.i_block.iter().take(15).enumerate() {
            raw[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        return Ok(raw[..size].to_vec());
    }

    let block_bytes = fs.block_size();
    let total_blocks = size.div_ceil(block_bytes);
    let mut buf = Vec::with_capacity(size);

    if inode.uses_extents() {
        let blocks = resolve_inode_blocks(fs, device, inode_num, inode)?;
        append_extent_logical_blocks(device, fs, &blocks, total_blocks, &mut buf)?;
    } else {
        for lbn in 0..total_blocks {
            let logical = u32::try_from(lbn).map_err(|_| Ext4Error::file_too_large())?;
            match resolve_inode_block(fs, device, inode_num, inode, logical)? {
                Some(phys) => {
                    let cached = fs.datablock_cache.get_or_load(device, phys)?;
                    buf.extend_from_slice(&cached.data);
                }
                None => append_zero_block(&mut buf, block_bytes)?,
            }
        }
    }

    buf.truncate(size);

    Ok(buf)
}

fn resolve_symlink_path(current_path: &str, target: &str) -> String {
    if target.starts_with('/') {
        return normalize_path(target);
    }
    let parent = match current_path.rfind('/') {
        Some(0) | None => "/",
        Some(pos) => &current_path[..pos],
    };
    let mut combined = String::new();
    if parent == "/" {
        combined.push('/');
        combined.push_str(target);
    } else {
        combined.push_str(parent);
        combined.push('/');
        combined.push_str(target);
    }
    normalize_path(&combined)
}

fn append_extent_logical_blocks<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    mappings: &alloc::collections::BTreeMap<u32, AbsoluteBN>,
    total_blocks: usize,
    buffer: &mut Vec<u8>,
) -> Ext4Result<()> {
    let block_size = fs.block_size();
    let total_blocks = u64::try_from(total_blocks).map_err(|_| Ext4Error::file_too_large())?;
    let mapped_blocks = u64::try_from(mappings.len()).map_err(|_| Ext4Error::file_too_large())?;
    let dense = mapped_blocks == total_blocks
        && mappings.first_key_value().map(|(&key, _)| key) == Some(0)
        && mappings
            .last_key_value()
            .is_some_and(|(&key, _)| u64::from(key) + 1 == total_blocks);
    if dense {
        for &physical in mappings.values() {
            let cached = fs.datablock_cache.get_or_load(device, physical)?;
            buffer.extend_from_slice(&cached.data);
        }
        return Ok(());
    }

    let mut next_logical = 0u64;
    for (&logical, &physical) in mappings {
        let logical = u64::from(logical);
        if logical >= total_blocks {
            break;
        }
        while next_logical < logical {
            append_zero_block(buffer, block_size)?;
            next_logical += 1;
        }
        let cached = fs.datablock_cache.get_or_load(device, physical)?;
        buffer.extend_from_slice(&cached.data);
        next_logical = logical + 1;
    }
    while next_logical < total_blocks {
        append_zero_block(buffer, block_size)?;
        next_logical += 1;
    }
    Ok(())
}

fn append_zero_block(buffer: &mut Vec<u8>, block_bytes: usize) -> Ext4Result<()> {
    let new_len = buffer
        .len()
        .checked_add(block_bytes)
        .ok_or_else(Ext4Error::overflow)?;
    buffer.resize(new_len, 0);
    Ok(())
}
