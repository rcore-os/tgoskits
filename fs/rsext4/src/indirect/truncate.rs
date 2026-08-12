//! Legacy direct/indirect truncate planning and publication.

use alloc::vec::Vec;
use core::mem::size_of;

use super::{
    DIRECT_BLOCKS, DOUBLE_INDIRECT_SLOT, LegacyBlockReader, LegacyInodeOwnership,
    SINGLE_INDIRECT_SLOT, TRIPLE_INDIRECT_SLOT, collect_legacy_inode_ownership, is_fast_symlink,
};
use crate::{
    BlockIo, Ext4FileSystem, Jbd2Dev,
    blockdev::TransactionCredits,
    bmalloc::{AbsoluteBN, BGIndex, InodeNumber},
    disknode::Ext4Inode,
    endian::{read_u32_le, write_u32_le},
    error::{Ext4Error, Ext4Result},
};

#[derive(Debug)]
struct LegacyPointerBlockEdit {
    block: AbsoluteBN,
    previous: Vec<u32>,
    updated: Vec<u32>,
}

/// A fully validated set of legacy mapping changes for one truncate operation.
///
/// Planning never mutates allocation or inode state. This separation lets the
/// caller publish the new inode image before returning blocks to the allocator,
/// and it leaves an explicit rollback point for pointer-block write failures.
pub(crate) struct LegacyTruncatePlan {
    updated_inode_pointers: [u32; 15],
    pointer_edits: Vec<LegacyPointerBlockEdit>,
    data_blocks_to_free: Vec<AbsoluteBN>,
    metadata_blocks_to_free: Vec<AbsoluteBN>,
    remaining_allocated_blocks: u64,
}

pub(crate) struct LegacyTransactionFootprint {
    pub(crate) allocation_groups: Vec<BGIndex>,
    pub(crate) credits: TransactionCredits,
}

impl LegacyTruncatePlan {
    pub(crate) fn has_removals(&self) -> bool {
        !self.pointer_edits.is_empty()
            || !self.data_blocks_to_free.is_empty()
            || !self.metadata_blocks_to_free.is_empty()
    }

    pub(crate) fn apply_pointer_edits<B: BlockIo>(
        &self,
        device: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
        for (applied, edit) in self.pointer_edits.iter().enumerate() {
            if let Err(operation_error) = write_checked_pointer_block(
                device,
                edit.block,
                &edit.previous,
                &edit.updated,
                "indirect:truncate_pointer_changed",
            ) {
                let rollback = restore_pointer_edits(device, &self.pointer_edits[..applied]);
                return Err(match rollback {
                    Ok(()) => operation_error,
                    Err(rollback_error) => {
                        rollback_error.with_operation("rollback:indirect_truncate_pointer")
                    }
                });
            }
        }
        Ok(())
    }

    pub(crate) fn apply_inode_mapping(
        &self,
        inode: &mut Ext4Inode,
        block_size: u32,
        huge_file_feature: bool,
    ) -> Ext4Result<()> {
        let sectors = self
            .remaining_allocated_blocks
            .checked_mul(u64::from(block_size / 512))
            .ok_or_else(Ext4Error::overflow)?;
        inode.set_blocks_count(sectors, block_size, huge_file_feature)?;
        inode.i_block = self.updated_inode_pointers;
        Ok(())
    }

    pub(crate) fn free_removed_blocks<B: BlockIo>(
        &self,
        filesystem: &mut Ext4FileSystem,
        device: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
        for &block in &self.data_blocks_to_free {
            filesystem.datablock_cache.invalidate(block);
            filesystem.free_block(device, block)?;
        }
        for &block in &self.metadata_blocks_to_free {
            device.forget_detached_metadata(block)?;
            filesystem.datablock_cache.invalidate(block);
            filesystem.free_block(device, block)?;
        }
        Ok(())
    }

    pub(crate) fn allocation_groups(
        &self,
        filesystem: &Ext4FileSystem,
    ) -> Ext4Result<Vec<BGIndex>> {
        let mut groups = Vec::new();
        for &block in self
            .data_blocks_to_free
            .iter()
            .chain(&self.metadata_blocks_to_free)
        {
            let (group, _) = filesystem.block_allocator.global_to_group(block)?;
            if !groups.contains(&group) {
                groups.push(group);
            }
        }
        groups.sort_unstable();
        Ok(groups)
    }

    pub(crate) fn transaction_footprint(
        &self,
        filesystem: &Ext4FileSystem,
    ) -> Ext4Result<LegacyTransactionFootprint> {
        let allocation_groups = self.allocation_groups(filesystem)?;
        let metadata_credits = self
            .pointer_edits
            .len()
            .checked_add(
                allocation_groups
                    .len()
                    .checked_mul(2)
                    .ok_or_else(Ext4Error::overflow)?,
            )
            .and_then(|credits| credits.checked_add(2))
            .ok_or_else(Ext4Error::overflow)?;
        Ok(LegacyTransactionFootprint {
            allocation_groups,
            credits: TransactionCredits::metadata_with_revokes(
                metadata_credits,
                self.metadata_blocks_to_free.len(),
            ),
        })
    }
}

/// Validates the complete legacy tree and plans removal of every mapping at or
/// beyond `first_free_logical`, including mappings hidden beyond `i_size`.
pub(crate) fn plan_legacy_inode_truncate<B: BlockIo>(
    filesystem: &Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    inode_number: InodeNumber,
    inode: &Ext4Inode,
    first_free_logical: u64,
) -> Ext4Result<LegacyTruncatePlan> {
    plan_legacy_inode_range_removal(
        filesystem,
        device,
        inode_number,
        inode,
        first_free_logical,
        u64::MAX,
    )
}

/// Validates the complete legacy tree and plans removal of mappings inside
/// the finite half-open logical range `start..end`.
pub(crate) fn plan_legacy_inode_range_removal<B: BlockIo>(
    filesystem: &Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    inode_number: InodeNumber,
    inode: &Ext4Inode,
    start: u64,
    end: u64,
) -> Ext4Result<LegacyTruncatePlan> {
    if inode.uses_extents() || is_fast_symlink(filesystem, inode) {
        return Err(Ext4Error::unsupported().with_operation("indirect:truncate_format"));
    }
    let removal = LogicalRemovalRange::new(start, end)?;

    // This full ownership pass is intentionally separate from range planning:
    // corruption in hidden siblings, cycles, or duplicate physical ownership
    // must abort before any pointer or bitmap is changed.
    let ownership = collect_legacy_inode_ownership(filesystem, device, inode_number, inode)?;
    let allocated_blocks = allocated_blocks(&ownership)?;
    let mut plan = LegacyTruncatePlan {
        updated_inode_pointers: inode.i_block,
        pointer_edits: Vec::new(),
        data_blocks_to_free: Vec::new(),
        metadata_blocks_to_free: Vec::new(),
        remaining_allocated_blocks: 0,
    };

    plan_direct_blocks(inode, removal, &mut plan);
    LegacyTruncatePlanner::new(filesystem, device, inode_number, removal, &mut plan)?
        .plan_indirect_roots()?;
    plan.remaining_allocated_blocks = allocated_blocks
        .checked_sub(blocks_to_free(&plan)?)
        .ok_or_else(|| Ext4Error::corrupted().with_operation("indirect:truncate_accounting"))?;
    Ok(plan)
}

fn allocated_blocks(ownership: &LegacyInodeOwnership) -> Ext4Result<u64> {
    let data = u64::try_from(ownership.data_blocks.len()).map_err(|_| Ext4Error::overflow())?;
    let metadata =
        u64::try_from(ownership.metadata_blocks.len()).map_err(|_| Ext4Error::overflow())?;
    data.checked_add(metadata).ok_or_else(Ext4Error::overflow)
}

fn blocks_to_free(plan: &LegacyTruncatePlan) -> Ext4Result<u64> {
    let data = u64::try_from(plan.data_blocks_to_free.len()).map_err(|_| Ext4Error::overflow())?;
    let metadata =
        u64::try_from(plan.metadata_blocks_to_free.len()).map_err(|_| Ext4Error::overflow())?;
    data.checked_add(metadata).ok_or_else(Ext4Error::overflow)
}

fn plan_direct_blocks(
    inode: &Ext4Inode,
    removal: LogicalRemovalRange,
    plan: &mut LegacyTruncatePlan,
) {
    for slot in (0..DIRECT_BLOCKS).rev() {
        let pointer = inode.i_block[slot];
        if pointer != 0 && removal.contains(slot as u64) {
            plan.data_blocks_to_free.push(AbsoluteBN::from(pointer));
            plan.updated_inode_pointers[slot] = 0;
        }
    }
}

#[derive(Clone, Copy)]
struct LogicalRemovalRange {
    start: u64,
    end: u64,
}

impl LogicalRemovalRange {
    fn new(start: u64, end: u64) -> Ext4Result<Self> {
        if start > end {
            return Err(Ext4Error::invalid_input().with_operation("indirect:removal_range"));
        }
        Ok(Self { start, end })
    }

    const fn contains(self, logical: u64) -> bool {
        self.start <= logical && logical < self.end
    }

    const fn intersects(self, start: u64, end: u64) -> bool {
        self.start < end && start < self.end
    }
}

#[derive(Clone, Copy)]
struct LegacyIndirectRoot {
    slot: usize,
    depth: usize,
    logical_base: u64,
    stride: u64,
}

struct LegacyTruncatePlanner<'fs, 'dev, 'plan, B: BlockIo> {
    reader: LegacyBlockReader<'fs, 'dev, B>,
    removal: LogicalRemovalRange,
    plan: &'plan mut LegacyTruncatePlan,
}

impl<'fs, 'dev, 'plan, B: BlockIo> LegacyTruncatePlanner<'fs, 'dev, 'plan, B> {
    fn new(
        filesystem: &'fs Ext4FileSystem,
        device: &'dev mut Jbd2Dev<B>,
        inode_number: InodeNumber,
        removal: LogicalRemovalRange,
        plan: &'plan mut LegacyTruncatePlan,
    ) -> Ext4Result<Self> {
        Ok(Self {
            reader: LegacyBlockReader::new(filesystem, device, inode_number)?,
            removal,
            plan,
        })
    }

    fn plan_indirect_roots(&mut self) -> Ext4Result<()> {
        let pointers = self.reader.pointers_per_block;
        let double_capacity = pointers
            .checked_mul(pointers)
            .ok_or_else(Ext4Error::overflow)?;
        for root in [
            LegacyIndirectRoot {
                slot: SINGLE_INDIRECT_SLOT,
                depth: 1,
                logical_base: DIRECT_BLOCKS as u64,
                stride: 1,
            },
            LegacyIndirectRoot {
                slot: DOUBLE_INDIRECT_SLOT,
                depth: 2,
                logical_base: DIRECT_BLOCKS as u64 + pointers,
                stride: pointers,
            },
            LegacyIndirectRoot {
                slot: TRIPLE_INDIRECT_SLOT,
                depth: 3,
                logical_base: DIRECT_BLOCKS as u64 + pointers + double_capacity,
                stride: double_capacity,
            },
        ] {
            self.plan_indirect_root(root)?;
        }
        Ok(())
    }

    fn plan_indirect_root(&mut self, root: LegacyIndirectRoot) -> Ext4Result<()> {
        let pointer = self.plan.updated_inode_pointers[root.slot];
        if pointer == 0 {
            return Ok(());
        }
        let logical_end = root
            .logical_base
            .checked_add(
                root.stride
                    .checked_mul(self.reader.pointers_per_block)
                    .ok_or_else(Ext4Error::overflow)?,
            )
            .ok_or_else(Ext4Error::overflow)?;
        if !self.removal.intersects(root.logical_base, logical_end) {
            return Ok(());
        }

        let root_block = AbsoluteBN::from(pointer);
        if self.plan_truncate_subtree(root_block, root.depth, root.logical_base, root.stride)? {
            self.plan.metadata_blocks_to_free.push(root_block);
            self.plan.updated_inode_pointers[root.slot] = 0;
        }
        Ok(())
    }

    fn plan_truncate_subtree(
        &mut self,
        metadata: AbsoluteBN,
        depth: usize,
        logical_base: u64,
        stride: u64,
    ) -> Ext4Result<bool> {
        self.reader.enter_metadata_block(metadata)?;
        let previous = self.reader.read_pointer_block(metadata)?;
        let mut updated = previous.clone();
        let next_stride = if depth > 1 {
            Some(
                stride
                    .checked_div(self.reader.pointers_per_block)
                    .ok_or_else(Ext4Error::overflow)?,
            )
        } else {
            None
        };

        for index in (0..updated.len()).rev() {
            let pointer = updated[index];
            if pointer == 0 {
                continue;
            }
            let logical = logical_base
                .checked_add(
                    u64::try_from(index)
                        .map_err(|_| Ext4Error::overflow())?
                        .checked_mul(stride)
                        .ok_or_else(Ext4Error::overflow)?,
                )
                .ok_or_else(Ext4Error::overflow)?;
            let physical = AbsoluteBN::from(pointer);
            if depth == 1 {
                if self.removal.contains(logical) {
                    self.plan.data_blocks_to_free.push(physical);
                    updated[index] = 0;
                }
                continue;
            }

            let child_end = logical
                .checked_add(stride)
                .ok_or_else(Ext4Error::overflow)?;
            if !self.removal.intersects(logical, child_end) {
                continue;
            }

            let child_empty = self.plan_truncate_subtree(
                physical,
                depth - 1,
                logical,
                next_stride.ok_or_else(Ext4Error::overflow)?,
            )?;
            if child_empty {
                self.plan.metadata_blocks_to_free.push(physical);
                updated[index] = 0;
            }
        }
        self.reader.metadata_path.pop();

        let empty = updated.iter().all(|&pointer| pointer == 0);
        if !empty && updated != previous {
            self.plan.pointer_edits.push(LegacyPointerBlockEdit {
                block: metadata,
                previous,
                updated,
            });
        }
        Ok(empty)
    }
}

fn write_checked_pointer_block<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    block: AbsoluteBN,
    expected: &[u32],
    replacement: &[u32],
    changed_operation: &'static str,
) -> Ext4Result<()> {
    device.read_block(block)?;
    let current: Vec<u32> = device
        .buffer()
        .as_chunks::<{ size_of::<u32>() }>()
        .0
        .iter()
        .map(|bytes| read_u32_le(bytes))
        .collect();
    if current != expected {
        return Err(Ext4Error::corrupted().with_operation(changed_operation));
    }
    device.update_block(block, true, |buffer| {
        for (bytes, &pointer) in buffer
            .as_chunks_mut::<{ size_of::<u32>() }>()
            .0
            .iter_mut()
            .zip(replacement)
        {
            write_u32_le(pointer, bytes);
        }
        Ok(())
    })
}

fn restore_pointer_edits<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    edits: &[LegacyPointerBlockEdit],
) -> Ext4Result<()> {
    for edit in edits.iter().rev() {
        write_checked_pointer_block(
            device,
            edit.block,
            &edit.updated,
            &edit.previous,
            "indirect:restore_truncate_pointer_changed",
        )?;
    }
    Ok(())
}
