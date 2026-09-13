use super::*;
use crate::dir::{CreateEntryRequest, FileName, insert_dir_entry_raw};

// Linux ext4_create/ext4_mkdir/ext4_symlink reserve DATA_TRANS_BLOCKS,
// INDEX_EXTRA_TRANS_BLOCKS, and three inode-allocation buffers. Writable quota
// is not implemented, so extent filesystems reserve 24 + 12 + 3 blocks.
const CREATE_TRANSACTION_CREDITS: usize = 39;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CreateInodePayload<'a> {
    Empty,
    Data(&'a [u8]),
    Device(DeviceNumber),
}

struct CreatedInode {
    number: InodeNumber,
    inode: Ext4Inode,
    data_blocks: Vec<AbsoluteBN>,
}

pub(crate) fn discard_unpublished_inode_blocks<B: BlockIo>(
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

pub(crate) fn discard_unpublished_inode<B: BlockIo>(
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

pub(crate) fn error_after_cleanup(
    operation_error: Ext4Error,
    cleanup: Ext4Result<()>,
) -> Ext4Error {
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

    let symlink_mode = Ext4Inode::S_IFLNK | 0o777;
    create_inode_at(
        device,
        fs,
        CreateEntryRequest {
            parent: parent_ino_num,
            name: FileName::new(child.as_bytes())?,
            mode: symlink_mode,
            uid,
            gid,
        },
        CreateInodePayload::Data(src_path.as_bytes()),
        Ext4DirEntry2::EXT4_FT_SYMLINK,
    )
    .map(|_| ())
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

/// Creates one non-directory inode below an already resolved parent.
pub(crate) fn create_inode_at<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    request: CreateEntryRequest<'_>,
    payload: CreateInodePayload<'_>,
    file_type: u8,
) -> Ext4Result<Ext4Inode> {
    let counters_before = fs.group_counter_snapshot();
    fs.with_metadata_transaction(device, CREATE_TRANSACTION_CREDITS, |fs, device| {
        let created = create_inode_at_inner(device, fs, request, payload, file_type)?;

        // Ordered create writes initialized payload blocks before the metadata
        // transaction can commit their inode mapping and directory entry.
        for &block in &created.data_blocks {
            fs.datablock_cache.flush(device, block)?;
        }
        fs.inodetable_cache.flush(device, created.number)?;
        fs.inodetable_cache.flush(device, request.parent)?;
        fs.flush_changed_group_metadata(device, &counters_before)?;
        fs.sync_superblock(device)?;
        Ok(created.inode)
    })
}

fn create_inode_at_inner<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    request: CreateEntryRequest<'_>,
    payload: CreateInodePayload<'_>,
    file_type: u8,
) -> Ext4Result<CreatedInode> {
    if request.name.is_reserved() || request.mode & Ext4Inode::S_IFMT == Ext4Inode::S_IFDIR {
        return Err(Ext4Error::invalid_input());
    }
    let inode_type = request.mode & Ext4Inode::S_IFMT;
    if directory_entry_type_for_mode(request.mode) != Some(file_type) {
        return Err(Ext4Error::invalid_input().with_operation("inode:create_file_type"));
    }
    let supports_data_mapping = matches!(inode_type, Ext4Inode::S_IFREG | Ext4Inode::S_IFLNK);
    let (initial_data, device_number, fast_symlink) = match (inode_type, payload) {
        (Ext4Inode::S_IFREG, CreateInodePayload::Empty) => (None, None, None),
        (Ext4Inode::S_IFREG, CreateInodePayload::Data(data)) => (Some(data), None, None),
        (Ext4Inode::S_IFLNK, CreateInodePayload::Empty) => (None, None, Some(&[][..])),
        (Ext4Inode::S_IFLNK, CreateInodePayload::Data(data)) if data.len() < 60 => {
            (None, None, Some(data))
        }
        (Ext4Inode::S_IFLNK, CreateInodePayload::Data(data)) => (Some(data), None, None),
        (Ext4Inode::S_IFCHR | Ext4Inode::S_IFBLK, CreateInodePayload::Device(device)) => {
            (None, Some(device), None)
        }
        (Ext4Inode::S_IFIFO | Ext4Inode::S_IFSOCK, CreateInodePayload::Empty) => (None, None, None),
        _ => return Err(Ext4Error::invalid_input().with_operation("inode:create_payload")),
    };
    let uses_extent_mapping = supports_data_mapping && fast_symlink.is_none();
    let parent_inode = fs.get_inode_by_num(device, request.parent)?;
    if !parent_inode.is_dir() {
        return Err(Ext4Error::not_dir());
    }
    match find_named_entry_in_parent(
        fs,
        device,
        request.parent,
        &parent_inode,
        request.name.as_bytes(),
    ) {
        Ok(_) => return Err(Ext4Error::already_exists()),
        Err(error) if error.kind() == Ext4ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    // Allocate the inode before writing any initial data blocks.
    let new_file_ino = fs.alloc_inode(device)?;

    // Materialize the initial file payload block by block.
    let block_size = fs.block_size();
    let mut data_blocks: Vec<AbsoluteBN> = Vec::new();
    let total_written = fast_symlink
        .map(|target| target.len())
        .or_else(|| initial_data.map(|data| data.len()))
        .unwrap_or(0);
    if let Some(buf) = initial_data {
        let mut remaining = if inode_type == Ext4Inode::S_IFLNK {
            buf.len().checked_add(1).ok_or_else(Ext4Error::overflow)?
        } else {
            buf.len()
        };
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
                let copy_len = core::cmp::min(write_len, buf.len().saturating_sub(src_off));
                let end = src_off + copy_len;
                data[..copy_len].copy_from_slice(&buf[src_off..end]);
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
            remaining -= write_len;
            src_off += write_len;
        }
    }

    // Build the final inode image in memory, then persist it through the
    // unified metadata finalization path.
    let mut new_inode = Ext4Inode::empty_for_reuse(fs.default_inode_extra_isize());
    new_inode.set_mode_full(request.mode);
    new_inode.i_flags = Ext4Inode::mask_flags_for_mode(
        request.mode,
        parent_inode.i_flags & Ext4Inode::EXT4_FL_INHERITED,
    );
    if let Some(device_number) = device_number
        && let Err(error) = new_inode.set_device_number(device_number)
    {
        return Err(error_after_cleanup(
            error,
            discard_unpublished_inode(fs, device, new_file_ino, &data_blocks),
        ));
    }
    if let Some(target) = fast_symlink {
        let mut inline = [0u8; 60];
        inline[..target.len()].copy_from_slice(target);
        for (word, bytes) in new_inode.i_block.iter_mut().zip(inline.as_chunks::<4>().0) {
            *word = u32::from_le_bytes(*bytes);
        }
    }

    // Extent-enabled files start with an embedded extent root.
    if uses_extent_mapping && fs.superblock.has_extents() {
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
        new_inode.i_size_lo = size_lo;
        new_inode.i_size_high = size_hi;
        new_inode.i_blocks_lo = 0;
        new_inode.l_i_blocks_high = 0;
        if uses_extent_mapping && fs.superblock.has_extents() {
            new_inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
            new_inode.write_extend_header();
        } else if device_number.is_none() && fast_symlink.is_none() {
            new_inode.i_block = [0; 15];
        }
    }

    let mut create_update = Ext4InodeMetadataUpdate::create(request.mode);
    create_update.uid = Some(request.uid);
    create_update.gid = Some(request.gid);
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

    let mut parent_inode_copy = parent_inode;
    if let Err(error) = insert_dir_entry_raw(
        fs,
        device,
        request.parent,
        &mut parent_inode_copy,
        new_file_ino,
        request.name,
        file_type,
    ) {
        let entry_absent = matches!(
            find_named_entry_in_parent(
                fs,
                device,
                request.parent,
                &parent_inode_copy,
                request.name.as_bytes(),
            ),
            Err(lookup_error) if lookup_error.kind() == Ext4ErrorKind::NotFound
        );
        if !entry_absent {
            return Err(error);
        }
        let error = error_after_cleanup(
            error,
            discard_unpublished_inode(fs, device, new_file_ino, &data_blocks),
        );
        return Err(error);
    }

    let inode = fs.get_inode_by_num(device, new_file_ino)?;
    Ok(CreatedInode {
        number: new_file_ino,
        inode,
        data_blocks,
    })
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
    let norm_path = normalize_path(path);
    if norm_path.is_empty() || norm_path == "/" {
        return Err(Ext4Error::invalid_input());
    }
    if get_file_inode(fs, device, &norm_path)?.is_some() {
        return Err(Ext4Error::already_exists());
    }

    let mut valid_path = norm_path;
    let split_point = valid_path.rfind('/').ok_or_else(Ext4Error::invalid_input)?;
    let child = valid_path.split_off(split_point)[1..].to_string();
    let parent = if valid_path.is_empty() {
        "/".to_string()
    } else {
        valid_path
    };
    let child = FileName::new(child.as_bytes())?;

    ensure_directory(device, fs, &parent, uid, gid)?;
    let (parent_ino_num, _) =
        get_inode_with_num(fs, device, &parent)?.ok_or_else(Ext4Error::not_found)?;
    let file_type = file_type.unwrap_or(Ext4DirEntry2::EXT4_FT_REG_FILE);
    let imode = match file_type {
        Ext4DirEntry2::EXT4_FT_SYMLINK => Ext4Inode::S_IFLNK | 0o777,
        Ext4DirEntry2::EXT4_FT_REG_FILE => Ext4Inode::S_IFREG | 0o644,
        Ext4DirEntry2::EXT4_FT_BLKDEV => Ext4Inode::S_IFBLK | 0o600,
        Ext4DirEntry2::EXT4_FT_CHRDEV => Ext4Inode::S_IFCHR | 0o600,
        Ext4DirEntry2::EXT4_FT_FIFO => Ext4Inode::S_IFIFO | 0o644,
        Ext4DirEntry2::EXT4_FT_SOCK => Ext4Inode::S_IFSOCK | 0o644,
        _ => return Err(Ext4Error::invalid_input().with_operation("inode:create_file_type")),
    };
    let payload = match file_type {
        Ext4DirEntry2::EXT4_FT_CHRDEV | Ext4DirEntry2::EXT4_FT_BLKDEV => match initial_data {
            Some(data) => CreateInodePayload::Data(data),
            None => CreateInodePayload::Device(DeviceNumber::ZERO),
        },
        _ => initial_data.map_or(CreateInodePayload::Empty, CreateInodePayload::Data),
    };
    create_inode_at(
        device,
        fs,
        CreateEntryRequest {
            parent: parent_ino_num,
            name: child,
            mode: imode,
            uid,
            gid,
        },
        payload,
        file_type,
    )
}
