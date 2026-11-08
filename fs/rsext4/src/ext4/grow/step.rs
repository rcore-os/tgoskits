//! Preparation, durable commit and publication of one ext4 growth step.

use super::*;
use crate::ext4::system_zone::{group_first_block, group_has_superblock, inode_table_blocks};

/// Caps unpublished metadata memory and initialization work per durable batch.
/// Shared per-batch preflight and the actual image count bound its credits.
const MAX_BATCH_GROUPS: u64 = 256;

/// Bounds the two newly initialized bitmap images per group. Descriptor and
/// resize-inode images retain the separately preflighted layout/credit bounds.
const MAX_BATCH_BITMAP_BYTES: u64 = 2 * 1024 * 1024;

/// Selects the next geometry for both preflight and publication. `target` is
/// in filesystem blocks; `credits` is the journal's metadata-block limit.
/// Extends a published partial tail alone, otherwise bounds bitmap memory and
/// group count, then halves the candidate until its complete journal budget
/// fits. A one-group candidate that cannot fit is unsupported, without writes.
pub(super) fn next_geometry(
    sb: &Ext4Superblock,
    target: u64,
    credits: usize,
) -> Ext4Result<Ext4Superblock> {
    let per_group = u64::from(sb.s_blocks_per_group);
    let group = (sb.blocks_count() - u64::from(sb.s_first_data_block)) / per_group;
    let old_gdt = sb
        .checked_block_groups_count()?
        .div_ceil(sb.descs_per_block());
    let mut groups = if group < u64::from(sb.checked_block_groups_count()?) {
        1
    } else {
        MAX_BATCH_GROUPS.min(MAX_BATCH_BITMAP_BYTES / (2 * sb.block_size()))
    };
    if groups == 0 {
        return Err(Ext4Error::unsupported().with_operation("grow:bitmap_budget"));
    }
    let start = group_first_block(sb, u32::try_from(group).map_err(|_| Ext4Error::overflow())?)?;
    loop {
        let end = start
            .checked_add(
                per_group
                    .checked_mul(groups)
                    .ok_or_else(Ext4Error::overflow)?,
            )
            .ok_or_else(Ext4Error::overflow)?
            .min(target);
        let mut next = *sb;
        set_blocks(&mut next, end);
        let new_gdt = next
            .checked_block_groups_count()?
            .div_ceil(sb.descs_per_block());
        next.s_reserved_gdt_blocks = sb
            .s_reserved_gdt_blocks
            .checked_sub(u16::try_from(new_gdt - old_gdt).map_err(|_| Ext4Error::overflow())?)
            .ok_or_else(Ext4Error::unsupported)?;
        if transaction_credits(sb, &next)? <= credits {
            return Ok(next);
        }
        if groups == 1 {
            return Err(Ext4Error::unsupported().with_operation("grow:transaction_capacity"));
        }
        groups /= 2;
    }
}

/// Counts only old-geometry blocks modified by this batch, without reading or
/// changing metadata. Primary/old backup superblocks and changed GDT blocks,
/// the partial-tail bitmap and conditional inode-7 edits own distinct credits.
/// New local metadata is initialized outside the journal transaction.
pub(super) fn transaction_credits(old: &Ext4Superblock, new: &Ext4Superblock) -> Ext4Result<usize> {
    let old_groups = old.checked_block_groups_count()?;
    let groups = new.checked_block_groups_count()?;
    let first = u32::try_from(
        (old.blocks_count() - u64::from(old.s_first_data_block))
            / u64::from(old.s_blocks_per_group),
    )
    .map_err(|_| Ext4Error::overflow())?;
    let old_gdt = old_groups.div_ceil(old.descs_per_block());
    let new_gdt = groups.div_ceil(old.descs_per_block());
    let changed_gdt = new_gdt - first / old.descs_per_block();
    let copies = (0..old_groups)
        .filter(|group| group_has_superblock(old, *group))
        .count();
    let mut credits = copies
        .checked_mul(changed_gdt as usize + 1)
        .ok_or_else(Ext4Error::overflow)?;
    let added_backup = (old_groups..groups).any(|group| group_has_superblock(old, group));
    let inode_credits = if old.s_reserved_gdt_blocks != 0 && (new_gdt != old_gdt || added_backup) {
        1 + usize::from(new_gdt != old_gdt)
            + if added_backup {
                usize::from(new.s_reserved_gdt_blocks)
            } else {
                0
            }
    } else {
        0
    };
    credits = credits
        .checked_add(usize::from(first < old_groups))
        .and_then(|count| count.checked_add(inode_credits))
        .ok_or_else(Ext4Error::overflow)?;
    Ok(credits)
}

/// Candidate state, detached from the mounted allocator and descriptor authority.
pub(super) struct GrowthStep {
    /// Candidate geometry and counts, published only after durable completion.
    superblock: Ext4Superblock,
    /// Full candidate group records, retaining preexisting metadata locations.
    descriptors: Vec<Ext4GroupDesc>,
    /// Complete metadata block images, split by old/new geometry when committed.
    images: Images,
    /// New inode-table extents as (absolute filesystem block, block count).
    new_inode_tables: Vec<(AbsoluteBN, u64)>,
}

/// Sets or clears bitmap bits in the half-open range `first..end` according to
/// `allocated`. Callers provide a bitmap large enough for that bit range.
fn mark(bitmap: &mut [u8], first: usize, end: usize, allocated: bool) {
    for bit in first..end {
        if allocated {
            bitmap[bit / 8] |= 1 << (bit % 8);
        } else {
            bitmap[bit / 8] &= !(1 << (bit % 8));
        }
    }
}

impl Ext4FileSystem {
    /// Extends the current partial group or adds a bounded batch of new groups. All
    /// metadata edits remain private until `GrowthStep::publish` commits them.
    /// `target` is the preflighted final filesystem block count; `device` must
    /// be the same exclusive journal view as this mount. Read, checksum, range
    /// or layout failures return without publishing the candidate.
    pub(super) fn prepare_growth_step<B: BlockIo>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        target: u64,
    ) -> Ext4Result<GrowthStep> {
        let old = self.superblock;
        let old_blocks = old.blocks_count();
        let per_group = u64::from(old.s_blocks_per_group);
        let group = (old_blocks - u64::from(old.s_first_data_block)) / per_group;
        let first_group = u32::try_from(group).map_err(|_| Ext4Error::overflow())?;
        let mut superblock = next_geometry(
            &old,
            target,
            device
                .transaction_credit_limit()?
                .ok_or_else(Ext4Error::unsupported)?,
        )?;
        let end = superblock.blocks_count();
        let groups = superblock.checked_block_groups_count()?;
        let old_gdt = self.group_count.div_ceil(old.descs_per_block());
        let new_gdt = groups.div_ceil(old.descs_per_block());
        superblock.s_inodes_count = groups
            .checked_mul(old.s_inodes_per_group)
            .ok_or_else(Ext4Error::overflow)?;
        let reserve = (u128::from(old.reserved_blocks_count()) * u128::from(end)
            / u128::from(old_blocks)) as u64;
        superblock.s_r_blocks_count_lo = reserve as u32;
        superblock.s_r_blocks_count_hi = (reserve >> 32) as u32;
        // Zero requests recomputation instead of persisting a stale cached overhead.
        superblock.s_overhead_blocks = 0;
        let mut descriptors = self.group_descs.clone();
        let mut images = Images::new();
        let mut new_inode_tables = Vec::new();
        let block_size = self.block_size();
        // New groups use local bitmap and inode-table blocks even when old
        // groups use flex_bg placement; existing metadata locations stay fixed.
        for group in first_group..groups {
            let start = group_first_block(&old, group)?;
            let group_end = start
                .checked_add(per_group)
                .ok_or_else(Ext4Error::overflow)?
                .min(end);
            let valid = (group_end - start) as usize;
            if group >= self.group_count {
                let base = if group_has_superblock(&superblock, group) {
                    1 + u64::from(new_gdt) + u64::from(superblock.s_reserved_gdt_blocks)
                } else {
                    0
                };
                let table_blocks = inode_table_blocks(&superblock)?;
                let metadata = base + 2 + table_blocks;
                if metadata > group_end - start {
                    return Err(Ext4Error::unsupported().with_operation("grow:small_group"));
                }
                let block_bitmap = start + base;
                let inode_bitmap = block_bitmap + 1;
                let inode_table = inode_bitmap + 1;
                let mut bitmap = vec![0; block_size];
                mark(&mut bitmap, 0, metadata as usize, true);
                mark(&mut bitmap, valid, block_size * 8, true);
                let mut inodes = vec![0; block_size];
                mark(
                    &mut inodes,
                    old.s_inodes_per_group as usize,
                    block_size * 8,
                    true,
                );
                let free = u32::try_from(group_end - start - metadata)
                    .map_err(|_| Ext4Error::overflow())?;
                // Lazy inode accounting is only defined when group descriptors are
                // checksummed. Legacy layouts require a zero unused-inode count,
                // even though we eagerly zero their inode tables as well.
                let checksummed = ext4_superblock_has_metadata_csum(&superblock)
                    || superblock
                        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_GDT_CSUM);
                let unused_inodes = if checksummed {
                    old.s_inodes_per_group
                } else {
                    0
                };
                let mut descriptor = Ext4GroupDesc {
                    bg_block_bitmap_lo: block_bitmap as u32,
                    bg_block_bitmap_hi: (block_bitmap >> 32) as u32,
                    bg_inode_bitmap_lo: inode_bitmap as u32,
                    bg_inode_bitmap_hi: (inode_bitmap >> 32) as u32,
                    bg_inode_table_lo: inode_table as u32,
                    bg_inode_table_hi: (inode_table >> 32) as u32,
                    bg_free_blocks_count_lo: free as u16,
                    bg_free_blocks_count_hi: (free >> 16) as u16,
                    bg_free_inodes_count_lo: old.s_inodes_per_group as u16,
                    bg_free_inodes_count_hi: (old.s_inodes_per_group >> 16) as u16,
                    bg_itable_unused_lo: unused_inodes as u16,
                    bg_itable_unused_hi: (unused_inodes >> 16) as u16,
                    bg_flags: Ext4GroupDesc::EXT4_BG_INODE_ZEROED,
                    ..Default::default()
                };
                descriptor.update_bitmap_checksums(&superblock, Some(&bitmap), Some(&inodes));
                descriptors.push(descriptor);
                images.insert(AbsoluteBN::new(block_bitmap), bitmap);
                images.insert(AbsoluteBN::new(inode_bitmap), inodes);
                new_inode_tables.push((AbsoluteBN::new(inode_table), table_blocks));
                if base != 0 {
                    for index in new_gdt..new_gdt + u32::from(superblock.s_reserved_gdt_blocks) {
                        images.insert(
                            AbsoluteBN::new(start + 1 + u64::from(index)),
                            vec![0; block_size],
                        );
                    }
                }
            } else {
                let descriptor = descriptors
                    .get_mut(group as usize)
                    .ok_or_else(Ext4Error::corrupted)?;
                let bitmap = image(&mut images, device, descriptor.block_bitmap())?;
                if descriptor.is_block_bitmap_uninit() {
                    self.system_zones
                        .initialize_group_block_bitmap(&old, group, bitmap)?;
                    descriptor.bg_flags &= !Ext4GroupDesc::EXT4_BG_BLOCK_UNINIT;
                } else if ext4_superblock_has_metadata_csum(&old) {
                    let checksum = ext4_block_bitmap_csum32(&old, bitmap);
                    if !descriptor.block_bitmap_csum_matches(&old, checksum) {
                        return Err(
                            Ext4Error::checksum().with_operation("grow:block_bitmap_checksum")
                        );
                    }
                }
                // Only newly addressable blocks become free. Bitmap padding beyond
                // the new end stays allocated and must never reach the allocator.
                mark(bitmap, (old_blocks - start) as usize, valid, false);
                mark(bitmap, valid, block_size * 8, true);
                let free = descriptor
                    .free_blocks_count()
                    .checked_add(
                        u32::try_from(group_end - old_blocks).map_err(|_| Ext4Error::overflow())?,
                    )
                    .ok_or_else(Ext4Error::overflow)?;
                descriptor.bg_free_blocks_count_lo = free as u16;
                descriptor.bg_free_blocks_count_hi = (free >> 16) as u16;
                descriptor.update_bitmap_checksums(&superblock, Some(bitmap), None);
            }
        }
        let free_blocks: u64 = descriptors
            .iter()
            .map(|descriptor| u64::from(descriptor.free_blocks_count()))
            .sum();
        superblock.s_free_blocks_count_lo = free_blocks as u32;
        superblock.s_free_blocks_count_hi = (free_blocks >> 32) as u32;
        superblock.s_free_inodes_count =
            descriptors.iter().try_fold(0u32, |count, descriptor| {
                count
                    .checked_add(descriptor.free_inodes_count())
                    .ok_or_else(Ext4Error::overflow)
            })?;
        resize_inode::update(self, device, &superblock, groups, &mut images)?;
        // Only this batch's descriptors change. Their checksums depend on the
        // fixed UUID/seed, group number and descriptor bytes, not capacity.
        // Preserve their full extension bytes and all other descriptor blocks.
        let gdt_start = u64::from(old.s_first_data_block) + 1;
        let first_gdt = first_group / old.descs_per_block();
        for index in old_gdt..new_gdt {
            images.insert(
                AbsoluteBN::new(gdt_start + u64::from(index)),
                vec![0; block_size],
            );
        }
        let size = old.get_desc_size() as usize;
        for group in first_group..groups {
            let block = gdt_start + u64::from(group / old.descs_per_block());
            let slot = (group % old.descs_per_block()) as usize * size;
            let descriptor = descriptors
                .get_mut(group as usize)
                .ok_or_else(Ext4Error::corrupted)?;
            let raw = &mut image(&mut images, device, block)?[slot..slot + size];
            descriptor.encode_with_checksum(&superblock, group, raw, None, None)?;
        }
        for backup_group in 0..groups {
            if !group_has_superblock(&superblock, backup_group) {
                continue;
            }
            let base = group_first_block(&superblock, backup_group)?;
            let mut backup = superblock;
            backup.s_block_group_nr = backup_group.min(u32::from(u16::MAX)) as u16;
            backup.update_checksum();
            let (block, offset) = if backup_group == 0 {
                (1024 / block_size as u64, 1024 % block_size)
            } else {
                (base, 0)
            };
            let destination = image(&mut images, device, block)?;
            backup.to_disk_bytes(&mut destination[offset..offset + SUPERBLOCK_SIZE]);
            if backup_group != 0 {
                // Existing backups need only the changed GDT block, including
                // a newly consumed reserve block. A new backup needs the whole
                // table, but reading unchanged primary blocks must not stage
                // redundant primary writes in this transaction.
                let indexes = if backup_group >= self.group_count {
                    0..new_gdt
                } else {
                    first_gdt..new_gdt
                };
                for index in indexes {
                    let primary = AbsoluteBN::new(gdt_start + u64::from(index));
                    let raw = if let Some(bytes) = images.get(&primary) {
                        bytes.clone()
                    } else {
                        device.read_block(primary)?;
                        device.buffer().to_vec()
                    };
                    images.insert(AbsoluteBN::new(base + 1 + u64::from(index)), raw);
                }
            }
        }
        superblock.update_checksum();
        // Validate candidate geometry without modifying the mounted authority.
        SystemZoneMap::from_layout(&superblock, &descriptors)?;
        Ok(GrowthStep {
            superblock,
            descriptors,
            images,
            new_inode_tables,
        })
    }
}

impl GrowthStep {
    /// Initializes blocks outside the old geometry first, journals changes to
    /// existing blocks together, then replaces the mount's in-memory authority.
    /// Consumes this step; `fs` and `device` must still describe the source
    /// geometry used during preparation. Commit errors propagate even if some
    /// writes reached disk, so the failed mount must not be reused.
    /// All inode-table extents are submitted together so the device can batch
    /// discontiguous writes and recycle its own completed DMA buffers.
    pub(super) fn publish<B: BlockIo>(
        self,
        fs: &mut Ext4FileSystem,
        device: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
        let old_blocks = fs.superblock.blocks_count();
        let credits = self
            .images
            .keys()
            .filter(|block| block.raw() < old_blocks)
            .count();
        if credits > transaction_credits(&fs.superblock, &self.superblock)? {
            return Err(Ext4Error::corrupted().with_operation("grow:step_credit_accounting"));
        }
        if credits
            > device
                .transaction_credit_limit()?
                .ok_or_else(Ext4Error::unsupported)?
        {
            return Err(Ext4Error::unsupported().with_operation("grow:step_transaction_capacity"));
        }
        if self
            .new_inode_tables
            .iter()
            .any(|(start, _)| start.raw() < old_blocks)
        {
            return Err(Ext4Error::corrupted().with_operation("grow:published_inode_table"));
        }
        device.zero_unpublished_ranges(&self.new_inode_tables)?;
        // Unpublished data is safe to initialize repeatedly after a crash.
        for (block, bytes) in &self.images {
            if block.raw() >= old_blocks {
                device.write_unpublished_blocks(bytes, *block, 1)?;
            }
        }
        // JBD2's payload preflush also persists all preceding initialization
        // writes on this endpoint before its commit record becomes durable.
        // A separate flush here would checkpoint earlier growth unnecessarily.
        device.with_transaction_credits(credits.into(), |device| {
            for (block, bytes) in &self.images {
                if block.raw() < old_blocks {
                    device.update_block(*block, true, |output| {
                        output.copy_from_slice(bytes);
                        Ok(())
                    })?;
                }
            }
            Ok(())
        })?;
        // The durable log owns these images until space reclamation or the
        // final checkpoint. Later batches read their newest journal overlay;
        // no allocator or external inode reader is published during growth.
        device.commit()?;
        fs.superblock = self.superblock;
        fs.group_count = fs.superblock.checked_block_groups_count()?;
        fs.group_descs = self.descriptors;
        // Resize-inode edits bypass the inode cache. The next batch must load
        // its newly committed record, not a stale i_blocks/checksum snapshot.
        fs.inodetable_cache.clear();
        // Allocation, dirty-descriptor tracking and journal protection remain
        // private until grow_before_publish rebuilds them once at the end.
        Ok(())
    }
}
