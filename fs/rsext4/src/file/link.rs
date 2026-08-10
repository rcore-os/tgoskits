use super::{delete::remove_inodeentry_from_parentdir, *};

/// Create a hard link.
pub fn link<B: BlockIo + crate::runtime::Clock>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    link_path: &str,
    linked_path: &str,
) -> Ext4Result<()> {
    let link_norm = normalize_path(link_path);
    let linked_norm = normalize_path(linked_path);

    // Resolve the target inode first.
    let (target_ino, target_inode) = match get_file_inode(fs, block_dev, &linked_norm) {
        Ok(Some(v)) => v,
        Ok(None) => return Err(Ext4Error::not_found()),
        Err(e) => return Err(e),
    };

    // Hard-linking directories is rejected.
    if target_inode.is_dir() {
        return Err(Ext4Error::permission_denied());
    }
    let new_links = target_inode.incremented_links_count(false)?;

    // Destination entry must not already exist.
    if get_file_inode(fs, block_dev, &link_norm)?.is_some() {
        return Err(Ext4Error::already_exists());
    }

    // The destination parent directory must exist and be a directory.
    let (parent_path, child_name) = if let Some(pos) = link_norm.rfind('/') {
        let parent = if pos == 0 {
            "/".to_string()
        } else {
            link_norm[..pos].to_string()
        };
        let child = link_norm[pos + 1..].to_string();
        (parent, child)
    } else {
        ("/".to_string(), link_norm)
    };
    let (parent_ino, mut parent_inode) = match get_inode_with_num(fs, block_dev, &parent_path)? {
        Some(v) => v,
        None => return Err(Ext4Error::not_found()),
    };
    if !parent_inode.is_dir() {
        return Err(Ext4Error::not_dir());
    }

    // Reuse the source entry's file type when possible so the new directory
    // entry matches existing metadata.
    let (linked_parent_path, linked_child_name) = if let Some(pos) = linked_norm.rfind('/') {
        let parent = if pos == 0 {
            "/".to_string()
        } else {
            linked_norm[..pos].to_string()
        };
        let child = linked_norm[pos + 1..].to_string();
        (parent, child)
    } else {
        ("/".to_string(), linked_norm.clone())
    };

    let mut copied_ft: Option<u8> = None;
    if let Some((lpino, mut lp_inode)) = get_inode_with_num(fs, block_dev, &linked_parent_path)? {
        let blocks = resolve_inode_blocks(fs, block_dev, lpino, &mut lp_inode)?;
        for &phys in blocks.values() {
            let cached = fs.datablock_cache.get_or_load(block_dev, phys)?;
            let data = &cached.data;
            let iter = DirEntryIterator::new(data);
            for (entry, _) in iter {
                if entry.inode == 0 {
                    continue;
                }
                if entry.name == linked_child_name.as_bytes() {
                    copied_ft = Some(entry.file_type);
                    break;
                }
            }
            if copied_ft.is_some() {
                break;
            }
        }
    }

    let file_type = copied_ft.unwrap_or_else(|| {
        if target_inode.is_file() {
            Ext4DirEntry2::EXT4_FT_REG_FILE
        } else if target_inode.is_symlink() {
            Ext4DirEntry2::EXT4_FT_SYMLINK
        } else {
            Ext4DirEntry2::EXT4_FT_UNKNOWN
        }
    });

    // `insert_dir_entry` recalculates name length and record length for the new
    // entry automatically.
    insert_dir_entry(
        fs,
        block_dev,
        parent_ino,
        &mut parent_inode,
        target_ino,
        &child_name,
        file_type,
    )?;

    // Update the target link count and roll back the inserted entry on failure.
    if let Err(error) = fs.set_inode_links_count(block_dev, target_ino, new_links) {
        let _ = remove_inodeentry_from_parentdir(fs, block_dev, &parent_path, &child_name);
        return Err(error);
    }

    Ok(())
}
