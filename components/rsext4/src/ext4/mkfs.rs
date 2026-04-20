use super::*;

/// Derived filesystem geometry used only during mkfs planning.
pub struct FsLayoutInfo {
    /// Logical block size in bytes.
    block_size: u32,
    /// Blocks per group.
    blocks_per_group: u32,
    /// Inodes per group.
    inodes_per_group: u32,
    /// Inode size in bytes.
    inode_size: u16,
    /// Number of block groups.
    groups: u32,
    /// Group-descriptor size in bytes.
    desc_size: u16,
    /// Number of descriptors that fit in one block.
    descs_per_block: u32,
    /// Number of blocks occupied by the primary GDT.
    gdt_blocks: u32,
    /// Number of blocks occupied by each group's inode table.
    inode_table_blocks: u32,
    /// First data block number stored in `s_first_data_block`.
    first_data_block: u32,
    /// Reserved GDT blocks kept for future growth.
    reserved_gdt_blocks: u32,
    /// Group 0 block-bitmap block number.
    group0_block_bitmap: u32,
    /// Group 0 inode-bitmap block number.
    group0_inode_bitmap: u32,
    /// Group 0 inode-table start block.
    group0_inode_table: u32,
    /// Number of metadata blocks consumed in group 0.
    group0_metadata_blocks: u32,
    /// Total reserved blocks kept for privileged users.
    reserved_blocks: u64,
}

/// Per-group layout derived during mkfs.
pub struct BlcokGroupLayout {
    /// Absolute first block of the group.
    pub group_start_block: u64,
    /// Absolute block number of the block bitmap.
    pub group_blcok_bitmap_startblocks: u64,
    /// Absolute block number of the inode bitmap.
    pub group_inode_bitmap_startblocks: u64,
    /// Absolute start block of the inode table.
    pub group_inode_table_startblocks: u64,
    /// Number of blocks consumed by metadata inside the group.
    pub metadata_blocks_in_group: u32,
}

pub fn compute_fs_layout(inode_size: u16, total_blocks: u64) -> FsLayoutInfo {
    let block_size: u32 = 1024u32 << LOG_BLOCK_SIZE;

    // ext4 defaults to `8 * block_size` blocks per group.
    let blocks_per_group: u32 = 8 * block_size;

    // Use a simple density heuristic for inode count.
    let inodes_per_group: u32 = blocks_per_group / 4;

    // Round up so the last partial group is still represented.
    let groups: u32 = total_blocks.div_ceil(blocks_per_group as u64) as u32;

    // Prefer the 64-bit descriptor format unless the feature set explicitly
    // falls back to the legacy 32-bit layout.
    let desc_size: u16 =
        if DEFAULT_FEATURE_INCOMPAT & Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT != 0 {
            GROUP_DESC_SIZE
        } else {
            GROUP_DESC_SIZE_OLD
        };

    // Descriptor packing determines how many GDT blocks are required.
    let descs_per_block: u32 = if desc_size == 0 {
        0
    } else {
        block_size / desc_size as u32
    };

    // Number of blocks used by the primary group descriptor table.
    let gdt_blocks: u32 = if descs_per_block == 0 {
        0
    } else {
        groups.div_ceil(descs_per_block)
    };

    // Each group stores a full inode table contiguous to its bitmaps.
    let inode_table_blocks: u32 = if block_size == 0 {
        0
    } else {
        (inodes_per_group * inode_size as u32).div_ceil(block_size)
    };

    // ext4 uses `s_first_data_block = 0` for block sizes above 1 KiB, and `1`
    // for 1 KiB filesystems.
    let first_data_block: u32 = if block_size > 1024 { 0 } else { 1 };

    // Reserve extra GDT space for potential future resize support.
    let reserved_gdt_blocks: u32 = RESERVED_GDT_BLOCKS;

    // Group 0 hosts the primary superblock and primary GDT, so its bitmaps and
    // inode table start after the reserved GDT area.
    let group0_start: u32 = first_data_block;
    let reserved_gdt_start: u32 = group0_start + 2; // boot/super + primary GDT
    let group0_block_bitmap: u32 = reserved_gdt_start + reserved_gdt_blocks;
    let group0_inode_bitmap: u32 = group0_block_bitmap + 1;
    let group0_inode_table: u32 = group0_inode_bitmap + 1;
    let group0_metadata_blocks: u32 = (group0_inode_table + inode_table_blocks) - group0_start;

    // Reserve roughly 5% of blocks for privileged recovery space.
    let reserved_blocks: u64 = total_blocks / 20;

    FsLayoutInfo {
        block_size,
        blocks_per_group,
        inodes_per_group,
        inode_size,
        groups,
        desc_size,
        descs_per_block,
        gdt_blocks,
        inode_table_blocks,
        first_data_block,
        reserved_gdt_blocks,
        group0_block_bitmap,
        group0_inode_bitmap,
        group0_inode_table,
        group0_metadata_blocks,
        reserved_blocks,
    }
}

pub fn mkfs<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
    debug!("Start initializing Ext4 filesystem...");
    // Disable journaling while laying out the initial filesystem image. The
    // journal inode and journal superblock do not exist yet at this stage.
    block_dev.set_journal_use(false);
    let old_jouranl_use = block_dev.is_use_journal();

    // Compute the full mkfs layout before any on-disk write happens.
    let total_blocks = block_dev.total_blocks();
    let layout = compute_fs_layout(DEFAULT_INODE_SIZE, total_blocks);
    let total_groups = layout.groups;

    debug!("  Total blocks: {total_blocks}");
    debug!("  Block size: {} bytes", layout.block_size);
    debug!("  Block group count: {total_groups}");
    debug!("  Blocks per group: {}", layout.blocks_per_group);
    debug!("  Inodes per group: {}", layout.inodes_per_group);

    // Write the primary superblock and any sparse backups first so every later
    // descriptor/bitmap write can assume a valid superblock image exists.
    let superblock = build_superblock(total_blocks, &layout);
    write_superblock(block_dev, &superblock)?;
    debug!("Superblock written");

    write_superblock_redundant_backup(block_dev, &superblock, total_groups, &layout)?;

    let mut descs: VecDeque<Ext4GroupDesc> = VecDeque::new();
    // Seed all group descriptors before initializing individual bitmaps.
    for group_id in 0..total_groups {
        let mut desc = build_uninit_group_desc(&superblock, group_id, &layout);
        write_group_desc(block_dev, group_id, &mut desc)?;
        descs.push_back(desc);
    }
    write_gdt_redundant_backup(block_dev, &descs, &superblock, total_groups, &layout)?;
    debug!("{total_groups} block group descriptors written");

    // Group 0 is initialized eagerly because mkfs immediately creates the root
    // directory inside it.
    initialize_group_0(block_dev, &layout)?;
    debug!("Block group 0 initialized (for root directory)");

    // Other groups start with only metadata blocks allocated.
    initialize_other_groups_bitmaps(block_dev, &layout, &superblock)?;

    let mut initialized_descs: VecDeque<Ext4GroupDesc> = VecDeque::new();
    for group_id in 0..total_groups {
        let mut desc = build_uninit_group_desc(&superblock, group_id, &layout);
        if group_id == 0 {
            desc.bg_flags = Ext4GroupDesc::EXT4_BG_INODE_ZEROED;
        }
        write_group_desc(block_dev, group_id, &mut desc)?;
        initialized_descs.push_back(desc);
    }
    write_gdt_redundant_backup(
        block_dev,
        &initialized_descs,
        &superblock,
        total_groups,
        &layout,
    )?;

    // Reuse the normal mount/bootstrap path to create root and lost+found so
    // mkfs and mount share the same initialization logic.
    {
        let mut fs = Ext4FileSystem::mount(block_dev).expect("Mount Failed!");
        fs.umount(block_dev)?;
    }

    // Final sanity check: read back the superblock and validate the magic.
    let verify_sb = read_superblock(block_dev)?;

    // Restore the previous journal setting for the caller.
    block_dev.set_journal_use(old_jouranl_use);

    if verify_sb.s_magic == EXT4_SUPER_MAGIC {
        debug!(
            "Format completed, superblock magic verified: {:#x}",
            verify_sb.s_magic
        );
        Ok(())
    } else {
        debug!("Superblock magic verification failed");
        Err(Ext4Error::corrupted())
    }
}

/// Builds the in-memory superblock used by mkfs.
fn build_superblock(total_blocks: u64, layout: &FsLayoutInfo) -> Ext4Superblock {
    let mut sb = Ext4Superblock {
        s_magic: EXT4_SUPER_MAGIC,
        s_blocks_count_lo: (total_blocks & 0xFFFFFFFF) as u32,
        s_blocks_count_hi: (total_blocks >> 32) as u32,
        s_log_block_size: LOG_BLOCK_SIZE,
        s_log_cluster_size: LOG_BLOCK_SIZE,
        s_blocks_per_group: layout.blocks_per_group,
        s_inodes_per_group: layout.inodes_per_group,
        s_clusters_per_group: layout.blocks_per_group,
        s_inodes_count: layout.groups * layout.inodes_per_group,
        s_inode_size: layout.inode_size,
        s_first_ino: RESERVED_INODES + 1,
        s_first_data_block: layout.first_data_block,
        s_r_blocks_count_lo: (layout.reserved_blocks & 0xFFFFFFFF) as u32,
        s_r_blocks_count_hi: (layout.reserved_blocks >> 32) as u32,
        ..Default::default()
    };

    // Seed the directory hash machinery and UUID fields up front so every
    // later checksum uses the final superblock identity.
    let uuid = generate_uuid();
    sb.s_hash_seed = uuid.0;

    let filesys_uuid = generate_uuid_8();
    sb.s_uuid = filesys_uuid;

    // Initial free-block count equals total blocks minus reserved space and the
    // metadata consumed by group 0.
    let metadata_blocks = layout.group0_metadata_blocks as u64;
    let mut free_blocks = total_blocks
        .saturating_sub(metadata_blocks)
        .saturating_sub(layout.reserved_blocks);
    if free_blocks > total_blocks {
        free_blocks = 0;
    }
    sb.s_free_blocks_count_lo = (free_blocks & 0xFFFFFFFF) as u32;
    sb.s_free_blocks_count_hi = (free_blocks >> 32) as u32;

    sb.s_min_extra_isize = 32;
    sb.s_want_extra_isize = 32;

    // Reserved inode numbers start out unavailable.
    sb.s_free_inodes_count = sb.s_inodes_count.saturating_sub(RESERVED_INODES);

    // Mark the freshly created filesystem clean and choose the default error
    // policy used by this implementation.
    sb.s_state = Ext4Superblock::EXT4_VALID_FS;
    sb.s_errors = Ext4Superblock::EXT4_ERRORS_RO;

    // Advertise Linux dynamic-revision semantics.
    sb.s_creator_os = Ext4Superblock::EXT4_OS_LINUX;
    sb.s_rev_level = Ext4Superblock::EXT4_DYNAMIC_REV;

    // Enable the default feature set chosen for this implementation.
    sb.s_feature_compat = DEFAULT_FEATURE_COMPAT;
    sb.s_feature_incompat = DEFAULT_FEATURE_INCOMPAT;
    sb.s_feature_ro_compat = DEFAULT_FEATURE_RO_COMPAT;

    // Descriptor size and checksum type must be finalized before the
    // superblock checksum is computed.
    sb.s_desc_size = layout.desc_size;
    sb.s_reserved_gdt_blocks = layout.reserved_gdt_blocks as u16;
    sb.s_checksum_type = if ext4_superblock_has_metadata_csum(&sb) {
        1
    } else {
        0
    };
    sb.update_checksum();

    sb
}

/// Builds an initial group descriptor before per-group bitmaps are written.
fn build_uninit_group_desc(
    sb: &Ext4Superblock,
    group_id: u32,
    layout: &FsLayoutInfo,
) -> Ext4GroupDesc {
    let mut desc = Ext4GroupDesc::default();

    // Derive the physical layout from the shared group-layout helper so mkfs
    // and backup-writing logic stay consistent.
    let gl = cloc_group_layout(
        group_id,
        sb,
        layout.blocks_per_group,
        layout.inode_table_blocks,
        layout.group0_block_bitmap,
        layout.group0_inode_bitmap,
        layout.group0_inode_table,
        layout.gdt_blocks,
    );

    // Persist the group-local metadata block locations.
    desc.bg_block_bitmap_lo = gl.group_blcok_bitmap_startblocks as u32;
    desc.bg_inode_bitmap_lo = gl.group_inode_bitmap_startblocks as u32;
    desc.bg_inode_table_lo = gl.group_inode_table_startblocks as u32;

    // Free-block count is the group's total capacity minus its metadata area.
    let used_meta = gl.metadata_blocks_in_group;
    let free_blocks = layout.blocks_per_group.saturating_sub(used_meta);

    if group_id == 0 {
        // Group 0 consumes the reserved inode range immediately.
        desc.bg_free_blocks_count_lo = free_blocks as u16;
        desc.bg_free_inodes_count_lo =
            layout.inodes_per_group.saturating_sub(RESERVED_INODES) as u16;
        desc.bg_itable_unused_lo = layout.inodes_per_group.saturating_sub(RESERVED_INODES) as u16;
    } else {
        desc.bg_free_blocks_count_lo = free_blocks as u16;
        desc.bg_free_inodes_count_lo = layout.inodes_per_group as u16;
        desc.bg_itable_unused_lo = layout.inodes_per_group as u16;
    }

    // This implementation initializes descriptors directly and does not rely on
    // deferred UNINIT accounting here.
    desc.bg_free_blocks_count_hi = 0;
    desc.bg_free_inodes_count_hi = 0;
    desc.bg_used_dirs_count_lo = 0;
    desc.bg_used_dirs_count_hi = 0;
    desc.bg_flags = 0;

    desc
}

/// Writes sparse-super superblock backups to eligible groups.
fn write_superblock_redundant_backup<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    sb: &Ext4Superblock,
    groups_count: u32,
    fs_layout: &FsLayoutInfo,
) -> Ext4Result<()> {
    // Group 0 already holds the primary copy, so backup writing starts from 1.
    let sprse_feature =
        sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER);
    if sprse_feature {
        for gid in 1..groups_count {
            let group_layout = cloc_group_layout(
                gid,
                sb,
                fs_layout.blocks_per_group,
                fs_layout.inode_table_blocks,
                fs_layout.group0_block_bitmap,
                fs_layout.group0_inode_bitmap,
                fs_layout.group0_inode_table,
                fs_layout.gdt_blocks,
            );
            if need_redundant_backup(gid) {
                let super_blocks = group_layout.group_start_block;
                block_dev
                    .read_block(AbsoluteBN::new(super_blocks))
                    .expect("Superblock read failed!");
                let buffer = block_dev.buffer_mut();
                sb.to_disk_bytes(&mut buffer[0..SUPERBLOCK_SIZE]);
                block_dev.write_block(AbsoluteBN::new(super_blocks), true)?;
            }
        }
    }
    Ok(())
}

/// Writes the primary superblock to disk.
pub(crate) fn write_superblock<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    sb: &Ext4Superblock,
) -> Ext4Result<()> {
    // The primary ext4 superblock always starts at byte offset 1024.
    if BLOCK_SIZE == 1024 {
        block_dev.read_block(AbsoluteBN::from(1u32))?;
        let buffer = block_dev.buffer_mut();
        sb.to_disk_bytes(&mut buffer[0..SUPERBLOCK_SIZE]);
        block_dev.write_block(AbsoluteBN::from(1u32), true)?;
    } else {
        block_dev.read_block(AbsoluteBN::from(0u32))?;
        let buffer = block_dev.buffer_mut();
        let offset = Ext4Superblock::SUPERBLOCK_OFFSET as usize;
        let end = offset + Ext4Superblock::SUPERBLOCK_SIZE;
        sb.to_disk_bytes(&mut buffer[offset..end]);
        // Force the write out immediately so later mount-time reads never see a
        // stale primary superblock during crash recovery.
        block_dev.write_block(AbsoluteBN::from(0u32), false)?;
    }

    Ok(())
}

/// Reads the primary superblock from disk.
pub(crate) fn read_superblock<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
) -> Ext4Result<Ext4Superblock> {
    // Read the containing filesystem block, then slice out the 1024-byte
    // superblock payload.
    if BLOCK_SIZE == 1024 {
        block_dev.read_block(AbsoluteBN::from(1u32))?;
        let buffer = block_dev.buffer();
        let sb = Ext4Superblock::from_disk_bytes(&buffer[0..SUPERBLOCK_SIZE]);
        Ok(sb)
    } else {
        block_dev.read_block(AbsoluteBN::from(0u32))?;
        let buffer = block_dev.buffer();
        let offset = Ext4Superblock::SUPERBLOCK_OFFSET as usize;
        let end = offset + Ext4Superblock::SUPERBLOCK_SIZE;
        let sb = Ext4Superblock::from_disk_bytes(&buffer[offset..end]);
        Ok(sb)
    }
}

/// Writes redundant GDT copies to sparse-super backup groups.
fn write_gdt_redundant_backup<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    descs: &VecDeque<Ext4GroupDesc>,
    sb: &Ext4Superblock,
    groups_count: u32,
    fs_layout: &FsLayoutInfo,
) -> Ext4Result<()> {
    // Validate that the reserved GDT area can hold the serialized descriptor
    // table before any backup write starts.
    let desc_size = sb.get_desc_size();
    let desc_all_size = descs.len() * desc_size as usize;
    let can_recive_size = fs_layout.gdt_blocks * fs_layout.descs_per_block * desc_size as u32;
    if can_recive_size < desc_all_size as u32 {
        return Err(Ext4Error::buffer_too_small(
            can_recive_size as usize,
            desc_all_size,
        ));
    }

    let sprse_feature =
        sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER);
    if sprse_feature {
        for gid in 1..groups_count {
            if need_redundant_backup(gid) {
                let group_layout = cloc_group_layout(
                    gid,
                    sb,
                    fs_layout.blocks_per_group,
                    fs_layout.inode_table_blocks,
                    fs_layout.group0_block_bitmap,
                    fs_layout.group0_inode_bitmap,
                    fs_layout.group0_inode_table,
                    fs_layout.gdt_blocks,
                );
                let gdt_start = group_layout.group_start_block + 1;

                let mut desc_iter = descs.iter();
                // Stream descriptor copies block by block into the reserved GDT
                // area of this backup group.
                for gdt_block_id in gdt_start..group_layout.group_blcok_bitmap_startblocks {
                    block_dev.read_block(AbsoluteBN::new(gdt_block_id))?;
                    let buffer = block_dev.buffer_mut();
                    let mut current_offset = 0_usize;
                    for _ in 0..fs_layout.descs_per_block {
                        if let Some(desc) = desc_iter.next() {
                            desc.to_disk_bytes(
                                &mut buffer[current_offset..current_offset + desc_size as usize],
                            );
                            current_offset += desc_size as usize;
                        }
                    }
                    block_dev.write_block(AbsoluteBN::new(gdt_block_id), true)?;
                }
            }
        }
    }

    Ok(())
}

/// Writes one group descriptor into the primary GDT.
fn write_group_desc<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    group_id: u32,
    desc: &mut Ext4GroupDesc,
) -> Ext4Result<()> {
    // Resolve the descriptor size from the on-disk superblock so the write path
    // matches the exact format chosen during mkfs.
    let superblock = read_superblock(block_dev)?;
    let desc_size = superblock.get_desc_size() as usize;

    // Convert the descriptor's byte offset inside the GDT into a physical block
    // number plus an offset within that block.
    let gdt_base: u64 = BLOCK_SIZE as u64;
    let byte_offset = gdt_base + group_id as u64 * desc_size as u64;
    let block_size_u64 = BLOCK_SIZE as u64;
    let block_num = byte_offset / block_size_u64;
    let in_block = (byte_offset % block_size_u64) as usize;
    let end = in_block + desc_size;

    let inode_bitmap_blk = desc.inode_bitmap() as u32;
    block_dev.read_block(inode_bitmap_blk.into())?;
    let inode_bitmap_bytes = block_dev.buffer().to_vec();
    let block_bitmap_blk = desc.block_bitmap() as u32;
    block_dev.read_block(block_bitmap_blk.into())?;
    let block_bitmap_bytes = block_dev.buffer().to_vec();
    desc.update_checksum(
        &superblock,
        group_id,
        Some(&block_bitmap_bytes),
        Some(&inode_bitmap_bytes),
    );

    block_dev.read_block(AbsoluteBN::new(block_num))?;
    let buffer = block_dev.buffer_mut();
    if end > buffer.len() {
        return Err(Ext4Error::corrupted());
    }
    desc.to_disk_bytes(&mut buffer[in_block..end]);
    block_dev.write_block(AbsoluteBN::new(block_num), true)?;

    Ok(())
}

/// Initializes group 0 bitmaps, inode table, and descriptor state.
fn initialize_group_0<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    layout: &FsLayoutInfo,
) -> Ext4Result<()> {
    // Group 0 has a fixed layout derived during mkfs planning.
    let block_bitmap_blk = layout.group0_block_bitmap;
    let inode_bitmap_blk = layout.group0_inode_bitmap;
    let inode_table_blk = layout.group0_inode_table;

    {
        let buffer = block_dev.buffer_mut();
        buffer.fill(0);
        // Mark all group-0 metadata blocks allocated in the block bitmap.
        let used_metadata_blocks = layout.group0_metadata_blocks as usize;
        for i in 0..used_metadata_blocks {
            let byte_idx = i / 8;
            let bit_idx = i % 8;
            buffer[byte_idx] |= 1 << bit_idx;
        }
    }
    block_dev.write_block(block_bitmap_blk.into(), true)?;

    {
        let buffer = block_dev.buffer_mut();
        buffer.fill(0);
        // Mark reserved inodes allocated.
        for i in 0..RESERVED_INODES {
            let byte_idx = (i / 8) as usize;
            let bit_idx = i % 8;
            buffer[byte_idx] |= 1 << bit_idx;
        }

        // Mark bitmap padding bits allocated so they are never handed out.
        let bits_per_group = BLOCK_SIZE_U32 * 8;
        for i in layout.inodes_per_group..bits_per_group {
            let byte_idx: usize = (i / 8) as usize;
            let bit_idx = i % 8;
            buffer[byte_idx] |= 1 << bit_idx;
        }
    }
    block_dev.write_block(inode_bitmap_blk.into(), true)?;

    // Zero the inode table before the filesystem is mounted for the first time.
    {
        let buffer = block_dev.buffer_mut();
        buffer.fill(0);
    }
    for i in 0..layout.inode_table_blocks {
        block_dev.write_block((inode_table_blk + i).into(), true)?;
    }

    // Persist the now-initialized descriptor for group 0.
    let mut desc = Ext4GroupDesc {
        bg_flags: Ext4GroupDesc::EXT4_BG_INODE_ZEROED,
        bg_free_blocks_count_lo: layout
            .blocks_per_group
            .saturating_sub(layout.group0_metadata_blocks) as u16,
        bg_free_inodes_count_lo: layout.inodes_per_group.saturating_sub(RESERVED_INODES) as u16,
        bg_itable_unused_lo: layout.inodes_per_group.saturating_sub(RESERVED_INODES) as u16,
        bg_block_bitmap_lo: block_bitmap_blk,
        bg_inode_bitmap_lo: inode_bitmap_blk,
        bg_inode_table_lo: inode_table_blk,
        ..Default::default()
    };

    write_group_desc(block_dev, 0, &mut desc)?;

    Ok(())
}

/// Initializes bitmaps for every group after group 0.
///
/// Fresh groups start with only their metadata blocks allocated.
fn initialize_other_groups_bitmaps<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    layout: &FsLayoutInfo,
    sb: &Ext4Superblock,
) -> Ext4Result<()> {
    // Group 0 has already been handled separately.
    for group_id in 1..layout.groups {
        // Reuse the same layout calculation as descriptor construction.
        let gl = cloc_group_layout(
            group_id,
            sb,
            layout.blocks_per_group,
            layout.inode_table_blocks,
            layout.group0_block_bitmap,
            layout.group0_inode_bitmap,
            layout.group0_inode_table,
            layout.gdt_blocks,
        );

        let block_bitmap_blk = gl.group_blcok_bitmap_startblocks as u32;
        let inode_bitmap_blk = gl.group_inode_bitmap_startblocks as u32;

        // Start with a zeroed block bitmap, then mark metadata blocks used.
        {
            let buffer = block_dev.buffer_mut();
            buffer.fill(0);
            let used_blocks = gl.metadata_blocks_in_group as usize;
            for i in 0..used_blocks {
                let byte_idx = i / 8;
                let bit_idx = i % 8;
                buffer[byte_idx] |= 1 << bit_idx;
            }
        }
        block_dev.write_block(block_bitmap_blk.into(), true)?;

        {
            // Start with all inodes free, then mask the trailing padding bits.
            let buffer = block_dev.buffer_mut();
            buffer.fill(0);

            let bits_per_group = BLOCK_SIZE_U32 * 8;
            for i in layout.inodes_per_group..bits_per_group {
                let byte_idx: usize = (i / 8) as usize;
                let bit_idx = i % 8;
                buffer[byte_idx] |= 1 << bit_idx;
            }
        }
        block_dev.write_block(inode_bitmap_blk.into(), true)?;
    }

    Ok(())
}
