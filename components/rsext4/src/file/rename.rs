use super::{
    delete::{delete_dir, delete_file, remove_inodeentry_from_parentdir},
    *,
};

/// Renames or replaces a file-system entry.
pub fn rename<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    old_path: &str,
    new_path: &str,
) -> Ext4Result<()> {
    let old_norm = split_paren_child_and_tranlatevalid(old_path);
    let new_norm = split_paren_child_and_tranlatevalid(new_path);

    // Replace existing destination entries before moving the source entry.
    if let Some((_ino, inod)) = get_inode_with_num(fs, device, &new_norm).ok().flatten() {
        if inod.is_dir() {
            delete_dir(fs, device, new_path)?;
        } else {
            delete_file(fs, device, new_path)?;
        }
    }
    // The destination must be gone before the move starts.
    if get_inode_with_num(fs, device, &new_norm)
        .ok()
        .flatten()
        .is_some()
    {
        return Err(Ext4Error::corrupted());
    }

    mv(fs, device, &old_norm, &new_norm)?;

    // Verify that the source disappeared and the destination now resolves.
    if get_inode_with_num(fs, device, &old_norm)
        .ok()
        .flatten()
        .is_some()
    {
        return Err(Ext4Error::corrupted());
    }
    if get_inode_with_num(fs, device, &new_norm)
        .ok()
        .flatten()
        .is_none()
    {
        return Err(Ext4Error::corrupted());
    }

    Ok(())
}

pub fn mv<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    old_path: &str,
    new_path: &str,
) -> Ext4Result<()> {
    // Move flow:
    // 1. resolve the source entry,
    // 2. validate the destination parent and absence of a conflicting entry,
    // 3. insert the new entry,
    // 4. remove the old entry,
    // 5. fix directory-specific link counts and `..` when moving directories.

    let old_norm = split_paren_child_and_tranlatevalid(old_path);
    let new_norm = split_paren_child_and_tranlatevalid(new_path);

    let (old_parent, old_name) = match old_norm.rfind('/') {
        Some(pos) => {
            let parent = if pos == 0 {
                "/".to_string()
            } else {
                old_norm[..pos].to_string()
            };
            let name = old_norm[pos + 1..].to_string();
            (parent, name)
        }
        None => {
            error!("mv invalid old_path(no '/'): old_path={old_path}");
            return Err(Ext4Error::invalid_input());
        }
    };
    let (new_parent, new_name) = match new_norm.rfind('/') {
        Some(pos) => {
            let parent = if pos == 0 {
                "/".to_string()
            } else {
                new_norm[..pos].to_string()
            };
            let name = new_norm[pos + 1..].to_string();
            (parent, name)
        }
        None => {
            error!("mv invalid new_path(no '/'): new_path={new_path}");
            return Err(Ext4Error::invalid_input());
        }
    };

    // Resolve the source entry and preserve its inode number plus file type.
    let (_old_pino, mut old_parent_inode) = match get_inode_with_num(fs, block_dev, &old_parent)
        .ok()
        .flatten()
    {
        Some(v) => v,
        None => {
            error!("mv old parent not found: old_path={old_path} old_parent={old_parent}");
            return Err(Ext4Error::invalid_input());
        }
    };

    let mut src_ino: Option<InodeNumber> = None;
    let mut src_ft: Option<u8> = None;
    if let Ok(blocks) = resolve_inode_block_allextend(fs, block_dev, &mut old_parent_inode) {
        for phys in blocks {
            let cached = match fs.datablock_cache.get_or_load(block_dev, phys.1) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let data = &cached.data[..BLOCK_SIZE];
            let iter = DirEntryIterator::new(data);
            for (entry, _) in iter {
                if entry.inode == 0 {
                    continue;
                }
                if entry.name == old_name.as_bytes() {
                    src_ino =
                        Some(InodeNumber::new(entry.inode).map_err(|_| Ext4Error::corrupted())?);
                    src_ft = Some(entry.file_type);
                    break;
                }
            }
            if src_ino.is_some() {
                break;
            }
        }
    }
    if src_ino.is_none() {
        // Fallback for non-extent directories: scan block pointers directly.
        let total_size = old_parent_inode.size() as usize;
        let total_blocks = if total_size == 0 {
            0
        } else {
            total_size.div_ceil(BLOCK_SIZE)
        };
        for lbn in 0..total_blocks {
            let phys = match resolve_inode_block(block_dev, &mut old_parent_inode, lbn as u32) {
                Ok(Some(b)) => b,
                _ => continue,
            };
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
                if entry.name == old_name.as_bytes() {
                    src_ino =
                        Some(InodeNumber::new(entry.inode).map_err(|_| Ext4Error::corrupted())?);
                    src_ft = Some(entry.file_type);
                    break;
                }
            }
            if src_ino.is_some() {
                break;
            }
        }
    }
    let src_ino = match src_ino {
        Some(v) => v,
        None => {
            error!(
                "mv source entry not found in old parent: old_path={old_path} \
                 old_parent={old_parent} old_name={old_name}"
            );
            return Err(Ext4Error::invalid_input());
        }
    };
    let src_ft = src_ft.unwrap_or(Ext4DirEntry2::EXT4_FT_UNKNOWN);

    // Destination parent directory must exist and be a directory.
    let (new_pino, new_parent_inode) = match get_inode_with_num(fs, block_dev, &new_parent)
        .ok()
        .flatten()
    {
        Some(v) => v,
        None => {
            error!("mv new parent not found: new_path={new_path} new_parent={new_parent}");
            return Err(Ext4Error::invalid_input());
        }
    };
    if !new_parent_inode.is_dir() {
        error!("mv new parent is not dir: new_path={new_path} new_parent={new_parent}");
        return Err(Ext4Error::invalid_input());
    }

    // Destination must not already exist at this point.
    if get_inode_with_num(fs, block_dev, &new_norm)
        .ok()
        .flatten()
        .is_some()
    {
        error!("mv destination already exists: new_path={new_path} new_norm={new_norm}");
        return Err(Ext4Error::invalid_input());
    }

    // The root directory itself cannot be moved.
    if old_norm == "/" {
        error!("mv refuses to move root: old_path={old_path}");
        return Err(Ext4Error::invalid_input());
    }

    // Publish the source inode under its new parent/name first.
    let mut new_parent_inode_copy = new_parent_inode;
    if insert_dir_entry(
        fs,
        block_dev,
        new_pino,
        &mut new_parent_inode_copy,
        src_ino,
        &new_name,
        src_ft,
    )
    .is_err()
    {
        error!(
            "mv insert_dir_entry failed: old_path={old_path} new_path={new_path} \
             new_parent={new_parent} new_name={new_name} src_ino={src_ino}"
        );
        return Err(Ext4Error::io());
    }

    // Remove the old entry, rolling back the new one if that fails.
    if remove_inodeentry_from_parentdir(fs, block_dev, &old_parent, &old_name).is_err() {
        let _ = remove_inodeentry_from_parentdir(fs, block_dev, &new_parent, &new_name);
        error!(
            "mv remove old entry failed: old_parent={old_parent} old_name={old_name} (rollback \
             new_parent={new_parent} new_name={new_name})"
        );
        return Err(Ext4Error::corrupted());
    }

    // Directory moves across parents must fix both parents' link counts and the
    // moved directory's `..` entry.
    let mut moved_inode = match fs.get_inode_by_num(block_dev, src_ino) {
        Ok(v) => v,
        Err(e) => {
            error!("mv get_inode_by_num failed ino={src_ino} err={e:?} ({e})");
            return Err(e);
        }
    };
    if moved_inode.is_dir() {
        // Only cross-parent moves need link-count and `..` adjustments.
        let old_pino = match get_inode_with_num(fs, block_dev, &old_parent)
            .ok()
            .flatten()
        {
            Some((n, _)) => n,
            None => {
                error!("mv old parent vanished while moving dir: old_parent={old_parent}");
                return Err(Ext4Error::invalid_input());
            }
        };
        if old_pino != new_pino {
            if let Ok(old_parent_inode) = fs.get_inode_by_num(block_dev, old_pino) {
                let new_links = old_parent_inode.i_links_count.saturating_sub(1);
                let _ = fs.set_inode_links_count(block_dev, old_pino, new_links);
            }
            if let Ok(new_parent_inode) = fs.get_inode_by_num(block_dev, new_pino) {
                let new_links = new_parent_inode.i_links_count.saturating_add(1);
                let _ = fs.set_inode_links_count(block_dev, new_pino, new_links);
            }

            // Rewrite the `..` entry inside the moved directory's first block.
            let first_blk = match resolve_inode_block(block_dev, &mut moved_inode, 0) {
                Ok(Some(b)) => b,
                _ => {
                    error!("mv resolve_inode_block failed for moved dir ino={src_ino}");
                    return Err(Ext4Error::corrupted());
                }
            };
            let _ = fs.datablock_cache.modify(block_dev, first_blk, |data| {
                let block_bytes = BLOCK_SIZE;
                if block_bytes < 24 {
                    return;
                }
                // '.' entry at offset 0
                let rec_len0 = u16::from_le_bytes([data[4], data[5]]) as usize;
                if rec_len0 == 0 || rec_len0 + 8 > block_bytes {
                    return;
                }
                let off1 = rec_len0;
                if off1 + 4 > block_bytes {
                    return;
                }
                let bytes = new_pino.raw().to_le_bytes();
                data[off1] = bytes[0];
                data[off1 + 1] = bytes[1];
                data[off1 + 2] = bytes[2];
                data[off1 + 3] = bytes[3];
                update_ext4_dirblock_csum32(
                    &fs.superblock,
                    src_ino.raw(),
                    moved_inode.i_generation,
                    data,
                );
            });
            let _ = fs.touch_inode_ctime_for_link_change(block_dev, src_ino);
        }
    }

    Ok(())
}
