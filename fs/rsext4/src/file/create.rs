use super::*;

fn discard_unpublished_inode_blocks<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    data_blocks: &[AbsoluteBN],
) -> Ext4Result<()> {
    let mut first_error = None;
    for &blk in data_blocks {
        fs.datablock_cache.invalidate(blk);
        if let Err(error) = fs.free_block(device, blk)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error.with_operation("rollback:file_blocks")),
        None => Ok(()),
    }
}

fn discard_unpublished_inode<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    inode_num: InodeNumber,
    data_blocks: &[AbsoluteBN],
) -> Ext4Result<()> {
    let block_result = discard_unpublished_inode_blocks(fs, device, data_blocks);
    let inode_result = fs.free_inode(device, inode_num);
    match (block_result, inode_result) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error.with_operation("rollback:file_inode")),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn error_after_cleanup(operation_error: Ext4Error, cleanup: Ext4Result<()>) -> Ext4Error {
    match cleanup {
        Ok(()) => operation_error,
        Err(cleanup_error) => cleanup_error,
    }
}

/// Create a symbolic link (root-owned).
pub fn create_symbol_link<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    src_path: &str,
    dst_path: &str,
) -> Ext4Result<()> {
    create_symbol_link_with_owner(device, fs, src_path, dst_path, 0, 0)
}

/// Create a symbolic link with explicit uid/gid ownership.
pub fn create_symbol_link_with_owner<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    src_path: &str,
    dst_path: &str,
    uid: u32,
    gid: u32,
) -> Ext4Result<()> {
    // Validate the source and destination before allocating the new symlink.
    let src_norm = normalize_path(src_path);
    let dst_norm = normalize_path(dst_path);

    if get_file_inode(fs, device, &src_norm)?.is_none() {
        return Err(Ext4Error::invalid_input());
    }
    if get_file_inode(fs, device, &dst_norm)?.is_some() {
        return Err(Ext4Error::invalid_input());
    }

    // Split the destination into parent directory and entry name.
    let (parent, child) = if let Some(pos) = dst_norm.rfind('/') {
        let p = if pos == 0 {
            "/".to_string()
        } else {
            dst_norm[..pos].to_string()
        };
        let c = dst_norm[pos + 1..].to_string();
        (p, c)
    } else {
        ("/".to_string(), dst_norm)
    };

    let (parent_ino_num, parent_inode) = match get_inode_with_num(fs, device, &parent)? {
        Some(v) => v,
        None => return Err(Ext4Error::invalid_input()),
    };
    if !parent_inode.is_dir() {
        return Err(Ext4Error::invalid_input());
    }

    // Allocate and populate the new symlink inode.
    let new_ino = fs.alloc_inode(device)?;

    let target_bytes = src_path.as_bytes();
    let target_len = target_bytes.len();
    let block_size = fs.block_size();
    let size_lo = (target_len as u64 & 0xffffffff) as u32;
    let size_hi = ((target_len as u64) >> 32) as u32;
    let symlink_mode = Ext4Inode::S_IFLNK | 0o777;

    let mut new_inode = Ext4Inode::empty_for_reuse(fs.default_inode_extra_isize());
    new_inode.i_links_count = 1;
    new_inode.i_size_lo = size_lo;
    new_inode.i_size_high = size_hi;
    new_inode.i_flags = Ext4Inode::mask_flags_for_mode(
        symlink_mode,
        parent_inode.i_flags & Ext4Inode::EXT4_FL_INHERITED,
    );
    let mut data_blocks: Vec<AbsoluteBN> = Vec::new();

    if target_len == 0 {
        new_inode.i_blocks_lo = 0;
        new_inode.l_i_blocks_high = 0;
        new_inode.i_block = [0; 15];
    } else if target_len < 60 {
        // Fast symlink: store the target directly inside `i_block`.
        let mut raw = [0u8; 60];
        raw[..target_len].copy_from_slice(target_bytes);
        for i in 0..15 {
            new_inode.i_block[i] =
                u32::from_le_bytes([raw[i * 4], raw[i * 4 + 1], raw[i * 4 + 2], raw[i * 4 + 3]]);
        }
        new_inode.i_blocks_lo = 0;
        new_inode.l_i_blocks_high = 0;
    } else {
        // Long symlink: spill the target path into data blocks.
        let mut remaining = target_len;
        let mut src_off = 0usize;

        while remaining > 0 {
            if !fs.superblock.has_extents() && data_blocks.len() >= 12 {
                let error = error_after_cleanup(
                    Ext4Error::unsupported(),
                    discard_unpublished_inode(fs, device, new_ino, &data_blocks),
                );
                return Err(error);
            }

            let blk = match fs.alloc_block(device) {
                Ok(block) => block,
                Err(error) => {
                    let error = error_after_cleanup(
                        error,
                        discard_unpublished_inode(fs, device, new_ino, &data_blocks),
                    );
                    return Err(error);
                }
            };
            let write_len = core::cmp::min(remaining, block_size);
            if let Err(e) = fs.datablock_cache.modify_new(device, blk, |data| {
                for b in data.iter_mut() {
                    *b = 0;
                }
                let end = src_off + write_len;
                data[..write_len].copy_from_slice(&target_bytes[src_off..end]);
            }) {
                fs.datablock_cache.invalidate(blk);
                data_blocks.push(blk);
                let error = error_after_cleanup(
                    e,
                    discard_unpublished_inode(fs, device, new_ino, &data_blocks),
                );
                return Err(error);
            }

            data_blocks.push(blk);
            remaining -= write_len;
            src_off += write_len;
        }

        let used_datablocks = data_blocks.len() as u64;
        let iblocks_used = used_datablocks
            .checked_mul(block_size as u64 / 512)
            .ok_or_else(Ext4Error::overflow);
        let huge_file_feature = fs
            .superblock
            .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
        if let Err(error) = iblocks_used.and_then(|sectors| {
            new_inode.set_blocks_count(sectors, block_size as u32, huge_file_feature)
        }) {
            let error = error_after_cleanup(
                error,
                discard_unpublished_inode(fs, device, new_ino, &data_blocks),
            );
            return Err(error);
        }

        if let Err(error) = build_file_block_mapping_with_inode_num(
            fs,
            &mut new_inode,
            new_ino,
            &data_blocks,
            device,
        ) {
            let error = error_after_cleanup(
                error,
                discard_unpublished_inode(fs, device, new_ino, &data_blocks),
            );
            return Err(error);
        }
    }

    let mut create_update = Ext4InodeMetadataUpdate::create(symlink_mode);
    create_update.uid = Some(uid);
    create_update.gid = Some(gid);
    if fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_PROJECT)
        && parent_inode.i_flags & Ext4Inode::EXT4_PROJINHERIT_FL != 0
    {
        create_update.projid = Some(parent_inode.i_projid);
    }

    if let Err(error) = fs.finalize_inode_update(device, new_ino, &mut new_inode, create_update) {
        let error = error_after_cleanup(
            error,
            discard_unpublished_inode(fs, device, new_ino, &data_blocks),
        );
        return Err(error);
    }

    // Publish the new symlink by inserting its directory entry.
    let mut parent_inode_copy = parent_inode;
    insert_dir_entry(
        fs,
        device,
        parent_ino_num,
        &mut parent_inode_copy,
        new_ino,
        &child,
        Ext4DirEntry2::EXT4_FT_SYMLINK,
    )?;

    Ok(())
}

/// Create a file entry, creating missing parent directories on demand (root-owned).
pub fn mkfile<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    initial_data: Option<&[u8]>,
    file_type: Option<u8>,
) -> Ext4Result<Ext4Inode> {
    mkfile_with_owner(device, fs, path, initial_data, file_type, 0, 0)
}

/// Create a file entry with explicit uid/gid ownership.
pub fn mkfile_with_owner<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    initial_data: Option<&[u8]>,
    file_type: Option<u8>,
    uid: u32,
    gid: u32,
) -> Ext4Result<Ext4Inode> {
    // Normalize first so all later path splitting uses one canonical form.
    let norm_path = normalize_path(path);
    if norm_path.is_empty() || norm_path == "/" {
        return Err(Ext4Error::invalid_input());
    }

    // Refuse to overwrite an existing entry.
    if get_file_inode(fs, device, &norm_path)?.is_some() {
        return Err(Ext4Error::already_exists());
    }

    // Split the normalized path into parent directory and leaf name.
    let mut valid_path = norm_path;
    let split_point = match valid_path.rfind('/') {
        Some(v) => v,
        None => return Err(Ext4Error::invalid_input()),
    };
    let child = valid_path.split_off(split_point)[1..].to_string();
    if child.is_empty() {
        return Err(Ext4Error::invalid_input());
    }
    let parent = if valid_path.is_empty() {
        "/".to_string()
    } else {
        valid_path
    };

    // Create missing parent directories before allocating the file inode.
    ensure_directory(device, fs, &parent, uid, gid)?;

    // Reload the parent inode after directory creation so we use the final
    // parent metadata and inode number.
    let (parent_ino_num, parent_inode) =
        get_inode_with_num(fs, device, &parent)?.ok_or(Ext4Error::not_found())?;

    // Allocate the inode before writing any initial data blocks.
    let new_file_ino = fs.alloc_inode(device)?;

    // Materialize the initial file payload block by block.
    let block_size = fs.block_size();
    let mut data_blocks: Vec<AbsoluteBN> = Vec::new();
    let mut total_written: usize = 0;
    if let Some(buf) = initial_data {
        let mut remaining = buf.len();
        let mut src_off = 0usize;

        while remaining > 0 {
            // Non-extent files only support the 12 direct pointers here.
            if !fs.superblock.has_extents() && data_blocks.len() >= 12 {
                let error = error_after_cleanup(
                    Ext4Error::unsupported(),
                    discard_unpublished_inode(fs, device, new_file_ino, &data_blocks),
                );
                return Err(error);
            }

            let blk = match fs.alloc_block(device) {
                Ok(b) => b,
                Err(e) => {
                    let error = error_after_cleanup(
                        e,
                        discard_unpublished_inode(fs, device, new_file_ino, &data_blocks),
                    );
                    return Err(error);
                }
            };

            let write_len = core::cmp::min(remaining, block_size);

            // Zero-fill each new block and copy the live payload prefix into it.
            if let Err(e) = fs.datablock_cache.modify_new(device, blk, |data| {
                for b in data.iter_mut() {
                    *b = 0;
                }
                let end = src_off + write_len;
                data[..write_len].copy_from_slice(&buf[src_off..end]);
            }) {
                fs.datablock_cache.invalidate(blk);
                data_blocks.push(blk);
                let error = error_after_cleanup(
                    e,
                    discard_unpublished_inode(fs, device, new_file_ino, &data_blocks),
                );
                return Err(error);
            }

            data_blocks.push(blk);
            total_written += write_len;
            remaining -= write_len;
            src_off += write_len;
        }
    }

    // Build the final inode image in memory, then persist it through the
    // unified metadata finalization path.
    let mut new_inode = Ext4Inode::empty_for_reuse(fs.default_inode_extra_isize());
    let imode = if let Some(ft) = file_type {
        match ft {
            Ext4DirEntry2::EXT4_FT_SYMLINK => Ext4Inode::S_IFLNK | 0o777,
            Ext4DirEntry2::EXT4_FT_REG_FILE => Ext4Inode::S_IFREG | 0o644,
            Ext4DirEntry2::EXT4_FT_DIR => Ext4Inode::S_IFDIR | 0o755,
            Ext4DirEntry2::EXT4_FT_BLKDEV => Ext4Inode::S_IFBLK | 0o600,
            Ext4DirEntry2::EXT4_FT_CHRDEV => Ext4Inode::S_IFCHR | 0o600,
            Ext4DirEntry2::EXT4_FT_FIFO => Ext4Inode::S_IFIFO | 0o644,
            Ext4DirEntry2::EXT4_FT_SOCK => Ext4Inode::S_IFSOCK | 0o644,
            _ => Ext4Inode::S_IFREG | 0o644,
        }
    } else {
        Ext4Inode::S_IFREG | 0o644
    };

    new_inode.i_flags =
        Ext4Inode::mask_flags_for_mode(imode, parent_inode.i_flags & Ext4Inode::EXT4_FL_INHERITED);

    // Extent-enabled files start with an embedded extent root.
    if fs.superblock.has_extents() {
        new_inode.write_extend_header();
    }

    new_inode.i_links_count = 1;

    let size_lo = (total_written & 0xffffffff) as u32;
    let size_hi = ((total_written as u64) >> 32) as u32;

    if !data_blocks.is_empty() {
        // File starts with allocated data blocks.
        let used_databyte = data_blocks.len() as u64;
        let iblocks_used = used_databyte
            .checked_mul(block_size as u64 / 512)
            .ok_or_else(Ext4Error::overflow);
        new_inode.i_size_lo = size_lo;
        new_inode.i_size_high = size_hi;
        let huge_file_feature = fs
            .superblock
            .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
        if let Err(error) = iblocks_used.and_then(|sectors| {
            new_inode.set_blocks_count(sectors, block_size as u32, huge_file_feature)
        }) {
            let error = error_after_cleanup(
                error,
                discard_unpublished_inode(fs, device, new_file_ino, &data_blocks),
            );
            return Err(error);
        }

        if let Err(error) = build_file_block_mapping_with_inode_num(
            fs,
            &mut new_inode,
            new_file_ino,
            &data_blocks,
            device,
        ) {
            let error = error_after_cleanup(
                error,
                discard_unpublished_inode(fs, device, new_file_ino, &data_blocks),
            );
            return Err(error);
        }
    } else {
        // Empty file starts with no data blocks.
        new_inode.i_size_lo = 0;
        new_inode.i_size_high = 0;
        new_inode.i_blocks_lo = 0;
        new_inode.l_i_blocks_high = 0;
        if fs.superblock.has_extents() {
            new_inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
            new_inode.write_extend_header();
        } else {
            new_inode.i_block = [0; 15];
        }
    }

    let mut create_update = Ext4InodeMetadataUpdate::create(imode);
    create_update.uid = Some(uid);
    create_update.gid = Some(gid);
    if fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_PROJECT)
        && parent_inode.i_flags & Ext4Inode::EXT4_PROJINHERIT_FL != 0
    {
        create_update.projid = Some(parent_inode.i_projid);
    }

    if let Err(error) =
        fs.finalize_inode_update(device, new_file_ino, &mut new_inode, create_update)
    {
        let error = error_after_cleanup(
            error,
            discard_unpublished_inode(fs, device, new_file_ino, &data_blocks),
        );
        return Err(error);
    }

    // Finally publish the file by linking it into the parent directory.
    let file_type = match file_type {
        Some(ft) => ft,
        None => Ext4DirEntry2::EXT4_FT_REG_FILE,
    };

    let mut parent_inode_copy = parent_inode;
    insert_dir_entry(
        fs,
        device,
        parent_ino_num,
        &mut parent_inode_copy,
        new_file_ino,
        &child,
        file_type,
    )?;

    fs.get_inode_by_num(device, new_file_ino)
}
