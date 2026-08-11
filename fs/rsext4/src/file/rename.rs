use super::{
    delete::{
        delete_dir, delete_file, find_named_entry_in_parent, is_dir_empty,
        remove_inodeentry_from_parentdir,
    },
    *,
};

// TODO: RENAME_EXCHANGE — atomic swap of src and dst
// TODO: RENAME_NOREPLACE — EEXIST if dst exists

/// Renames or replaces a file-system entry.
///
/// When the destination already exists, POSIX requires type cross-checks:
/// - rename(file, dir)  → ENOTDIR
/// - rename(dir, file)  → EISDIR
/// - rename(dir, dir)   → ENOTEMPTY if dst is non-empty
/// - rename(file, file) → overwrite
pub fn rename<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    old_path: &str,
    new_path: &str,
) -> Ext4Result<()> {
    let old_norm = normalize_path(old_path);
    let new_norm = normalize_path(new_path);

    // Resolve source type for cross-type checks.
    let src_is_dir = get_inode_with_num(fs, device, &old_norm)?.is_some_and(|(_, i)| i.is_dir());

    // Replace existing destination entries before moving the source entry.
    if let Some((dst_ino, dst_inode)) = get_inode_with_num(fs, device, &new_norm)? {
        if dst_inode.is_dir() {
            if !src_is_dir {
                // rename file → dir: not allowed
                return Err(Ext4Error::not_dir());
            }
            // rename dir → dir: destination must be empty
            let mut dir_inode = dst_inode; // Ext4Inode is Copy
            if !is_dir_empty(fs, device, dst_ino, &mut dir_inode)? {
                return Err(Ext4Error::not_empty());
            }
            delete_dir(fs, device, new_path)?;
        } else {
            // dst is a file
            if src_is_dir {
                // rename dir → file: not allowed
                return Err(Ext4Error::is_dir());
            }
            delete_file(fs, device, new_path)?;
        }
    }
    // The destination must be gone before the move starts.
    if get_inode_with_num(fs, device, &new_norm)?.is_some() {
        return Err(Ext4Error::corrupted());
    }

    mv(fs, device, &old_norm, &new_norm)?;

    // Verify that the source disappeared and the destination now resolves.
    if get_inode_with_num(fs, device, &old_norm)?.is_some() {
        return Err(Ext4Error::corrupted());
    }
    if get_inode_with_num(fs, device, &new_norm)?.is_none() {
        return Err(Ext4Error::corrupted());
    }

    Ok(())
}

pub fn mv<B: BlockIo>(
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

    let old_norm = normalize_path(old_path);
    let new_norm = normalize_path(new_path);

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
            return Err(Ext4Error::invalid_input());
        }
    };

    // Resolve the source entry and preserve its inode number plus file type.
    let (old_pino, old_parent_inode) =
        get_inode_with_num(fs, block_dev, &old_parent)?.ok_or_else(Ext4Error::not_found)?;

    let old_entry = find_named_entry_in_parent(
        fs,
        block_dev,
        old_pino,
        &old_parent_inode,
        old_name.as_bytes(),
    )?;
    let src_ino = old_entry.ino;
    let src_ft = old_entry.file_type;
    let mut moved_inode = fs.get_inode_by_num(block_dev, src_ino)?;

    // Destination parent directory must exist and be a directory.
    let (new_pino, new_parent_inode) =
        get_inode_with_num(fs, block_dev, &new_parent)?.ok_or_else(Ext4Error::not_found)?;
    if !new_parent_inode.is_dir() {
        return Err(Ext4Error::invalid_input());
    }

    // Destination must not already exist at this point.
    if get_inode_with_num(fs, block_dev, &new_norm)?.is_some() {
        return Err(Ext4Error::already_exists());
    }

    // The root directory itself cannot be moved.
    if old_norm == "/" {
        return Err(Ext4Error::invalid_input());
    }

    let cross_parent_directory_links = if moved_inode.is_dir() && old_pino != new_pino {
        let dir_nlink_feature = fs
            .superblock
            .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_DIR_NLINK);
        Some((
            old_parent_inode.decremented_links_count()?,
            new_parent_inode.incremented_links_count(dir_nlink_feature)?,
        ))
    } else {
        None
    };

    // Publish the source inode under its new parent/name first.
    let mut new_parent_inode_copy = new_parent_inode;
    insert_dir_entry(
        fs,
        block_dev,
        new_pino,
        &mut new_parent_inode_copy,
        src_ino,
        &new_name,
        src_ft,
    )?;

    // Remove the old entry, rolling back the new one if that fails.
    if let Err(error) = remove_inodeentry_from_parentdir(fs, block_dev, &old_parent, &old_name) {
        let _ = remove_inodeentry_from_parentdir(fs, block_dev, &new_parent, &new_name);
        return Err(error);
    }

    // Directory moves across parents must fix both parents' link counts and the
    // moved directory's `..` entry.
    if moved_inode.is_dir() {
        // Only cross-parent moves need link-count and `..` adjustments.
        if let Some((old_links, new_links)) = cross_parent_directory_links {
            fs.set_inode_links_count(block_dev, old_pino, old_links)?;
            fs.set_inode_links_count(block_dev, new_pino, new_links)?;

            // Rewrite the `..` entry inside the moved directory's first block.
            let first_blk = resolve_inode_block(fs, block_dev, src_ino, &mut moved_inode, 0)?
                .ok_or_else(Ext4Error::corrupted)?;
            let mut valid_parent_entry = true;
            fs.datablock_cache.modify(block_dev, first_blk, |data| {
                let block_bytes = data.len();
                if block_bytes < 24 {
                    valid_parent_entry = false;
                    return;
                }
                // '.' entry at offset 0
                let rec_len0 = u16::from_le_bytes([data[4], data[5]]) as usize;
                if rec_len0 == 0 || rec_len0 + 8 > block_bytes {
                    valid_parent_entry = false;
                    return;
                }
                let off1 = rec_len0;
                if off1 + 4 > block_bytes {
                    valid_parent_entry = false;
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
            })?;
            if !valid_parent_entry {
                return Err(Ext4Error::corrupted());
            }
            fs.touch_inode_ctime_for_link_change(block_dev, src_ino)?;
        }
    }

    Ok(())
}
