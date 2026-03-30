//! Root directory bootstrap helpers.

use log::debug;

use crate::{
    blockdev::*, bmalloc::BGIndex, checksum::update_ext4_dirblock_csum32, config::*,
    crc32c::ext4_superblock_has_metadata_csum, dir::insert_dir_entry, disknode::*,
    endian::DiskFormat, entries::*, error::*, ext4::*, file::*, metadata::Ext4InodeMetadataUpdate,
    superblock::Ext4Superblock,
};

/// Creates the root directory contents and inode.
///
/// This formats the first directory block with `.` and `..`, persists a fresh
/// root inode through the metadata path, and updates the directory count in the
/// first block group.
pub fn create_root_directory_entry<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
) -> Ext4Result<()> {
    debug!("Initializing root directory...");

    let root_inode_num = fs.root_inode;
    let data_block = fs.alloc_block(block_dev)?;
    let has_checksum = ext4_superblock_has_metadata_csum(&fs.superblock);
    let root_gen = fs.get_root(block_dev)?.i_generation;

    {
        // Format the initial root directory block before the inode is finalized.
        let cached = fs.datablock_cache.create_new(data_block);
        let data = &mut cached.data;

        let dot_name = b".";
        let dot_rec_len = Ext4DirEntry2::entry_len(dot_name.len() as u8);
        let dot = Ext4DirEntry2::new(
            root_inode_num.raw(),
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
            root_inode_num.raw(),
            dotdot_rec_len,
            Ext4DirEntry2::EXT4_FT_DIR,
            dotdot_name,
        );

        {
            dot.to_disk_bytes(&mut data[0..8]);
            let name_len = dot.name_len as usize;
            data[8..8 + name_len].copy_from_slice(&dot.name[..name_len]);
        }

        {
            let offset = dot_rec_len as usize;
            dotdot.to_disk_bytes(&mut data[offset..offset + 8]);
            let name_len = dotdot.name_len as usize;
            data[offset + 8..offset + 8 + name_len].copy_from_slice(&dotdot.name[..name_len]);
        }

        if has_checksum {
            let tail = Ext4DirEntryTail::new();
            let tail_offset = BLOCK_SIZE - Ext4DirEntryTail::TAIL_LEN as usize;
            tail.to_disk_bytes(
                &mut data[tail_offset..tail_offset + Ext4DirEntryTail::TAIL_LEN as usize],
            );
            update_ext4_dirblock_csum32(&fs.superblock, root_inode_num.raw(), root_gen, data);
        }
    }

    // Persist a clean directory inode that points at the newly initialized block.
    let dir_mode = Ext4Inode::S_IFDIR | 0o755;
    let mut inode = Ext4Inode::empty_for_reuse(fs.default_inode_extra_isize());
    inode.i_links_count = 2;
    inode.i_size_lo = BLOCK_SIZE as u32;
    inode.i_size_high = 0;
    inode.i_blocks_lo = (BLOCK_SIZE / 512) as u32;
    inode.l_i_blocks_high = 0;
    build_file_block_mapping(fs, &mut inode, &[data_block], block_dev);
    fs.finalize_inode_update(
        block_dev,
        fs.root_inode,
        &mut inode,
        Ext4InodeMetadataUpdate::create(dir_mode),
    )?;

    // Group 0 now contains one more live directory.
    if let Some(desc) = fs.get_group_desc_mut(BGIndex::new(0)) {
        let newc = desc.used_dirs_count().saturating_add(1);
        desc.bg_used_dirs_count_lo = (newc & 0xFFFF) as u16;
        desc.bg_used_dirs_count_hi = ((newc >> 16) & 0xFFFF) as u16;
    }

    debug!(
        "Root directory created: inode={}, data_block={}",
        fs.root_inode, data_block
    );
    Ok(())
}

/// Creates `/lost+found` and links it from the root directory.
///
/// The helper is idempotent for repeated mkfs-style setup and follows the same
/// directory bootstrap flow as root creation before linking the entry under `/`.
pub fn create_lost_found_directory<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
) -> Ext4Result<()> {
    // Allow callers to reuse the helper during setup without duplicating the directory.
    if file_entry_exisr(fs, block_dev, "/lost+found")? {
        return Ok(());
    }

    let root_inode_num = fs.root_inode;
    let mut root_inode = fs.get_root(block_dev)?;
    let has_checksum = ext4_superblock_has_metadata_csum(&fs.superblock);

    let lost_ino = fs.alloc_inode(block_dev)?;
    debug!("lost+found inode: {lost_ino}");

    let data_block = fs.alloc_block(block_dev)?;
    let lost_gen = fs.get_inode_by_num(block_dev, lost_ino)?.i_generation;

    {
        // Format the first block of the new directory, including the checksum tail.
        let cached = fs.datablock_cache.create_new(data_block);
        let data = &mut cached.data;

        let dot_name = b".";
        let dot_rec_len = Ext4DirEntry2::entry_len(dot_name.len() as u8);
        let dot = Ext4DirEntry2::new(
            lost_ino.raw(),
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
            root_inode_num.raw(),
            dotdot_rec_len,
            Ext4DirEntry2::EXT4_FT_DIR,
            dotdot_name,
        );

        {
            dot.to_disk_bytes(&mut data[0..8]);
            let name_len = dot.name_len as usize;
            data[8..8 + name_len].copy_from_slice(&dot.name[..name_len]);
        }

        {
            let offset = dot_rec_len as usize;
            dotdot.to_disk_bytes(&mut data[offset..offset + 8]);
            let name_len = dotdot.name_len as usize;
            data[offset + 8..offset + 8 + name_len].copy_from_slice(&dotdot.name[..name_len]);
        }

        if has_checksum {
            let tail = Ext4DirEntryTail::new();
            let tail_offset = BLOCK_SIZE - Ext4DirEntryTail::TAIL_LEN as usize;
            tail.to_disk_bytes(
                &mut data[tail_offset..tail_offset + Ext4DirEntryTail::TAIL_LEN as usize],
            );
            update_ext4_dirblock_csum32(&fs.superblock, lost_ino.raw(), lost_gen, data);
        }
    }

    let (lf_group, _idx) = fs.inode_allocator.global_to_group(lost_ino)?;
    let dir_mode = Ext4Inode::S_IFDIR | 0o755;
    let mut lost_inode = Ext4Inode::empty_for_reuse(fs.default_inode_extra_isize());
    lost_inode.i_links_count = 2;
    lost_inode.i_size_lo = BLOCK_SIZE as u32;
    lost_inode.i_size_high = 0;
    lost_inode.i_blocks_lo = (BLOCK_SIZE / 512) as u32;
    lost_inode.l_i_blocks_high = 0;
    lost_inode.i_flags =
        Ext4Inode::mask_flags_for_mode(dir_mode, root_inode.i_flags & Ext4Inode::EXT4_FL_INHERITED);
    build_file_block_mapping(fs, &mut lost_inode, &[data_block], block_dev);
    debug!(
        "When create lost+found inode iblock,:{:?} ,data_block:{:?}",
        lost_inode.i_block, data_block
    );
    // Carry project inheritance only when the feature bit and parent flag both allow it.
    let mut create_update = Ext4InodeMetadataUpdate::create(dir_mode);
    if fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_PROJECT)
        && root_inode.i_flags & Ext4Inode::EXT4_PROJINHERIT_FL != 0
    {
        create_update.projid = Some(root_inode.i_projid);
    }
    fs.finalize_inode_update(block_dev, lost_ino, &mut lost_inode, create_update)?;

    // Account the directory in its owning group before publishing the name in root.
    if let Some(desc) = fs.get_group_desc_mut(lf_group) {
        let newc = desc.used_dirs_count().saturating_add(1);
        desc.bg_used_dirs_count_lo = (newc & 0xFFFF) as u16;
        desc.bg_used_dirs_count_hi = ((newc >> 16) & 0xFFFF) as u16;
    }

    insert_dir_entry(
        fs,
        block_dev,
        root_inode_num,
        &mut root_inode,
        lost_ino,
        "lost+found",
        Ext4DirEntry2::EXT4_FT_DIR,
    )?;
    // The root gains one more subdirectory link after `lost+found` becomes visible.
    let root_new_links = root_inode.i_links_count.saturating_add(1);
    fs.set_inode_links_count(block_dev, fs.root_inode, root_new_links)?;

    fs.superblock.s_lpf_ino = lost_ino.raw();

    debug!("lost+found directory created: inode={lost_ino}, data_block={data_block}");

    Ok(())
}
