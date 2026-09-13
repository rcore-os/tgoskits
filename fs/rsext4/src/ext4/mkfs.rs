use super::*;

/// Derived filesystem geometry used only during mkfs planning.
pub struct FsLayoutInfo {
    /// Filesystem block size in bytes.
    block_size: u32,
    /// Total filesystem blocks.
    total_blocks: u64,
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
pub struct BlockGroupLayout {
    /// Absolute first block of the group.
    pub group_start_block: u64,
    /// Absolute block number of the block bitmap.
    pub group_block_bitmap_start_block: u64,
    /// Absolute block number of the inode bitmap.
    pub group_inode_bitmap_start_block: u64,
    /// Absolute start block of the inode table.
    pub group_inode_table_start_block: u64,
    /// Number of blocks consumed by metadata inside the group.
    pub metadata_blocks_in_group: u32,
}

/// Derives the on-disk layout for a new filesystem without writing the device.
///
/// # Errors
///
/// Returns an error when the requested geometry is invalid, overflows the
/// supported on-disk fields, or cannot hold group-zero metadata.
pub fn compute_fs_layout(
    inode_size: u16,
    total_blocks: u64,
    block_size: u32,
) -> Ext4Result<FsLayoutInfo> {
    validate_mkfs_geometry(inode_size, block_size)?;

    // ext4 defaults to `8 * block_size` blocks per group.
    let blocks_per_group: u32 = 8 * block_size;

    // Round up so the last partial group is still represented.
    let first_data_block = u32::from(block_size == 1024);
    let data_blocks = total_blocks
        .checked_sub(u64::from(first_data_block))
        .ok_or_else(Ext4Error::bad_superblock)?;
    if data_blocks == 0 {
        return Err(Ext4Error::bad_superblock().with_operation("mkfs:empty_filesystem"));
    }
    let groups = u32::try_from(data_blocks.div_ceil(u64::from(blocks_per_group)))
        .map_err(|_| Ext4Error::overflow())?;

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

    // Every group stores a complete inode table. Cap the global inode density
    // so the smallest (normally final) group can contain its own mandatory
    // metadata instead of publishing descriptor pointers beyond s_blocks_count.
    let last_group = groups - 1;
    let last_group_start = u64::from(first_data_block)
        .checked_add(u64::from(last_group) * u64::from(blocks_per_group))
        .ok_or_else(Ext4Error::overflow)?;
    let last_group_blocks = total_blocks
        .checked_sub(last_group_start)
        .ok_or_else(Ext4Error::overflow)?
        .min(u64::from(blocks_per_group));
    let sparse_super =
        DEFAULT_FEATURE_RO_COMPAT & Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER != 0;
    let last_has_backup = sparse_super && need_redundant_backup(last_group);
    let fixed_metadata_blocks = if last_group == 0 {
        1u64 + u64::from(gdt_blocks) + u64::from(RESERVED_GDT_BLOCKS) + 2
    } else if last_has_backup {
        1u64 + u64::from(gdt_blocks) + 2
    } else {
        2
    };
    let inode_table_capacity = last_group_blocks
        .checked_sub(fixed_metadata_blocks)
        .ok_or_else(|| Ext4Error::no_space().with_operation("mkfs:last_group_metadata"))?;
    let inodes_per_block = block_size / u32::from(inode_size);
    let density_target = blocks_per_group / 4;
    let capacity_inodes = u32::try_from(inode_table_capacity)
        .map_err(|_| Ext4Error::overflow())?
        .checked_mul(inodes_per_block)
        .ok_or_else(Ext4Error::overflow)?;
    // A single-group image keeps the historical density target: bootstrap
    // objects and the internal journal need more space than the bare inode
    // table fit calculation captures. Multi-group images may lower density to
    // make a partial final group structurally valid.
    let inodes_per_group = if groups == 1 {
        density_target
    } else {
        density_target.min(capacity_inodes)
    };
    if inodes_per_group < RESERVED_INODES {
        return Err(Ext4Error::no_space().with_operation("mkfs:inode_table"));
    }

    // Each group stores a full inode table contiguous to its bitmaps.
    let inode_table_blocks: u32 = if block_size == 0 {
        0
    } else {
        (inodes_per_group * inode_size as u32).div_ceil(block_size)
    };

    // ext4 uses `s_first_data_block = 0` for block sizes above 1 KiB, and `1`
    // for 1 KiB filesystems.
    // Reserve extra GDT space for potential future resize support.
    let reserved_gdt_blocks: u32 = RESERVED_GDT_BLOCKS;

    // Group 0 hosts the primary superblock and primary GDT, so its bitmaps and
    // inode table start after the reserved GDT area.
    let group0_start: u32 = first_data_block;
    let reserved_gdt_start = group0_start
        .checked_add(1)
        .and_then(|block| block.checked_add(gdt_blocks))
        .ok_or_else(Ext4Error::overflow)?;
    let group0_block_bitmap = reserved_gdt_start
        .checked_add(reserved_gdt_blocks)
        .ok_or_else(Ext4Error::overflow)?;
    let group0_inode_bitmap = group0_block_bitmap
        .checked_add(1)
        .ok_or_else(Ext4Error::overflow)?;
    let group0_inode_table = group0_inode_bitmap
        .checked_add(1)
        .ok_or_else(Ext4Error::overflow)?;
    let group0_metadata_end = group0_inode_table
        .checked_add(inode_table_blocks)
        .ok_or_else(Ext4Error::overflow)?;
    let group0_metadata_blocks = group0_metadata_end
        .checked_sub(group0_start)
        .ok_or_else(Ext4Error::overflow)?;

    // Reserve roughly 5% of blocks for privileged recovery space.
    let reserved_blocks: u64 = total_blocks / 20;

    let group0_blocks = data_blocks.min(u64::from(blocks_per_group)) as u32;
    if group0_metadata_blocks > group0_blocks {
        return Err(Ext4Error::no_space().with_operation("mkfs:group0_metadata"));
    }

    Ok(FsLayoutInfo {
        block_size,
        total_blocks,
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
    })
}

fn validate_mkfs_geometry(inode_size: u16, block_size: u32) -> Ext4Result<()> {
    if !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size)
        || !block_size.is_power_of_two()
        || inode_size < GOOD_OLD_INODE_SIZE
        || !inode_size.is_power_of_two()
        || u32::from(inode_size) > block_size
    {
        return Err(Ext4Error::bad_superblock().with_operation("mkfs:geometry"));
    }
    Ok(())
}

fn group_blocks_count(layout: &FsLayoutInfo, group_id: u32) -> u32 {
    let group_start = u64::from(layout.first_data_block)
        + u64::from(group_id) * u64::from(layout.blocks_per_group);
    if group_start >= layout.total_blocks {
        return 0;
    }

    let remaining = layout.total_blocks - group_start;
    remaining.min(u64::from(layout.blocks_per_group)) as u32
}

fn group_free_blocks(layout: &FsLayoutInfo, group_id: u32, metadata_blocks: u32) -> u32 {
    group_blocks_count(layout, group_id).saturating_sub(metadata_blocks)
}

fn mark_bitmap_range_allocated(bitmap: &mut [u8], start: u32, end: u32) {
    let bits = (bitmap.len() * 8) as u32;
    let end = end.min(bits);
    for bit in start.min(bits)..end {
        let byte_idx = (bit / 8) as usize;
        let bit_idx = bit % 8;
        bitmap[byte_idx] |= 1 << bit_idx;
    }
}

fn mark_block_bitmap_padding(bitmap: &mut [u8], layout: &FsLayoutInfo, group_id: u32) {
    let valid_blocks = group_blocks_count(layout, group_id);
    mark_bitmap_range_allocated(bitmap, valid_blocks, layout.blocks_per_group);
}

/// Geometry selected when creating a new ext4 filesystem.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MkfsOptions {
    /// Filesystem block size in bytes.
    pub block_size: u32,
    /// On-disk inode size in bytes.
    pub inode_size: u16,
}

impl Default for MkfsOptions {
    fn default() -> Self {
        Self {
            block_size: BLOCK_SIZE_U32,
            inode_size: DEFAULT_INODE_SIZE,
        }
    }
}

pub fn mkfs<B: BlockIo>(block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
    mkfs_with_options(block_dev, MkfsOptions::default())
}

/// Creates a fresh ext4 filesystem using the requested on-disk geometry.
///
/// # Errors
///
/// Returns an error when the geometry is unsupported, the device is too small,
/// or any metadata write, bootstrap mount, or durability operation fails.
pub fn mkfs_with_options<B: BlockIo>(
    block_dev: &mut Jbd2Dev<B>,
    options: MkfsOptions,
) -> Ext4Result<()> {
    validate_mkfs_geometry(options.inode_size, options.block_size)?;

    let previous_block_size = block_dev.block_size() as usize;
    block_dev.set_filesystem_block_size(options.block_size as usize)?;
    let total_blocks = block_dev.total_blocks();
    let layout = match compute_fs_layout(options.inode_size, total_blocks, options.block_size) {
        Ok(layout) => layout,
        Err(error) => {
            if previous_block_size != options.block_size as usize {
                block_dev.set_filesystem_block_size(previous_block_size)?;
            }
            return Err(error);
        }
    };

    let old_journal_use = block_dev.is_use_journal();
    // Disable journaling while laying out the initial filesystem image. The
    // journal inode and journal superblock do not exist yet at this stage.
    let result = (|| {
        block_dev.set_journal_use(false)?;
        let total_groups = layout.groups;

        // Write the primary superblock and any sparse backups first so every later
        // descriptor/bitmap write can assume a valid superblock image exists.
        let superblock = build_superblock(total_blocks, &layout);
        write_superblock(block_dev, &superblock)?;

        write_superblock_redundant_backup(block_dev, &superblock, total_groups, &layout)?;

        let mut descs: VecDeque<Ext4GroupDesc> = VecDeque::new();
        // Seed all group descriptors before initializing individual bitmaps.
        for group_id in 0..total_groups {
            let mut desc = build_uninit_group_desc(&superblock, group_id, &layout);
            write_group_desc(block_dev, group_id, &mut desc)?;
            descs.push_back(desc);
        }
        write_gdt_redundant_backup(block_dev, &descs, &superblock, total_groups, &layout)?;

        // Group 0 is initialized eagerly because mkfs immediately creates the root
        // directory inside it.
        initialize_group_0(block_dev, &layout)?;

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

        // Reuse the private mkfs bootstrap mount to create the journal, root,
        // and lost+found. Ordinary mounts never synthesize a missing journal.
        {
            let mut fs = Ext4FileSystem::mount_for_mkfs(block_dev)?;
            fs.umount(block_dev)?;
        }

        // Final sanity check: read back the superblock and validate the magic.
        let verify_sb = read_superblock(block_dev)?;

        if verify_sb.s_magic == EXT4_SUPER_MAGIC {
            Ok(())
        } else {
            Err(Ext4Error::corrupted())
        }
    })();
    let restore_result = block_dev.set_journal_use(old_journal_use);
    match result {
        Err(error) => Err(error),
        Ok(()) => restore_result,
    }
}

/// Builds the in-memory superblock used by mkfs.
fn build_superblock(total_blocks: u64, layout: &FsLayoutInfo) -> Ext4Superblock {
    let mut sb = Ext4Superblock {
        s_magic: EXT4_SUPER_MAGIC,
        s_blocks_count_lo: (total_blocks & 0xFFFFFFFF) as u32,
        s_blocks_count_hi: (total_blocks >> 32) as u32,
        s_log_block_size: layout.block_size.trailing_zeros() - 10,
        s_log_cluster_size: layout.block_size.trailing_zeros() - 10,
        s_blocks_per_group: layout.blocks_per_group,
        s_inodes_per_group: layout.inodes_per_group,
        s_clusters_per_group: layout.blocks_per_group,
        s_inodes_count: layout.groups * layout.inodes_per_group,
        s_inode_size: layout.inode_size,
        s_first_ino: RESERVED_INODES + 1,
        s_first_data_block: layout.first_data_block,
        s_r_blocks_count_lo: (layout.reserved_blocks & 0xFFFFFFFF) as u32,
        s_r_blocks_count_hi: (layout.reserved_blocks >> 32) as u32,
        s_feature_compat: DEFAULT_FEATURE_COMPAT,
        s_feature_incompat: DEFAULT_FEATURE_INCOMPAT,
        s_feature_ro_compat: DEFAULT_FEATURE_RO_COMPAT,
        ..Default::default()
    };

    // Seed the directory hash machinery and UUID fields up front so every
    // later checksum uses the final superblock identity.
    let uuid = generate_uuid();
    sb.s_hash_seed = uuid.0;

    let filesys_uuid = generate_uuid_8();
    sb.s_uuid = filesys_uuid;

    // Reserved blocks remain part of the filesystem's free-block count; the
    // reserved count only limits which callers may consume them. Derive the
    // global count from the same per-group layouts published in the GDT so a
    // partial final group cannot make the two accounting sources disagree.
    let free_blocks = (0..layout.groups)
        .map(|group_id| {
            u64::from(build_uninit_group_desc(&sb, group_id, layout).free_blocks_count())
        })
        .sum::<u64>();
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
    let gl = calc_group_layout(
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
    desc.bg_block_bitmap_lo = gl.group_block_bitmap_start_block as u32;
    desc.bg_inode_bitmap_lo = gl.group_inode_bitmap_start_block as u32;
    desc.bg_inode_table_lo = gl.group_inode_table_start_block as u32;

    // Free-block count is based on the group's real capacity. The last block
    // group is often partial, so blocks past s_blocks_count must never be
    // reported as free.
    let free_blocks = group_free_blocks(layout, group_id, gl.metadata_blocks_in_group);

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
fn write_superblock_redundant_backup<B: BlockIo>(
    block_dev: &mut Jbd2Dev<B>,
    sb: &Ext4Superblock,
    groups_count: u32,
    fs_layout: &FsLayoutInfo,
) -> Ext4Result<()> {
    // Group 0 already holds the primary copy, so backup writing starts from 1.
    let sparse_feature =
        sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER);
    if sparse_feature {
        for gid in 1..groups_count {
            let group_layout = calc_group_layout(
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
                block_dev.update_block(AbsoluteBN::new(super_blocks), true, |buffer| {
                    sb.to_disk_bytes(&mut buffer[0..SUPERBLOCK_SIZE]);
                    Ok(())
                })?;
            }
        }
    }
    Ok(())
}

/// Writes the primary superblock to disk.
pub(crate) fn write_superblock<B: BlockIo>(
    block_dev: &mut Jbd2Dev<B>,
    sb: &Ext4Superblock,
) -> Ext4Result<()> {
    let block_size = block_dev.block_size() as usize;
    let byte_offset = Ext4Superblock::SUPERBLOCK_OFFSET as usize;
    let block = AbsoluteBN::new((byte_offset / block_size) as u64);
    let in_block = byte_offset % block_size;
    let end = in_block
        .checked_add(Ext4Superblock::SUPERBLOCK_SIZE)
        .ok_or_else(Ext4Error::overflow)?;
    if end > block_size {
        return Err(Ext4Error::bad_superblock().with_operation("superblock:crosses_block"));
    }

    block_dev.update_block(block, true, |buffer| {
        sb.to_disk_bytes(&mut buffer[in_block..end]);
        Ok(())
    })
}

/// Reads the primary superblock from disk.
pub(crate) fn read_superblock<B: BlockIo>(
    block_dev: &mut Jbd2Dev<B>,
) -> Ext4Result<Ext4Superblock> {
    let mut bytes = [0; SUPERBLOCK_SIZE];
    block_dev.read_device_bytes(SUPERBLOCK_OFFSET, &mut bytes)?;
    let superblock = Ext4Superblock::from_disk_bytes(&bytes);
    let block_size = superblock.checked_block_size()?;
    block_dev.set_filesystem_block_size(block_size as usize)?;
    Ok(superblock)
}

/// Writes redundant GDT copies to sparse-super backup groups.
fn write_gdt_redundant_backup<B: BlockIo>(
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

    let sparse_feature =
        sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER);
    if sparse_feature {
        for gid in 1..groups_count {
            if need_redundant_backup(gid) {
                let group_layout = calc_group_layout(
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
                for gdt_block_id in gdt_start..group_layout.group_block_bitmap_start_block {
                    block_dev.update_block(AbsoluteBN::new(gdt_block_id), true, |buffer| {
                        let mut current_offset = 0_usize;
                        for _ in 0..fs_layout.descs_per_block {
                            if let Some(desc) = desc_iter.next() {
                                desc.to_disk_bytes(
                                    &mut buffer
                                        [current_offset..current_offset + desc_size as usize],
                                );
                                current_offset += desc_size as usize;
                            }
                        }
                        Ok(())
                    })?;
                }
            }
        }
    }

    Ok(())
}

/// Writes one group descriptor into the primary GDT.
fn write_group_desc<B: BlockIo>(
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
    let gdt_base = superblock.primary_gdt_byte_offset()?;
    let byte_offset = gdt_base + group_id as u64 * desc_size as u64;
    let block_size_u64 = u64::from(superblock.checked_block_size()?);
    let block_num = byte_offset / block_size_u64;
    let in_block = (byte_offset % block_size_u64) as usize;
    let end = in_block + desc_size;

    let inode_bitmap_blk = desc.inode_bitmap() as u32;
    block_dev.read_block(inode_bitmap_blk.into())?;
    let inode_bitmap_bytes = block_dev.buffer().to_vec();
    let block_bitmap_blk = desc.block_bitmap() as u32;
    block_dev.read_block(block_bitmap_blk.into())?;
    let block_bitmap_bytes = block_dev.buffer().to_vec();
    block_dev.update_block(AbsoluteBN::new(block_num), true, |buffer| {
        if end > buffer.len() {
            return Err(Ext4Error::corrupted());
        }
        desc.encode_with_checksum(
            &superblock,
            group_id,
            &mut buffer[in_block..end],
            Some(&block_bitmap_bytes),
            Some(&inode_bitmap_bytes),
        )
    })
}

/// Initializes group 0 bitmaps, inode table, and descriptor state.
fn initialize_group_0<B: BlockIo>(
    block_dev: &mut Jbd2Dev<B>,
    layout: &FsLayoutInfo,
) -> Ext4Result<()> {
    // Group 0 has a fixed layout derived during mkfs planning.
    let block_bitmap_blk = layout.group0_block_bitmap;
    let inode_bitmap_blk = layout.group0_inode_bitmap;
    let inode_table_blk = layout.group0_inode_table;

    let mut block_bitmap = vec![0; block_dev.block_size() as usize];
    // Mark all group-0 metadata blocks and out-of-filesystem padding bits
    // allocated in the block bitmap.
    mark_bitmap_range_allocated(&mut block_bitmap, 0, layout.group0_metadata_blocks);
    mark_block_bitmap_padding(&mut block_bitmap, layout, 0);
    block_dev.write_blocks(&block_bitmap, block_bitmap_blk.into(), 1, true)?;

    let mut inode_bitmap = vec![0; block_dev.block_size() as usize];
    {
        // Mark reserved inodes allocated.
        for i in 0..RESERVED_INODES {
            let byte_idx = (i / 8) as usize;
            let bit_idx = i % 8;
            inode_bitmap[byte_idx] |= 1 << bit_idx;
        }

        // Mark bitmap padding bits allocated so they are never handed out.
        let bits_per_group = layout.block_size * 8;
        for i in layout.inodes_per_group..bits_per_group {
            let byte_idx: usize = (i / 8) as usize;
            let bit_idx = i % 8;
            inode_bitmap[byte_idx] |= 1 << bit_idx;
        }
    }
    block_dev.write_blocks(&inode_bitmap, inode_bitmap_blk.into(), 1, true)?;

    // Zero the inode table before the filesystem is mounted for the first time.
    let zero_block = vec![0; block_dev.block_size() as usize];
    for i in 0..layout.inode_table_blocks {
        block_dev.write_blocks(&zero_block, (inode_table_blk + i).into(), 1, true)?;
    }

    // Persist the now-initialized descriptor for group 0.
    let mut desc = Ext4GroupDesc {
        bg_flags: Ext4GroupDesc::EXT4_BG_INODE_ZEROED,
        bg_free_blocks_count_lo: group_free_blocks(layout, 0, layout.group0_metadata_blocks) as u16,
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
fn initialize_other_groups_bitmaps<B: BlockIo>(
    block_dev: &mut Jbd2Dev<B>,
    layout: &FsLayoutInfo,
    sb: &Ext4Superblock,
) -> Ext4Result<()> {
    // Group 0 has already been handled separately.
    for group_id in 1..layout.groups {
        // Reuse the same layout calculation as descriptor construction.
        let gl = calc_group_layout(
            group_id,
            sb,
            layout.blocks_per_group,
            layout.inode_table_blocks,
            layout.group0_block_bitmap,
            layout.group0_inode_bitmap,
            layout.group0_inode_table,
            layout.gdt_blocks,
        );

        let block_bitmap_blk = gl.group_block_bitmap_start_block as u32;
        let inode_bitmap_blk = gl.group_inode_bitmap_start_block as u32;

        // Start with a zeroed block bitmap, then mark metadata blocks used.
        let mut block_bitmap = vec![0; block_dev.block_size() as usize];
        mark_bitmap_range_allocated(&mut block_bitmap, 0, gl.metadata_blocks_in_group);
        mark_block_bitmap_padding(&mut block_bitmap, layout, group_id);
        block_dev.write_blocks(&block_bitmap, block_bitmap_blk.into(), 1, true)?;

        let mut inode_bitmap = vec![0; block_dev.block_size() as usize];
        {
            // Start with all inodes free, then mask the trailing padding bits.
            let bits_per_group = layout.block_size * 8;
            for i in layout.inodes_per_group..bits_per_group {
                let byte_idx: usize = (i / 8) as usize;
                let bit_idx = i % 8;
                inode_bitmap[byte_idx] |= 1 << bit_idx;
            }
        }
        block_dev.write_blocks(&inode_bitmap, inode_bitmap_blk.into(), 1, true)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TinyDevice;

    impl BlockIo for TinyDevice {
        fn read(
            &mut self,
            _buffer: &mut [u8],
            _sector: crate::SectorId,
            _count: u32,
        ) -> Ext4Result<()> {
            Err(Ext4Error::io())
        }

        fn write(
            &mut self,
            _buffer: &[u8],
            _sector: crate::SectorId,
            _count: u32,
        ) -> Ext4Result<()> {
            Err(Ext4Error::io())
        }

        fn geometry(&self) -> crate::DeviceGeometry {
            crate::DeviceGeometry::new(512, 20)
        }

        fn capabilities(&self) -> crate::DeviceCapabilities {
            crate::DeviceCapabilities::default()
        }

        fn flush(&mut self) -> Ext4Result<()> {
            Ok(())
        }
    }

    impl crate::Clock for TinyDevice {
        fn now(&self) -> Ext4Result<Ext4Timestamp> {
            Ok(Ext4Timestamp::new(0, 0))
        }
    }

    #[test]
    fn group_zero_bitmaps_follow_the_complete_primary_gdt() {
        let blocks_per_group = 8 * BLOCK_SIZE_U32;
        let descs_per_block = BLOCK_SIZE_U32 / u32::from(GROUP_DESC_SIZE);
        let total_blocks = u64::from(blocks_per_group) * u64::from(descs_per_block + 1);

        let layout = compute_fs_layout(DEFAULT_INODE_SIZE, total_blocks, BLOCK_SIZE_U32)
            .expect("multi-block GDT layout must be representable");

        assert!(layout.gdt_blocks > 1);
        assert_eq!(
            layout.group0_block_bitmap,
            layout.first_data_block + 1 + layout.gdt_blocks + layout.reserved_gdt_blocks
        );
    }

    #[test]
    fn partial_group_zero_must_fit_all_mkfs_metadata() {
        let result = compute_fs_layout(DEFAULT_INODE_SIZE, 10, BLOCK_SIZE_U32);

        assert!(matches!(
            result,
            Err(error) if error.kind() == crate::error::Ext4ErrorKind::NoSpace
        ));
    }

    #[test]
    fn partial_last_group_must_fit_its_inode_table() {
        let blocks_per_group = 8 * BLOCK_SIZE_U32;
        let total_blocks = u64::from(blocks_per_group) + 128;
        let layout = compute_fs_layout(DEFAULT_INODE_SIZE, total_blocks, BLOCK_SIZE_U32)
            .expect("partial final group should have a representable layout");
        let superblock = build_superblock(total_blocks, &layout);
        let last_group = layout.groups - 1;
        let group = calc_group_layout(
            last_group,
            &superblock,
            layout.blocks_per_group,
            layout.inode_table_blocks,
            layout.group0_block_bitmap,
            layout.group0_inode_bitmap,
            layout.group0_inode_table,
            layout.gdt_blocks,
        );

        assert!(
            group.group_inode_table_start_block + u64::from(layout.inode_table_blocks)
                <= total_blocks
        );
    }

    #[test]
    fn superblock_free_blocks_match_group_descriptors() {
        let blocks_per_group = 8 * BLOCK_SIZE_U32;
        let total_blocks = u64::from(blocks_per_group) + 128;
        let layout = compute_fs_layout(DEFAULT_INODE_SIZE, total_blocks, BLOCK_SIZE_U32)
            .expect("partial final group should have a representable layout");
        let superblock = build_superblock(total_blocks, &layout);
        let descriptor_free_blocks: u64 = (0..layout.groups)
            .map(|group_id| {
                u64::from(
                    build_uninit_group_desc(&superblock, group_id, &layout).free_blocks_count(),
                )
            })
            .sum();

        assert_eq!(superblock.free_blocks_count(), descriptor_free_blocks);
    }

    #[test]
    fn rejected_mkfs_layout_restores_previous_block_geometry() {
        let mut device = Jbd2Dev::initial_jbd2dev(0, TinyDevice, false);

        let error = mkfs_with_options(
            &mut device,
            MkfsOptions {
                block_size: 1024,
                ..MkfsOptions::default()
            },
        )
        .expect_err("ten 1 KiB blocks cannot hold group-zero metadata");

        assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);
        assert_eq!(device.block_size(), BLOCK_SIZE_U32);
    }
}
