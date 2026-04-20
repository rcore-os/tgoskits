use super::*;

pub fn truncate<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    truncate_size: u64,
) -> Ext4Result<()> {
    let norm_path = split_paren_child_and_tranlatevalid(path);

    // Resolve the target inode once, then delegate to the inode-based helper.
    let (inode_num, _inode) = match get_inode_with_num(fs, device, &norm_path).ok().flatten() {
        Some(v) => v,
        None => return Err(Ext4Error::not_found()),
    };

    truncate_inode(device, fs, inode_num, truncate_size)
}

fn truncate_inode<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    truncate_size: u64,
) -> Ext4Result<()> {
    let mut inode = fs.get_inode_by_num(device, inode_num)?;

    if !inode.is_file() {
        warn!("trubcate abnormal file")
    } else if inode.is_symlink() {
        error!("Can't truncate symlink file!");
        return Err(Ext4Error::unsupported());
    }

    let old_size = inode.size();
    if truncate_size == old_size {
        return Ok(());
    }

    let block_bytes = BLOCK_SIZE as u64;
    let old_blocks = if old_size == 0 {
        0u64
    } else {
        old_size.div_ceil(block_bytes)
    };
    let new_blocks = if truncate_size == 0 {
        0u64
    } else {
        truncate_size.div_ceil(block_bytes)
    };

    // Extent-backed files handle sparse growth and extent-aware shrinking here.
    if fs.superblock.has_extents() && inode.have_extend_header_and_use_extend() {
        if truncate_size < old_size {
            // Delegate range removal to the extent tree so physical-block frees
            // stay consistent even when holes exist.
            let del_start_lbn = new_blocks as u32;

            loop {
                let blocks_map = resolve_inode_block_allextend(fs, device, &mut inode)?;
                let del_len = if truncate_size == 0 {
                    blocks_map.len() as u32
                } else {
                    blocks_map.range(del_start_lbn..).count() as u32
                };

                if del_len == 0 {
                    break;
                }

                let start_lbn = if truncate_size == 0 {
                    // Start from the first mapped logical block to avoid
                    // rescanning from zero on sparse files.
                    let Some((&first_lbn, _)) = blocks_map.iter().next() else {
                        break;
                    };
                    first_lbn
                } else {
                    del_start_lbn
                };

                let chunk = core::cmp::min(del_len, 0x7FFF);
                {
                    let mut tree = ExtentTree::new(&mut inode);
                    tree.remove_extend(fs, Ext4Extent::new(start_lbn, 0, chunk as u16), device)?;
                }
            }
        }

        if new_blocks > old_blocks {
            let mut new_blocks_map: Vec<(u32, AbsoluteBN)> = Vec::new();
            for lbn in old_blocks as u32..new_blocks as u32 {
                let phys = fs.alloc_block(device)?;
                fs.datablock_cache.modify_new(device, phys, |data| {
                    for b in data.iter_mut() {
                        *b = 0;
                    }
                })?;
                new_blocks_map.push((lbn, phys));
            }

            let mut tree = ExtentTree::new(&mut inode);
            if !new_blocks_map.is_empty() {
                let mut idx = 0usize;
                while idx < new_blocks_map.len() {
                    let (start_lbn, start_phys) = new_blocks_map[idx];
                    let mut run_len: u32 = 1;
                    let mut last_lbn = start_lbn;
                    let mut last_phys = start_phys;
                    idx += 1;
                    while idx < new_blocks_map.len() {
                        let (cur_lbn, cur_phys) = new_blocks_map[idx];
                        if cur_lbn == last_lbn + 1
                            && last_phys.checked_add(1).ok() == Some(cur_phys)
                        {
                            run_len = run_len.saturating_add(1);
                            last_lbn = cur_lbn;
                            last_phys = cur_phys;
                            idx += 1;
                        } else {
                            break;
                        }
                    }
                    let ext = Ext4Extent::new(start_lbn, start_phys.raw(), run_len as u16);
                    tree.insert_extent(fs, ext, device)?;
                }
            }
        }

        inode.i_size_lo = (truncate_size & 0xffff_ffff) as u32;
        inode.i_size_high = (truncate_size >> 32) as u32;
        // i_blocks reflects number of allocated blocks, not logical length. Recompute after edits.
        let alloc_blocks = resolve_inode_block_allextend(fs, device, &mut inode)?.len() as u64;
        let iblocks_used = alloc_blocks.saturating_mul(BLOCK_SIZE as u64 / 512);
        inode.i_blocks_lo = (iblocks_used & 0xffff_ffff) as u32;
        inode.l_i_blocks_high = ((iblocks_used >> 32) & 0xffff) as u16;

        fs.finalize_inode_update(
            device,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::truncate_access(),
        )?;
        return Ok(());
    }

    // Non-extent files currently support only the 12 direct block pointers.
    if new_blocks > 12 {
        return Err(Ext4Error::unsupported());
    }

    // Grow by allocating and zeroing new direct blocks.
    if new_blocks > old_blocks {
        for lbn in old_blocks as u32..new_blocks as u32 {
            let phys = fs.alloc_block(device)?;
            fs.datablock_cache.modify_new(device, phys, |data| {
                for b in data.iter_mut() {
                    *b = 0;
                }
            })?;
            inode.i_block[lbn as usize] = phys.to_u32()?;
        }
    }

    // Shrink by freeing trailing direct blocks and clearing their pointers.
    if new_blocks < old_blocks {
        for lbn in new_blocks as u32..old_blocks as u32 {
            let phys = inode.i_block[lbn as usize];
            if phys != 0 {
                let phys = AbsoluteBN::from(phys);
                fs.free_block(device, phys)?;
            }
            inode.i_block[lbn as usize] = 0;
        }
    }

    inode.i_size_lo = (truncate_size & 0xffff_ffff) as u32;
    inode.i_size_high = (truncate_size >> 32) as u32;
    let iblocks_used = new_blocks.saturating_mul(BLOCK_SIZE as u64 / 512);
    inode.i_blocks_lo = (iblocks_used & 0xffff_ffff) as u32;
    inode.l_i_blocks_high = ((iblocks_used >> 32) & 0xffff) as u16;

    fs.finalize_inode_update(
        device,
        inode_num,
        &mut inode,
        Ext4InodeMetadataUpdate::truncate_access(),
    )?;

    Ok(())
}

fn read_symlink_target<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode: &mut Ext4Inode,
) -> Ext4Result<Vec<u8>> {
    let size = inode.size() as usize;
    if size == 0 {
        return Ok(Vec::new());
    }

    if size <= 60 {
        let mut raw = [0u8; 60];
        for (i, word) in inode.i_block.iter().take(15).enumerate() {
            raw[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        return Ok(raw[..size].to_vec());
    }

    let block_bytes = BLOCK_SIZE;
    let total_blocks = size.div_ceil(block_bytes);
    let mut buf = Vec::with_capacity(size);

    if inode.have_extend_header_and_use_extend() {
        let blocks = resolve_inode_block_allextend(fs, device, inode)?;
        for &phys in blocks.values() {
            let cached = fs.datablock_cache.get_or_load(device, phys)?;
            let data = &cached.data[..block_bytes];
            buf.extend_from_slice(data);
            if buf.len() >= size {
                break;
            }
        }
    } else {
        for lbn in 0..total_blocks {
            let phys = match resolve_inode_block(device, inode, lbn as u32)? {
                Some(b) => b,
                None => break,
            };
            let cached = fs.datablock_cache.get_or_load(device, phys)?;
            let data = &cached.data[..block_bytes];
            buf.extend_from_slice(data);
        }
    }

    buf.truncate(size);

    Ok(buf)
}

fn resolve_symlink_path(current_path: &str, target: &str) -> String {
    if target.starts_with('/') {
        return split_paren_child_and_tranlatevalid(target);
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
    split_paren_child_and_tranlatevalid(&combined)
}

fn read_file_follow<B: BlockDevice>(
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
        let target_bytes = read_symlink_target(device, fs, &mut inode)?;
        let target = match core::str::from_utf8(&target_bytes) {
            Ok(s) => s,
            Err(_) => return Err(Ext4Error::corrupted()),
        };
        let resolved = resolve_symlink_path(path, target);
        return read_file_follow(device, fs, &resolved, depth + 1);
    }

    if !inode.is_file() {
        error!("Entry:{path} not aa file");
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

    let block_bytes = BLOCK_SIZE;
    let total_blocks = size.div_ceil(block_bytes);

    let mut buf = Vec::with_capacity(size);

    if inode.have_extend_header_and_use_extend() {
        let blocks = resolve_inode_block_allextend(fs, device, &mut inode)?;
        for &phys in blocks.values() {
            let cached = fs.datablock_cache.get_or_load(device, phys)?;
            let data = &cached.data[..block_bytes];
            buf.extend_from_slice(data);
            if buf.len() >= size {
                break;
            }
        }
    } else {
        for lbn in 0..total_blocks {
            let phys = match resolve_inode_block(device, &mut inode, lbn as u32)? {
                Some(b) => b,
                None => break,
            };

            let cached = fs.datablock_cache.get_or_load(device, phys)?;
            let data = &cached.data[..block_bytes];
            buf.extend_from_slice(data);
        }
    }

    buf.truncate(size);

    fs.touch_inode_atime_if_needed(device, inode_num)?;

    Ok(buf)
}

/// Read the whole file at `path`.
pub fn read_file<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> Ext4Result<Vec<u8>> {
    read_file_follow(device, fs, path, 0)
}

pub fn write_file<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    offset: u64,
    data: &[u8],
) -> Ext4Result<()> {
    if data.is_empty() {
        return Ok(());
    }

    // Resolve the inode once before switching to the inode-based writer.
    let info = match get_inode_with_num(fs, device, path).ok().flatten() {
        Some(v) => v,
        None => return Err(Ext4Error::not_found()),
    };
    let (inode_num, _inode) = info;

    write_inode_data(device, fs, inode_num, offset, data)
}

fn write_inode_data<B: BlockDevice>(
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
    let block_bytes = BLOCK_SIZE as u64;

    // Some older or partially initialized inodes may carry the extents flag
    // without a valid embedded header. Repair that before extent operations.
    if fs.superblock.has_extents() && !inode.have_extend_header_and_use_extend() {
        inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
        inode.write_extend_header();
    }

    if offset > old_size {
        info!("Expend write!");
    }

    let end = offset.saturating_add(data.len() as u64);

    let start_lbn = offset / block_bytes;
    let end_lbn = (end - 1) / block_bytes;

    // Non-extent files cannot grow through sparse writes in this implementation.
    if end > old_size
        && (!fs.superblock.has_extents() || !inode.have_extend_header_and_use_extend())
    {
        return Err(Ext4Error::unsupported());
    }

    for lbn in start_lbn..=end_lbn {
        let phys = if inode.have_extend_header_and_use_extend() {
            match resolve_inode_block(device, &mut inode, lbn as u32)? {
                Some(b) => b,
                None => {
                    let new_phys = fs.alloc_block(device)?;
                    fs.datablock_cache.modify_new(device, new_phys, |blk| {
                        for b in blk.iter_mut() {
                            *b = 0;
                        }
                    })?;
                    {
                        let mut tree = ExtentTree::new(&mut inode);
                        let ext = Ext4Extent::new(lbn as u32, new_phys.raw(), 1);
                        tree.insert_extent(fs, ext, device)?;
                    }

                    let add_iblocks = (BLOCK_SIZE / 512) as u32;
                    inode.i_blocks_lo = inode.i_blocks_lo.saturating_add(add_iblocks);
                    inode.l_i_blocks_high = inode
                        .l_i_blocks_high
                        .saturating_add(((add_iblocks as u64) >> 32) as u16);

                    new_phys
                }
            }
        } else {
            match resolve_inode_block(device, &mut inode, lbn as u32)? {
                Some(b) => b,
                None => return Err(Ext4Error::unsupported()),
            }
        };

        fs.datablock_cache.modify(device, phys, |blk| {
            let block_start = lbn * block_bytes;
            let block_end = block_start + block_bytes;

            let write_start = core::cmp::max(offset, block_start);
            let write_end = core::cmp::min(end, block_end);
            if write_start >= write_end {
                return;
            }

            let src_off = (write_start - offset) as u64;
            let dst_off = (write_start - block_start) as usize;
            let len = write_end - write_start;

            blk[dst_off..dst_off + len as usize]
                .copy_from_slice(&data[src_off as usize..(src_off + len) as usize]);
        })?;
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
