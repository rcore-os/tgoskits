//! Preservation of inode 7's reserved primary and backup GDT references.
//!
//! Its double-indirect block indexes reserved primary GDT blocks by the GDT
//! index modulo pointers-per-block. Each reserved primary block in turn lists
//! the corresponding blocks in backup groups, in ascending group order.

use super::*;
use crate::ext4::system_zone::{group_first_block, group_has_superblock};

/// Reads little-endian pointer slot `index` from `bytes`; indexes count u32
/// entries, not bytes. A truncated slot is treated as corrupt metadata.
fn get_u32(bytes: &[u8], index: usize) -> Ext4Result<u32> {
    let bytes = bytes
        .get(index * 4..index * 4 + 4)
        .ok_or_else(Ext4Error::corrupted)?;
    Ok(u32::from_le_bytes(
        bytes.try_into().map_err(|_| Ext4Error::corrupted())?,
    ))
}

/// Stores filesystem block address `value` into u32 pointer slot `index` in
/// `bytes`, rejecting unrepresentable addresses and truncated records.
fn put_u32(bytes: &mut [u8], index: usize, value: u64) -> Ext4Result<()> {
    let value = u32::try_from(value).map_err(|_| Ext4Error::unsupported())?;
    bytes
        .get_mut(index * 4..index * 4 + 4)
        .ok_or_else(Ext4Error::corrupted)?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}

/// Proves inode 7's allocation, checksum and complete reserved-reference tree
/// before growth is allowed to repurpose any of those blocks as a live GDT.
/// `fs` and `device` describe the same mounted source; `layout` is the complete
/// preflighted target. Reads only, with no work for unchanged geometry or a
/// filesystem without reserved GDT blocks. Invalid ownership is a hard error.
pub(super) fn validate<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    layout: &GrowthLayout,
) -> Ext4Result<()> {
    let sb = fs.superblock;
    if layout.blocks <= sb.blocks_count() {
        return Ok(());
    }
    let reserve = u32::from(sb.s_reserved_gdt_blocks);
    if reserve == 0 {
        return Ok(());
    }
    if !sb.has_feature_compat(Ext4Superblock::EXT4_FEATURE_COMPAT_RESIZE_INODE) {
        return Err(Ext4Error::unsupported().with_operation("grow:resize_inode_missing"));
    }
    let number = InodeNumber::new(7)?;
    let (inode, raw) = fs.get_inode_record(device, number)?;
    verify_raw_inode_checksum(&sb, number, &inode, &raw)?;
    if !inode.is_file()
        || inode.uses_extents()
        || inode.i_block[13] == 0
        || inode
            .i_block
            .iter()
            .enumerate()
            .any(|(index, block)| index != 13 && *block != 0)
    {
        return Err(Ext4Error::corrupted().with_operation("grow:resize_inode_shape"));
    }
    let double = u64::from(inode.i_block[13]);
    if double < u64::from(sb.s_first_data_block)
        || double >= sb.blocks_count()
        || double >= device.total_blocks()
        || !fs.system_zones.allows_range(double, 1, number)
        || !fs.inode_is_allocated_checked(device, number)?
    {
        return Err(Ext4Error::corrupted().with_operation("grow:resize_inode_ownership"));
    }
    let group = (double - u64::from(sb.s_first_data_block)) / u64::from(sb.s_blocks_per_group);
    let relative = (double - u64::from(sb.s_first_data_block)) % u64::from(sb.s_blocks_per_group);
    let descriptor = fs
        .group_descs
        .get(group as usize)
        .ok_or_else(Ext4Error::corrupted)?;
    if descriptor.is_block_bitmap_uninit() {
        return Err(Ext4Error::corrupted().with_operation("grow:resize_inode_unallocated"));
    }
    device.read_block(AbsoluteBN::new(descriptor.block_bitmap()))?;
    let bitmap = device.buffer();
    if bitmap[(relative / 8) as usize] & (1 << (relative % 8)) == 0
        || (ext4_superblock_has_metadata_csum(&sb)
            && !descriptor.block_bitmap_csum_matches(&sb, ext4_block_bitmap_csum32(&sb, bitmap)))
    {
        return Err(Ext4Error::corrupted().with_operation("grow:resize_inode_unallocated"));
    }
    let gdt = fs.group_count.div_ceil(sb.descs_per_block());
    let pointers = fs.block_size() / 4;
    if reserve as usize > pointers || layout.gdt_blocks > pointers as u32 + gdt {
        return Err(Ext4Error::unsupported().with_operation("grow:resize_inode_capacity"));
    }
    device.read_block(AbsoluteBN::new(u64::from(inode.i_block[13])))?;
    let indirect = device.buffer().to_vec();
    for slot in 0..pointers {
        let expected = (gdt..gdt + reserve).any(|index| index as usize % pointers == slot);
        if !expected && get_u32(&indirect, slot)? != 0 {
            return Err(
                Ext4Error::corrupted().with_operation("grow:resize_inode_unexpected_pointer")
            );
        }
    }
    let backups: Vec<_> = (1..fs.group_count)
        .filter(|group| group_has_superblock(&sb, *group))
        .collect();
    let future_backups = (1..layout.groups)
        .filter(|group| group_has_superblock(&sb, *group))
        .count();
    if future_backups > pointers {
        return Err(Ext4Error::unsupported().with_operation("grow:backup_index_capacity"));
    }
    // i_blocks counts 512-byte sectors, including the double-indirect block,
    // every reserved primary block and all of their backup blocks.
    let expected_blocks = 1 + u64::from(reserve) * (backups.len() as u64 + 1);
    if inode.blocks_count(sb.checked_block_size()?, true)
        != expected_blocks * fs.block_size() as u64 / 512
    {
        return Err(Ext4Error::corrupted().with_operation("grow:resize_inode_blocks"));
    }
    for index in gdt..gdt + reserve {
        let primary = u64::from(sb.s_first_data_block) + 1 + u64::from(index);
        if u64::from(get_u32(&indirect, index as usize % pointers)?) != primary {
            return Err(Ext4Error::corrupted().with_operation("grow:reserved_primary_pointer"));
        }
        device.read_block(AbsoluteBN::new(primary))?;
        for (slot, group) in backups.iter().copied().enumerate() {
            let expected = group_first_block(&sb, group)? + 1 + u64::from(index);
            if u64::from(get_u32(device.buffer(), slot)?) != expected {
                return Err(Ext4Error::corrupted().with_operation("grow:reserved_backup_pointer"));
            }
        }
        if device.buffer()[backups.len() * 4..]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(Ext4Error::corrupted().with_operation("grow:reserved_backup_tail"));
        }
    }
    // Inode 7 pointers are 32-bit even on a 64-bit filesystem.
    if group_first_block(&sb, layout.groups - 1)? + 1 + u64::from(gdt + reserve)
        > u64::from(u32::MAX)
    {
        return Err(Ext4Error::unsupported().with_operation("grow:resize_inode_32bit_pointer"));
    }
    Ok(())
}

/// Stages reference removal/addition and inode accounting in the same image
/// set as the superblock/GDT change; no on-disk ownership changes happen here.
/// `fs`/`device` describe the source, while `candidate`/`groups` describe one
/// prepared step. Edits are merged into `images`, including the raw inode record
/// and its checksum; callers commit the complete set together.
pub(super) fn update<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    candidate: &Ext4Superblock,
    groups: u32,
    images: &mut Images,
) -> Ext4Result<()> {
    let old = fs.superblock;
    if old.s_reserved_gdt_blocks == 0 {
        return Ok(());
    }
    let old_gdt = fs.group_count.div_ceil(old.descs_per_block());
    let new_gdt = groups.div_ceil(old.descs_per_block());
    let added_backup = (fs.group_count..groups).any(|group| group_has_superblock(&old, group));
    if old_gdt == new_gdt && !added_backup {
        return Ok(());
    }
    let number = InodeNumber::new(7)?;
    let (mut inode, mut raw) = fs.get_inode_record(device, number)?;
    let pointers = fs.block_size() / 4;
    let double = u64::from(inode.i_block[13]);
    // Consumed blocks become live descriptors. Remove inode 7's reference in
    // the same transaction that decreases s_reserved_gdt_blocks.
    for index in old_gdt..new_gdt {
        put_u32(image(images, device, double)?, index as usize % pointers, 0)?;
    }
    let backups: Vec<_> = (1..groups)
        .filter(|group| group_has_superblock(candidate, *group))
        .collect();
    // Every new sparse-super group adds a pointer at its final ordered slot.
    // A batch may add several backups while consuming multiple GDT reserves;
    // only the remaining reserves retain inode 7 ownership.
    for (slot, group) in backups.iter().copied().enumerate() {
        if group < fs.group_count {
            continue;
        }
        let base = group_first_block(candidate, group)?;
        for index in new_gdt..new_gdt + u32::from(candidate.s_reserved_gdt_blocks) {
            let primary = u64::from(old.s_first_data_block) + 1 + u64::from(index);
            put_u32(
                image(images, device, primary)?,
                slot,
                base + 1 + u64::from(index),
            )?;
        }
    }
    let sectors = (1 + u64::from(candidate.s_reserved_gdt_blocks) * (backups.len() as u64 + 1))
        * fs.block_size() as u64
        / 512;
    inode.set_blocks_count(sectors, old.checked_block_size()?, true)?;
    ext4_update_raw_inode_checksum(candidate, number, &mut inode, &mut raw)?;
    let (block, offset) = fs.inode_table_location(number)?;
    image(images, device, block.raw())?[offset..offset + raw.len()].copy_from_slice(&raw);
    Ok(())
}
