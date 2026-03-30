//! Directory creation helpers.

use alloc::{string::String, vec::Vec};

use crate::{
    alloc::string::ToString,
    blockdev::*,
    checksum::update_ext4_dirblock_csum32,
    config::*,
    crc32c::ext4_superblock_has_metadata_csum,
    dir::{
        create_lost_found_directory, get_inode_with_num, insert_dir_entry,
        split_paren_child_and_tranlatevalid,
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

/// Creates a directory inode and links it into the namespace.
///
/// The flow normalizes the path, ensures parent directories exist, builds the
/// new `.`/`..` block, persists the child inode, and finally links it into the
/// parent directory.
fn mkdir_internal<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    existing_ok: bool,
) -> Ext4Result<Ext4Inode> {
    let has_checksum = ext4_superblock_has_metadata_csum(&fs.superblock);
    let norm_path = split_paren_child_and_tranlatevalid(path);
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

    // Materialize missing parent directories from the top down.
    let mut cur_path = String::new();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        cur_path.push('/');
        cur_path.push_str(part);
        ensure_directory(device, fs, &cur_path)?;
    }

    let child = parts.last().unwrap().to_string();
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

    let (parent_ino_num, mut parent_inode) =
        get_inode_with_num(fs, device, &parent)?.ok_or(Ext4Error::not_found())?;
    if !parent_inode.is_dir() {
        return Err(Ext4Error::not_dir());
    }

    if parent == "/" && child == "lost+found" {
        create_lost_found_directory(fs, device)?;
        return find_file(fs, device, "/lost+found");
    }

    // Allocate the child inode and its first directory block only after parent validation.
    let new_dir_ino = fs.alloc_inode(device)?;
    let data_block = fs.alloc_block(device)?;
    let new_dir_gen = fs.get_inode_by_num(device, new_dir_ino)?.i_generation;

    {
        // Initialize `.` and `..`, leaving room for the checksum tail when enabled.
        let cached = fs.datablock_cache.create_new(data_block);
        let data = &mut cached.data;

        let dot_name = b".";
        let dot_rec_len = Ext4DirEntry2::entry_len(dot_name.len() as u8);
        let dot = Ext4DirEntry2::new(
            new_dir_ino.raw(),
            dot_rec_len,
            Ext4DirEntry2::EXT4_FT_DIR,
            dot_name,
        );

        let dotdot_name = b"..";
        let dotdot_rec_len = if has_checksum {
            (BLOCK_SIZE as u16)
                .saturating_sub(dot_rec_len)
                .saturating_sub(Ext4DirEntryTail::TAIL_LEN)
        } else {
            (BLOCK_SIZE as u16).saturating_sub(dot_rec_len)
        };
        let dotdot = Ext4DirEntry2::new(
            parent_ino_num.raw(),
            dotdot_rec_len,
            Ext4DirEntry2::EXT4_FT_DIR,
            dotdot_name,
        );

        dot.to_disk_bytes(&mut data[0..8]);
        let name_len = dot.name_len as usize;
        data[8..8 + name_len].copy_from_slice(&dot.name[..name_len]);

        let offset = dot_rec_len as usize;
        dotdot.to_disk_bytes(&mut data[offset..offset + 8]);
        let name_len = dotdot.name_len as usize;
        data[offset + 8..offset + 8 + name_len].copy_from_slice(&dotdot.name[..name_len]);

        if has_checksum {
            let tail = Ext4DirEntryTail::new();
            let tail_offset = BLOCK_SIZE - Ext4DirEntryTail::TAIL_LEN as usize;
            tail.to_disk_bytes(
                &mut data[tail_offset..tail_offset + Ext4DirEntryTail::TAIL_LEN as usize],
            );
            update_ext4_dirblock_csum32(&fs.superblock, new_dir_ino.raw(), new_dir_gen, data);
        }
    }

    // Persist the child directory inode through the unified metadata path.
    let (group_idx, _idx) = fs.inode_allocator.global_to_group(new_dir_ino)?;
    let dir_mode = Ext4Inode::S_IFDIR | 0o755;
    let mut new_inode = Ext4Inode::empty_for_reuse(fs.default_inode_extra_isize());
    new_inode.i_links_count = 2;
    new_inode.i_size_lo = BLOCK_SIZE as u32;
    new_inode.i_size_high = 0;
    new_inode.i_blocks_lo = (BLOCK_SIZE / 512) as u32;
    new_inode.l_i_blocks_high = 0;
    new_inode.i_flags = Ext4Inode::mask_flags_for_mode(
        dir_mode,
        parent_inode.i_flags & Ext4Inode::EXT4_FL_INHERITED,
    );
    build_file_block_mapping(fs, &mut new_inode, &[data_block], device);
    let mut create_update = Ext4InodeMetadataUpdate::create(dir_mode);
    if fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_PROJECT)
        && parent_inode.i_flags & Ext4Inode::EXT4_PROJINHERIT_FL != 0
    {
        create_update.projid = Some(parent_inode.i_projid);
    }
    fs.finalize_inode_update(device, new_dir_ino, &mut new_inode, create_update)?;

    // Publish the new directory: bump link accounting, group stats, then insert the name.
    let parent_new_links = parent_inode.i_links_count.saturating_add(1);
    fs.set_inode_links_count(device, parent_ino_num, parent_new_links)?;

    if let Some(desc) = fs.get_group_desc_mut(group_idx) {
        let newc = desc.used_dirs_count().saturating_add(1);
        desc.bg_used_dirs_count_lo = (newc & 0xFFFF) as u16;
        desc.bg_used_dirs_count_hi = ((newc >> 16) & 0xFFFF) as u16;
    }

    insert_dir_entry(
        fs,
        device,
        parent_ino_num,
        &mut parent_inode,
        new_dir_ino,
        &child,
        Ext4DirEntry2::EXT4_FT_DIR,
    )?;

    fs.get_inode_by_num(device, new_dir_ino)
}

pub(crate) fn ensure_directory<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> Ext4Result<Ext4Inode> {
    mkdir_internal(device, fs, path, true)
}

/// Creates a directory and any missing parent directories.
pub fn mkdir<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> Ext4Result<Ext4Inode> {
    mkdir_internal(device, fs, path, false)
}
