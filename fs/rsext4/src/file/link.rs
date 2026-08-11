use super::*;
use crate::dir::{FileName, LinkEntryRequest, insert_dir_entry_raw};

fn directory_entry_type(inode: &Ext4Inode) -> Ext4Result<u8> {
    match inode.i_mode & Ext4Inode::S_IFMT {
        Ext4Inode::S_IFREG => Ok(Ext4DirEntry2::EXT4_FT_REG_FILE),
        Ext4Inode::S_IFLNK => Ok(Ext4DirEntry2::EXT4_FT_SYMLINK),
        Ext4Inode::S_IFBLK => Ok(Ext4DirEntry2::EXT4_FT_BLKDEV),
        Ext4Inode::S_IFCHR => Ok(Ext4DirEntry2::EXT4_FT_CHRDEV),
        Ext4Inode::S_IFIFO => Ok(Ext4DirEntry2::EXT4_FT_FIFO),
        Ext4Inode::S_IFSOCK => Ok(Ext4DirEntry2::EXT4_FT_SOCK),
        Ext4Inode::S_IFDIR => Err(Ext4Error::permission_denied()),
        _ => Err(Ext4Error::corrupted().with_operation("link:inode_type")),
    }
}

/// Creates a hard link below an already resolved parent directory.
pub(crate) fn link_inode_at<B: BlockIo>(
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
    let file_type = directory_entry_type(&target_inode)?;
    let old_links = target_inode.i_links_count;
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

    fs.set_inode_links_count(block_dev, request.target, new_links)?;
    if let Err(insert_error) = insert_dir_entry_raw(
        fs,
        block_dev,
        request.parent,
        &mut parent_inode,
        request.target,
        request.name,
        file_type,
    ) {
        let lookup = find_named_entry_in_parent(
            fs,
            block_dev,
            request.parent,
            &parent_inode,
            request.name.as_bytes(),
        );
        match lookup {
            Ok(entry) if entry.ino == request.target => return Err(insert_error),
            Err(error) if error.kind() != Ext4ErrorKind::NotFound => return Err(insert_error),
            _ => {}
        }
        let rollback = fs
            .set_inode_links_count(block_dev, request.target, old_links)
            .map(|_| ());
        return Err(error_after_cleanup(insert_error, rollback));
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
