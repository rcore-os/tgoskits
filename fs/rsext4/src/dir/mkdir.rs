//! Directory creation helpers.

use alloc::{string::String, vec::Vec};

use crate::{
    alloc::string::ToString,
    blockdev::*,
    bmalloc::{AbsoluteBN, BGIndex, InodeNumber},
    checksum::update_ext4_dirblock_csum32,
    crc32c::ext4_superblock_has_metadata_csum,
    dir::{
        CreateEntryRequest, FileName, create_lost_found_directory, get_inode_with_num,
        insert_dir_entry_raw, normalize_path,
    },
    disknode::*,
    endian::DiskFormat,
    entries::*,
    error::*,
    ext4::*,
    file::*,
    loopfile::*,
    metadata::Ext4InodeMetadataUpdate,
    superblock::Ext4Superblock,
};

// Linux ext4_mkdir uses the same DATA + INDEX + inode-allocation budget as
// create and symlink. Writable quota is not implemented yet.
const MKDIR_TRANSACTION_CREDITS: usize = 39;

struct CreatedDirectory {
    number: InodeNumber,
    inode: Ext4Inode,
}

struct DirectoryPublishRollback<'a> {
    parent: InodeNumber,
    parent_links: u16,
    group: BGIndex,
    used_dirs: u32,
    child: InodeNumber,
    child_blocks: &'a [AbsoluteBN],
}

fn restore_directory_publish<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    rollback: DirectoryPublishRollback<'_>,
) -> Ext4Result<()> {
    if let Some(descriptor) = fs.get_group_desc_mut(rollback.group) {
        descriptor.bg_used_dirs_count_lo = (rollback.used_dirs & 0xffff) as u16;
        descriptor.bg_used_dirs_count_hi = ((rollback.used_dirs >> 16) & 0xffff) as u16;
    }

    let mut first_error = None;
    if let Err(error) = fs.set_inode_links_count(device, rollback.parent, rollback.parent_links) {
        first_error = Some(error.with_operation("rollback:mkdir_parent_links"));
    }
    if let Err(error) = discard_unpublished_inode(fs, device, rollback.child, rollback.child_blocks)
        && first_error.is_none()
    {
        first_error = Some(error);
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Creates one directory below an already resolved parent inode.
pub(crate) fn create_directory_at<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    request: CreateEntryRequest<'_>,
) -> Ext4Result<Ext4Inode> {
    let counters_before = fs.group_counter_snapshot();
    fs.with_metadata_transaction(device, MKDIR_TRANSACTION_CREDITS, |fs, device| {
        let created = create_directory_at_inner(device, fs, request)?;
        fs.inodetable_cache.flush(device, created.number)?;
        fs.inodetable_cache.flush(device, request.parent)?;
        fs.flush_changed_group_metadata(device, &counters_before)?;
        fs.sync_superblock(device)?;
        Ok(created.inode)
    })
}

fn create_directory_at_inner<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    request: CreateEntryRequest<'_>,
) -> Ext4Result<CreatedDirectory> {
    if request.name.is_reserved() || request.mode & Ext4Inode::S_IFMT != Ext4Inode::S_IFDIR {
        return Err(Ext4Error::invalid_input());
    }
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

    let dir_nlink_feature = fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_DIR_NLINK);
    let parent_old_links = parent_inode.i_links_count;
    let parent_new_links = parent_inode.incremented_links_count(dir_nlink_feature)?;
    let has_checksum = ext4_superblock_has_metadata_csum(&fs.superblock);
    let filesystem_block_size = fs.block_size();

    let new_dir_ino = fs.alloc_inode(device)?;
    let data_block = match fs.alloc_block(device) {
        Ok(block) => block,
        Err(error) => {
            let cleanup = fs
                .free_inode(device, new_dir_ino)
                .map_err(|error| error.with_operation("rollback:mkdir_inode"));
            return Err(error_after_cleanup(error, cleanup));
        }
    };
    let child_blocks = [data_block];
    let new_dir_gen = match fs.get_inode_by_num(device, new_dir_ino) {
        Ok(inode) => inode.i_generation,
        Err(error) => {
            return Err(error_after_cleanup(
                error,
                discard_unpublished_inode(fs, device, new_dir_ino, &child_blocks),
            ));
        }
    };

    if let Err(error) = fs
        .datablock_cache
        .modify_new_metadata(device, data_block, |data| {
            let block_size = data.len();
            let dot_rec_len = Ext4DirEntry2::entry_len(1);
            let dot = Ext4DirEntry2::new(
                new_dir_ino.raw(),
                dot_rec_len,
                Ext4DirEntry2::EXT4_FT_DIR,
                b".",
            );
            let dotdot_rec_len = if has_checksum {
                (block_size as u16)
                    .saturating_sub(dot_rec_len)
                    .saturating_sub(Ext4DirEntryTail::TAIL_LEN)
            } else {
                (block_size as u16).saturating_sub(dot_rec_len)
            };
            let dotdot = Ext4DirEntry2::new(
                request.parent.raw(),
                dotdot_rec_len,
                Ext4DirEntry2::EXT4_FT_DIR,
                b"..",
            );

            dot.to_disk_bytes(&mut data[0..8]);
            data[8] = b'.';
            let offset = dot_rec_len as usize;
            dotdot.to_disk_bytes(&mut data[offset..offset + 8]);
            data[offset + 8..offset + 10].copy_from_slice(b"..");
            if has_checksum {
                let tail = Ext4DirEntryTail::new();
                let tail_offset = block_size - Ext4DirEntryTail::TAIL_LEN as usize;
                tail.to_disk_bytes(
                    &mut data[tail_offset..tail_offset + Ext4DirEntryTail::TAIL_LEN as usize],
                );
                update_ext4_dirblock_csum32(&fs.superblock, new_dir_ino.raw(), new_dir_gen, data);
            }
        })
    {
        return Err(error_after_cleanup(
            error,
            discard_unpublished_inode(fs, device, new_dir_ino, &child_blocks),
        ));
    }
    if let Err(error) = fs.datablock_cache.flush_metadata(device, data_block) {
        return Err(error_after_cleanup(
            error,
            discard_unpublished_inode(fs, device, new_dir_ino, &child_blocks),
        ));
    }

    let (group_idx, _) = match fs.inode_allocator.global_to_group(new_dir_ino) {
        Ok(location) => location,
        Err(error) => {
            return Err(error_after_cleanup(
                error,
                discard_unpublished_inode(fs, device, new_dir_ino, &child_blocks),
            ));
        }
    };
    let mut new_inode = Ext4Inode::empty_for_reuse(fs.default_inode_extra_isize());
    new_inode.i_generation = new_dir_gen;
    new_inode.i_links_count = 2;
    new_inode.i_size_lo = filesystem_block_size as u32;
    new_inode.i_blocks_lo = (filesystem_block_size / 512) as u32;
    new_inode.i_flags = Ext4Inode::mask_flags_for_mode(
        request.mode,
        parent_inode.i_flags & Ext4Inode::EXT4_FL_INHERITED,
    );
    if let Err(error) = build_file_block_mapping_with_inode_num(
        fs,
        &mut new_inode,
        new_dir_ino,
        &child_blocks,
        device,
    ) {
        return Err(error_after_cleanup(
            error,
            discard_unpublished_inode(fs, device, new_dir_ino, &child_blocks),
        ));
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
    if let Err(error) = fs.finalize_inode_update(device, new_dir_ino, &mut new_inode, create_update)
    {
        return Err(error_after_cleanup(
            error,
            discard_unpublished_inode(fs, device, new_dir_ino, &child_blocks),
        ));
    }

    let used_dirs = match fs.get_group_desc(group_idx) {
        Some(descriptor) => descriptor.used_dirs_count(),
        None => {
            return Err(error_after_cleanup(
                Ext4Error::corrupted(),
                discard_unpublished_inode(fs, device, new_dir_ino, &child_blocks),
            ));
        }
    };
    if let Err(error) = fs.set_inode_links_count(device, request.parent, parent_new_links) {
        return Err(error_after_cleanup(
            error,
            restore_directory_publish(
                fs,
                device,
                DirectoryPublishRollback {
                    parent: request.parent,
                    parent_links: parent_old_links,
                    group: group_idx,
                    used_dirs,
                    child: new_dir_ino,
                    child_blocks: &child_blocks,
                },
            ),
        ));
    }
    if let Some(descriptor) = fs.get_group_desc_mut(group_idx) {
        let updated = used_dirs.saturating_add(1);
        descriptor.bg_used_dirs_count_lo = (updated & 0xffff) as u16;
        descriptor.bg_used_dirs_count_hi = ((updated >> 16) & 0xffff) as u16;
    }

    let mut parent_inode = match fs.get_inode_by_num(device, request.parent) {
        Ok(inode) => inode,
        Err(error) => {
            return Err(error_after_cleanup(
                error,
                restore_directory_publish(
                    fs,
                    device,
                    DirectoryPublishRollback {
                        parent: request.parent,
                        parent_links: parent_old_links,
                        group: group_idx,
                        used_dirs,
                        child: new_dir_ino,
                        child_blocks: &child_blocks,
                    },
                ),
            ));
        }
    };
    if let Err(error) = insert_dir_entry_raw(
        fs,
        device,
        request.parent,
        &mut parent_inode,
        new_dir_ino,
        request.name,
        Ext4DirEntry2::EXT4_FT_DIR,
    ) {
        let entry_absent = matches!(
            find_named_entry_in_parent(
                fs,
                device,
                request.parent,
                &parent_inode,
                request.name.as_bytes(),
            ),
            Err(lookup_error) if lookup_error.kind() == Ext4ErrorKind::NotFound
        );
        if !entry_absent {
            return Err(error);
        }
        return Err(error_after_cleanup(
            error,
            restore_directory_publish(
                fs,
                device,
                DirectoryPublishRollback {
                    parent: request.parent,
                    parent_links: parent_old_links,
                    group: group_idx,
                    used_dirs,
                    child: new_dir_ino,
                    child_blocks: &child_blocks,
                },
            ),
        ));
    }

    let inode = fs.get_inode_by_num(device, new_dir_ino)?;
    Ok(CreatedDirectory {
        number: new_dir_ino,
        inode,
    })
}

/// Creates a directory inode and links it into the namespace.
///
/// The flow normalizes the path, ensures parent directories exist, builds the
/// new `.`/`..` block, persists the child inode, and finally links it into the
/// parent directory.
fn mkdir_internal<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    existing_ok: bool,
    uid: u32,
    gid: u32,
) -> Ext4Result<Ext4Inode> {
    let norm_path = normalize_path(path);
    // Resolve trivial and already-existing paths before allocating anything.
    if norm_path.is_empty() {
        return Err(Ext4Error::invalid_input());
    }

    if norm_path == "/" {
        let root = fs.get_root(device)?;
        return if existing_ok {
            Ok(root)
        } else {
            Err(Ext4Error::already_exists())
        };
    }

    if let Some((_ino, inode)) = get_file_inode(fs, device, &norm_path)? {
        if existing_ok && inode.is_dir() {
            return Ok(inode);
        }
        return Err(Ext4Error::already_exists());
    }

    let parts: Vec<&str> = norm_path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return Err(Ext4Error::invalid_input());
    }
    for part in &parts {
        let component = FileName::new(part.as_bytes())?;
        if component.is_reserved() {
            return Err(Ext4Error::invalid_input());
        }
    }

    // Materialize missing parent directories from the top down.
    let mut cur_path = String::new();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        cur_path.push('/');
        cur_path.push_str(part);
        ensure_directory(device, fs, &cur_path, uid, gid)?;
    }

    let child = parts.last().copied().ok_or_else(Ext4Error::invalid_input)?;
    let child_name = FileName::new(child.as_bytes())?;
    let parent = if parts.len() == 1 {
        "/".to_string()
    } else {
        let mut p = String::new();
        for part in parts.iter().take(parts.len() - 1) {
            p.push('/');
            p.push_str(part);
        }
        p
    };

    let (parent_ino_num, parent_inode) =
        get_inode_with_num(fs, device, &parent)?.ok_or(Ext4Error::not_found())?;
    if !parent_inode.is_dir() {
        return Err(Ext4Error::not_dir());
    }

    if parent == "/" && child == "lost+found" {
        create_lost_found_directory(fs, device)?;
        return fs.find_file(device, "/lost+found");
    }
    create_directory_at(
        device,
        fs,
        CreateEntryRequest {
            parent: parent_ino_num,
            name: child_name,
            mode: Ext4Inode::S_IFDIR | 0o755,
            uid,
            gid,
        },
    )
}

pub(crate) fn ensure_directory<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    uid: u32,
    gid: u32,
) -> Ext4Result<Ext4Inode> {
    mkdir_internal(device, fs, path, true, uid, gid)
}

/// Creates a directory and any missing parent directories (root-owned).
pub fn mkdir<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> Ext4Result<Ext4Inode> {
    mkdir_internal(device, fs, path, false, 0, 0)
}

/// Creates a directory with explicit uid/gid ownership.
pub fn mkdir_with_owner<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    uid: u32,
    gid: u32,
) -> Ext4Result<Ext4Inode> {
    mkdir_internal(device, fs, path, false, uid, gid)
}
