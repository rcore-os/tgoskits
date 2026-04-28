use super::*;

fn free_inode_with_dtime<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
) -> Ext4Result<()> {
    let mut used_blocks: Vec<AbsoluteBN> = resolve_inode_block_allextend(fs, block_dev, inode)?
        .into_values()
        .collect();
    used_blocks.sort_unstable();

    let _ = fs.apply_inode_dtime(block_dev, inode_num, Ext4DtimeUpdate::SetNow)?;

    for blk in used_blocks {
        fs.free_block(block_dev, blk)?;
    }

    fs.free_inode(block_dev, inode_num)
}

/// Remove a non-directory link from its parent directory.
pub fn unlink<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    link_path: &str,
) -> Ext4Result<()> {
    // Resolve the parent directory and target entry before mutating link
    // counts or directory contents.
    let norm_path = split_paren_child_and_tranlatevalid(link_path);
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

    let (_pino, mut parent_inode) = match get_inode_with_num(fs, block_dev, &parent_path)
        .ok()
        .flatten()
    {
        Some(v) => v,
        None => return Err(Ext4Error::not_found()),
    };

    let mut target_ino: Option<InodeNumber> = None;
    let blocks = resolve_inode_block_allextend(fs, block_dev, &mut parent_inode)?;

    for &phys in blocks.values() {
        let cached = match fs.datablock_cache.get_or_load(block_dev, phys) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let data = &cached.data[..BLOCK_SIZE];
        let iter = DirEntryIterator::new(data);
        for (entry, _) in iter {
            if entry.inode == 0 {
                continue;
            }
            if entry.name == child_name.as_bytes() {
                target_ino =
                    Some(InodeNumber::new(entry.inode).map_err(|_| Ext4Error::corrupted())?);
                break;
            }
        }
        if target_ino.is_some() {
            break;
        }
    }

    let target_ino = match target_ino {
        Some(v) => v,
        None => return Err(Ext4Error::not_found()),
    };

    let mut target_inode = fs.get_inode_by_num(block_dev, target_ino)?;
    if target_inode.is_dir() {
        return Err(Ext4Error::is_dir());
    }

    // Drop the link count on the target inode first.
    let new_links = target_inode.i_links_count.saturating_sub(1);
    fs.set_inode_links_count(block_dev, target_ino, new_links)?;

    // When the final link disappears, free blocks and inode through the shared
    // deletion path.
    if new_links == 0 {
        free_inode_with_dtime(fs, block_dev, target_ino, &mut target_inode)?;
    }

    // Remove the directory entry only after inode state is updated.
    remove_inodeentry_from_parentdir(fs, block_dev, &parent_path, &child_name)?;
    Ok(())
}

pub fn remove_inodeentry_from_parentdir<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    parent_path: &str,
    child_name: &str,
) -> Ext4Result<()> {
    let parent_info = match get_inode_with_num(fs, block_dev, parent_path)
        .ok()
        .flatten()
    {
        Some(v) => v,
        None => return Err(Ext4Error::not_found()),
    };
    let (parent_ino_num, mut parent_inode) = parent_info;
    if !parent_inode.is_dir() {
        return Err(Ext4Error::not_dir());
    }

    let total_size = parent_inode.size() as usize;
    let block_bytes = BLOCK_SIZE;
    let total_blocks = if total_size == 0 {
        0
    } else {
        total_size.div_ceil(block_bytes)
    };

    let mut removed = false;
    let name_bytes = child_name.as_bytes();

    for lbn in 0..total_blocks {
        if removed {
            break;
        }
        let phys = match resolve_inode_block(block_dev, &mut parent_inode, lbn as u32) {
            Ok(Some(b)) => b,
            _ => continue,
        };
        let _ = fs.datablock_cache.modify(block_dev, phys, |data| {
            if removed {
                return;
            }
            let mut offset: usize = 0;
            let mut prev_off: Option<usize> = None;
            let mut prev_rec_len: u16 = 0;
            while offset + 8 <= block_bytes {
                let inode = u32::from_le_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]);
                let rec_len = u16::from_le_bytes([data[offset + 4], data[offset + 5]]);
                if rec_len < 8 {
                    break;
                }
                let name_len = data[offset + 6] as usize;
                let entry_end = offset + rec_len as usize;
                if entry_end > block_bytes {
                    break;
                }

                // Only compare the name inside this entry's recorded `rec_len`
                // so malformed trailing bytes do not leak into the comparison.
                if name_len > 0 && offset + 8 + name_len <= entry_end {
                    let name = &data[offset + 8..offset + 8 + name_len];
                    if inode != 0 && name == name_bytes {
                        if let Some(poff) = prev_off {
                            // Merge current entry's space into previous entry.
                            let new_len = prev_rec_len.saturating_add(rec_len);
                            let bytes = new_len.to_le_bytes();
                            data[poff + 4] = bytes[0];
                            data[poff + 5] = bytes[1];

                            // Clear current entry inode so it will be treated as free.
                            let zero = 0u32.to_le_bytes();
                            data[offset] = zero[0];
                            data[offset + 1] = zero[1];
                            data[offset + 2] = zero[2];
                            data[offset + 3] = zero[3];
                        } else {
                            // No previous entry in this block: mark this entry free.
                            let zero = 0u32.to_le_bytes();
                            data[offset] = zero[0];
                            data[offset + 1] = zero[1];
                            data[offset + 2] = zero[2];
                            data[offset + 3] = zero[3];
                        }
                        removed = true;
                        update_ext4_dirblock_csum32(
                            &fs.superblock,
                            parent_ino_num.raw(),
                            parent_inode.i_generation,
                            data,
                        );
                        break;
                    }
                }
                if entry_end >= block_bytes {
                    break;
                }
                prev_off = Some(offset);
                prev_rec_len = rec_len;
                offset = entry_end;
            }
        });
    }

    if removed {
        fs.touch_parent_dir_for_entry_change(block_dev, parent_ino_num)?;
        return Ok(());
    }

    Err(Ext4Error::not_found())
}

/// Remove a directory tree.
pub fn delete_dir<B: BlockDevice>(
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

    let norm_path = split_paren_child_and_tranlatevalid(path);
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
            let block_bytes = BLOCK_SIZE;

            let dir_blocks = resolve_inode_block_allextend(fs, block_dev, &mut frame.inode)?;

            let mut to_descend: Vec<(
                alloc::string::String,
                InodeNumber,
                Ext4Inode,
                alloc::string::String,
            )> = Vec::new();
            let mut removed_child_dirs: u16 = 0;

            for &phys in dir_blocks.values() {
                // Collect child entries first to avoid nested mutable borrows of
                // `fs` while the data-block cache entry is live.
                let mut child_entries: Vec<(InodeNumber, alloc::string::String)> = Vec::new();
                {
                    let cached = fs.datablock_cache.get_or_load(block_dev, phys)?;
                    let data = &cached.data[..block_bytes];
                    let iter = DirEntryIterator::new(data);
                    for (entry, _) in iter {
                        if entry.is_dot() || entry.is_dotdot() {
                            continue;
                        }
                        let child_name_bytes = entry.name.to_vec();
                        let child_name_str = match core::str::from_utf8(&child_name_bytes) {
                            Ok(s) => s,
                            Err(_) => {
                                warn!("invalid child name utf8 under dir {}", frame.path);
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

                    debug!("scan entry path={child_path}");

                    let child_inode = fs.get_inode_by_num(block_dev, child_ino)?;

                    // Delete non-directory children immediately. Directories are
                    // deferred to the DFS stack.
                    if !child_inode.is_dir() {
                        delete_file(fs, block_dev, &child_path)?;
                        continue;
                    }

                    removed_child_dirs = removed_child_dirs.saturating_add(1);
                    to_descend.push((child_path, child_ino, child_inode, child_name));
                }
            }

            if removed_child_dirs != 0 {
                let current_inode = fs.get_inode_by_num(block_dev, frame.ino_num)?;
                let new_links = current_inode
                    .i_links_count
                    .saturating_sub(removed_child_dirs);
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

        // A fully drained directory should have exactly the `.` and `..` links
        // left. Warn if the count disagrees, but keep deleting.
        if cur_inode.i_links_count != 2 {
            warn!(
                "dir inode links_count != 2 (links={}) path={} ino={}",
                cur_inode.i_links_count, frame.path, frame.ino_num
            );
        }

        // Remove the entry from the parent directory and then fix the parent's
        // directory link count.
        if let (Some(pp), Some(name)) = (&frame.parent_path, &frame.name_in_parent) {
            let removed_path = if pp == "/" {
                alloc::format!("/{name}")
            } else {
                alloc::format!("{pp}/{name}")
            };
            debug!("delete entry path={removed_path}");

            remove_inodeentry_from_parentdir(fs, block_dev, pp, name)?;

            let (pino, parent_inode) =
                get_inode_with_num(fs, block_dev, pp)?.ok_or(Ext4Error::corrupted())?;
            let parent_new_links = parent_inode.i_links_count.saturating_sub(1);
            fs.set_inode_links_count(block_dev, pino, parent_new_links)?;
        }

        free_inode_with_dtime(fs, block_dev, frame.ino_num, &mut cur_inode)?;

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

/// Remove a non-directory inode from its parent directory.
pub fn delete_file<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    path: &str,
) -> Ext4Result<()> {
    // find inode
    let norm_path = split_paren_child_and_tranlatevalid(path);
    let target = match get_file_inode(fs, block_dev, &norm_path) {
        Ok(Some((ino_num, inode))) => (ino_num, inode),
        Ok(None) => return Err(Ext4Error::not_found()),
        Err(e) => return Err(e),
    };
    let (ino_num, mut target_inode) = target;

    if target_inode.is_dir() {
        return Err(Ext4Error::is_dir());
    }

    // Drop the file's link count before removing the parent entry.
    let new_links = target_inode.i_links_count.saturating_sub(1);
    fs.set_inode_links_count(block_dev, ino_num, new_links)?;
    if new_links == 0 {
        debug!("Will free inode:{ino_num} path:{path}");
        free_inode_with_dtime(fs, block_dev, ino_num, &mut target_inode)?;
    } else {
        error!(
            "Inode num:{} links:{} >0 ,only remove entry!",
            ino_num, new_links
        );
    }

    // Resolve the parent path and child name for the directory-entry removal.
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

    remove_inodeentry_from_parentdir(fs, block_dev, &parent_path, &child_name)?;
    Ok(())
}
