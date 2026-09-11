//! Checked legacy direct and indirect block mapping.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};
use core::mem::size_of;

use crate::{
    BlockIo, Ext4FileSystem, Jbd2Dev,
    bmalloc::{AbsoluteBN, InodeNumber},
    disknode::Ext4Inode,
    endian::{read_u32_le, write_u32_le},
    error::{Ext4Error, Ext4Result},
    superblock::Ext4Superblock,
};

const DIRECT_BLOCKS: usize = 12;
const SINGLE_INDIRECT_SLOT: usize = 12;
const DOUBLE_INDIRECT_SLOT: usize = 13;
const TRIPLE_INDIRECT_SLOT: usize = 14;

mod truncate;

pub(crate) use truncate::{
    LegacyTransactionFootprint, LegacyTruncatePlan, plan_legacy_inode_range_removal,
    plan_legacy_inode_truncate,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IndirectPath {
    offsets: [usize; 4],
    depth: usize,
}

impl IndirectPath {
    fn direct(offset: usize) -> Self {
        Self {
            offsets: [offset, 0, 0, 0],
            depth: 1,
        }
    }

    fn nested(offsets: &[usize]) -> Self {
        let mut path = Self {
            offsets: [0; 4],
            depth: offsets.len(),
        };
        path.offsets[..offsets.len()].copy_from_slice(offsets);
        path
    }
}

/// Resolves one logical block through the ext2/ext3 direct/indirect layout.
pub(crate) fn resolve_legacy_inode_block<B: BlockIo>(
    filesystem: &Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    inode_number: InodeNumber,
    inode: &Ext4Inode,
    logical_block: u32,
) -> Ext4Result<Option<AbsoluteBN>> {
    if is_fast_symlink(filesystem, inode) {
        return Ok(None);
    }

    let path = block_to_path(filesystem.block_size(), logical_block)?;
    let mut reader = LegacyBlockReader::new(filesystem, device, inode_number)?;
    reader.resolve_path(inode, path)
}

pub(crate) fn validate_legacy_block_count(block_size: usize, blocks: u64) -> Ext4Result<()> {
    if blocks == 0 {
        return Ok(());
    }
    let last = u32::try_from(blocks - 1).map_err(|_| Ext4Error::file_too_large())?;
    block_to_path(block_size, last).map(|_| ())
}

/// Materializes all data mappings reachable below the inode's file size.
pub(crate) fn resolve_legacy_inode_blocks<B: BlockIo>(
    filesystem: &Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    inode_number: InodeNumber,
    inode: &Ext4Inode,
) -> Ext4Result<BTreeMap<u32, AbsoluteBN>> {
    let inode_size = filesystem.inode_size(inode);
    if inode_size == 0 || is_fast_symlink(filesystem, inode) {
        return Ok(BTreeMap::new());
    }

    let block_size = filesystem.block_size();
    let logical_blocks = inode_size.div_ceil(block_size as u64);
    let mut reader = LegacyBlockReader::new(filesystem, device, inode_number)?;
    if logical_blocks > reader.maximum_logical_blocks()? {
        return Err(Ext4Error::file_too_large().with_operation("indirect:file_size"));
    }
    reader.collect_inode_mappings(inode, logical_blocks)
}

/// Materializes every data mapping owned by a legacy inode, including blocks
/// beyond its published file size.
pub(crate) fn resolve_all_legacy_inode_blocks<B: BlockIo>(
    filesystem: &Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    inode_number: InodeNumber,
    inode: &Ext4Inode,
) -> Ext4Result<BTreeMap<u32, AbsoluteBN>> {
    if is_fast_symlink(filesystem, inode) {
        return Ok(BTreeMap::new());
    }

    let mut reader = LegacyBlockReader::new(filesystem, device, inode_number)?;
    let logical_limit = reader
        .maximum_logical_blocks()?
        .min(u64::from(u32::MAX) + 1);
    reader.collect_inode_mappings(inode, logical_limit)
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct LegacyInodeOwnership {
    data_blocks: Vec<AbsoluteBN>,
    metadata_blocks: Vec<AbsoluteBN>,
}

impl LegacyInodeOwnership {
    pub(crate) fn into_data_blocks(self) -> Vec<AbsoluteBN> {
        self.data_blocks
    }
}

/// Validates and collects every physical block owned by a legacy inode.
pub(crate) fn collect_legacy_inode_ownership<B: BlockIo>(
    filesystem: &Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    inode_number: InodeNumber,
    inode: &Ext4Inode,
) -> Ext4Result<LegacyInodeOwnership> {
    if inode.uses_extents() {
        return Err(Ext4Error::unsupported().with_operation("indirect:collect_extent_inode"));
    }
    let mut ownership = LegacyInodeOwnership {
        data_blocks: Vec::new(),
        metadata_blocks: Vec::new(),
    };
    if is_fast_symlink(filesystem, inode) {
        return Ok(ownership);
    }

    let mut reader = LegacyBlockReader::new(filesystem, device, inode_number)?;
    let mut claimed = BTreeSet::new();
    for &pointer in &inode.i_block[..DIRECT_BLOCKS] {
        if pointer != 0 {
            reader.collect_owned_data(AbsoluteBN::from(pointer), &mut ownership, &mut claimed)?;
        }
    }
    for (slot, depth) in [
        (SINGLE_INDIRECT_SLOT, 1),
        (DOUBLE_INDIRECT_SLOT, 2),
        (TRIPLE_INDIRECT_SLOT, 3),
    ] {
        let pointer = inode.i_block[slot];
        if pointer != 0 {
            reader.collect_owned_subtree(
                AbsoluteBN::from(pointer),
                depth,
                &mut ownership,
                &mut claimed,
            )?;
        }
    }
    Ok(ownership)
}

/// Returns whether a legacy inode owns indirect metadata blocks.
pub(crate) fn has_legacy_indirect_mapping(filesystem: &Ext4FileSystem, inode: &Ext4Inode) -> bool {
    !inode.uses_extents()
        && !is_fast_symlink(filesystem, inode)
        && inode.i_block[DIRECT_BLOCKS..]
            .iter()
            .any(|&block| block != 0)
}

#[derive(Clone, Copy, Debug)]
enum LegacyPointerOwner {
    Inode { slot: usize },
    Indirect { block: AbsoluteBN, index: usize },
}

#[derive(Debug)]
struct MissingLegacyBranch {
    owner: LegacyPointerOwner,
    metadata_offsets: Vec<usize>,
}

enum LegacyMappingState {
    Mapped(AbsoluteBN),
    Hole(MissingLegacyBranch),
}

struct LegacyAllocationRollback {
    owner: LegacyPointerOwner,
    published_pointer: u32,
    metadata_blocks: Vec<AbsoluteBN>,
    data_block: AbsoluteBN,
    previous_i_blocks_lo: u32,
    previous_i_blocks_high: u16,
    previous_huge_file: bool,
}

/// One existing or newly allocated legacy mapping.
pub(crate) struct LegacyBlockAllocation {
    physical: AbsoluteBN,
    rollback: Option<LegacyAllocationRollback>,
}

impl LegacyBlockAllocation {
    pub(crate) fn physical(&self) -> AbsoluteBN {
        self.physical
    }

    pub(crate) fn is_new(&self) -> bool {
        self.rollback.is_some()
    }

    /// Reverses an allocation that has not been committed by its inode update.
    pub(crate) fn rollback<B: BlockIo>(
        mut self,
        filesystem: &mut Ext4FileSystem,
        device: &mut Jbd2Dev<B>,
        inode_number: InodeNumber,
        inode: &mut Ext4Inode,
    ) -> Ext4Result<()> {
        let Some(rollback) = self.rollback.take() else {
            return Ok(());
        };

        match rollback.owner {
            LegacyPointerOwner::Inode { slot } => {
                if inode.i_block[slot] != rollback.published_pointer {
                    return Err(
                        Ext4Error::corrupted().with_operation("indirect:rollback_inode_pointer")
                    );
                }
                inode.i_block[slot] = 0;
            }
            LegacyPointerOwner::Indirect { block, index } => {
                let mut reader = LegacyBlockReader::new(filesystem, device, inode_number)?;
                reader.replace_pointer(block, index, rollback.published_pointer, 0)?;
            }
        }

        inode.i_blocks_lo = rollback.previous_i_blocks_lo;
        inode.l_i_blocks_high = rollback.previous_i_blocks_high;
        if rollback.previous_huge_file {
            inode.i_flags |= Ext4Inode::EXT4_HUGE_FILE_FL;
        } else {
            inode.i_flags &= !Ext4Inode::EXT4_HUGE_FILE_FL;
        }

        discard_unpublished_branch(
            filesystem,
            device,
            rollback.data_block,
            &rollback.metadata_blocks,
        )
    }
}

/// Allocates a complete missing legacy branch before publishing its first pointer.
pub(crate) fn allocate_legacy_inode_block<B: BlockIo>(
    filesystem: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    inode_number: InodeNumber,
    inode: &mut Ext4Inode,
    logical_block: u32,
) -> Ext4Result<LegacyBlockAllocation> {
    if inode.uses_extents() || is_fast_symlink(filesystem, inode) {
        return Err(Ext4Error::unsupported().with_operation("indirect:allocate_format"));
    }

    let path = block_to_path(filesystem.block_size(), logical_block)?;
    let state = {
        let mut reader = LegacyBlockReader::new(filesystem, device, inode_number)?;
        reader.mapping_state(inode, path)?
    };
    let missing = match state {
        LegacyMappingState::Mapped(physical) => {
            return Ok(LegacyBlockAllocation {
                physical,
                rollback: None,
            });
        }
        LegacyMappingState::Hole(missing) => missing,
    };

    let added_blocks = u64::try_from(missing.metadata_offsets.len())
        .map_err(|_| Ext4Error::overflow())?
        .checked_add(1)
        .ok_or_else(Ext4Error::overflow)?;
    let block_size = filesystem.block_size() as u32;
    let huge_file_feature = filesystem
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
    let mut updated_accounting = *inode;
    let added_sectors = added_blocks
        .checked_mul(u64::from(block_size / 512))
        .ok_or_else(Ext4Error::overflow)?;
    let updated_sectors = inode
        .blocks_count(block_size, huge_file_feature)
        .checked_add(added_sectors)
        .ok_or_else(Ext4Error::overflow)?;
    updated_accounting.set_blocks_count(updated_sectors, block_size, huge_file_feature)?;

    let mut metadata_blocks = Vec::with_capacity(missing.metadata_offsets.len());
    for _ in &missing.metadata_offsets {
        match filesystem.alloc_block(device) {
            Ok(block) => metadata_blocks.push(block),
            Err(operation_error) => {
                let cleanup = discard_unpublished_metadata(filesystem, device, &metadata_blocks);
                return Err(error_after_legacy_cleanup(operation_error, cleanup));
            }
        }
    }
    let data_block = match filesystem.alloc_block(device) {
        Ok(block) => block,
        Err(operation_error) => {
            let cleanup = discard_unpublished_metadata(filesystem, device, &metadata_blocks);
            return Err(error_after_legacy_cleanup(operation_error, cleanup));
        }
    };

    let previous_i_blocks_lo = inode.i_blocks_lo;
    let previous_i_blocks_high = inode.l_i_blocks_high;
    let previous_huge_file = inode.i_flags & Ext4Inode::EXT4_HUGE_FILE_FL != 0;
    let prepare_result = (|| {
        let mut child_pointer = data_block.to_u32()?;
        filesystem
            .datablock_cache
            .modify_new(device, data_block, |data| data.fill(0))?;

        for (&metadata, &offset) in metadata_blocks.iter().zip(&missing.metadata_offsets).rev() {
            let metadata_pointer = metadata.to_u32()?;
            let start = offset
                .checked_mul(size_of::<u32>())
                .ok_or_else(Ext4Error::overflow)?;
            let end = start
                .checked_add(size_of::<u32>())
                .ok_or_else(Ext4Error::overflow)?;
            device.update_block(metadata, true, |buffer| {
                buffer.fill(0);
                let target = buffer.get_mut(start..end).ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("indirect:branch_pointer_offset")
                })?;
                write_u32_le(child_pointer, target);
                Ok(())
            })?;
            child_pointer = metadata_pointer;
        }
        Ok(child_pointer)
    })();

    let published_pointer = match prepare_result {
        Ok(pointer) => pointer,
        Err(operation_error) => {
            let cleanup =
                discard_unpublished_branch(filesystem, device, data_block, &metadata_blocks);
            return Err(error_after_legacy_cleanup(operation_error, cleanup));
        }
    };

    match missing.owner {
        LegacyPointerOwner::Inode { slot } => inode.i_block[slot] = published_pointer,
        LegacyPointerOwner::Indirect { block, index } => {
            let publish_result = {
                let mut reader = LegacyBlockReader::new(filesystem, device, inode_number)?;
                reader.replace_pointer(block, index, 0, published_pointer)
            };
            if let Err(operation_error) = publish_result {
                let restore_result = {
                    let mut reader = LegacyBlockReader::new(filesystem, device, inode_number)?;
                    reader.restore_pointer(block, index, published_pointer, 0)
                };
                if let Err(restore_error) = restore_result {
                    // The parent may still reference the prepared branch. Do
                    // not free it unless restoring the old pointer succeeds.
                    return Err(restore_error.with_operation("rollback:indirect_publish_pointer"));
                }
                let cleanup =
                    discard_unpublished_branch(filesystem, device, data_block, &metadata_blocks);
                return Err(error_after_legacy_cleanup(operation_error, cleanup));
            }
        }
    }

    inode.i_blocks_lo = updated_accounting.i_blocks_lo;
    inode.l_i_blocks_high = updated_accounting.l_i_blocks_high;
    if updated_accounting.i_flags & Ext4Inode::EXT4_HUGE_FILE_FL != 0 {
        inode.i_flags |= Ext4Inode::EXT4_HUGE_FILE_FL;
    } else {
        inode.i_flags &= !Ext4Inode::EXT4_HUGE_FILE_FL;
    }

    Ok(LegacyBlockAllocation {
        physical: data_block,
        rollback: Some(LegacyAllocationRollback {
            owner: missing.owner,
            published_pointer,
            metadata_blocks,
            data_block,
            previous_i_blocks_lo,
            previous_i_blocks_high,
            previous_huge_file,
        }),
    })
}

fn discard_unpublished_branch<B: BlockIo>(
    filesystem: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    data_block: AbsoluteBN,
    metadata_blocks: &[AbsoluteBN],
) -> Ext4Result<()> {
    filesystem.datablock_cache.invalidate(data_block);
    let mut first_error = filesystem.free_block(device, data_block).err();
    if let Err(error) = discard_unpublished_metadata(filesystem, device, metadata_blocks)
        && first_error.is_none()
    {
        first_error = Some(error);
    }
    match first_error {
        Some(error) => Err(error.with_operation("rollback:indirect_branch")),
        None => Ok(()),
    }
}

fn discard_unpublished_metadata<B: BlockIo>(
    filesystem: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    metadata_blocks: &[AbsoluteBN],
) -> Ext4Result<()> {
    let mut first_error = None;
    for &metadata in metadata_blocks.iter().rev() {
        device.forget_unpublished_metadata(metadata);
        if let Err(error) = filesystem.free_block(device, metadata)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error.with_operation("rollback:indirect_metadata")),
        None => Ok(()),
    }
}

fn error_after_legacy_cleanup(operation_error: Ext4Error, cleanup: Ext4Result<()>) -> Ext4Error {
    match cleanup {
        Ok(()) => operation_error,
        Err(cleanup_error) => cleanup_error,
    }
}

struct LegacyBlockReader<'fs, 'dev, B: BlockIo> {
    filesystem: &'fs Ext4FileSystem,
    device: &'dev mut Jbd2Dev<B>,
    inode_number: InodeNumber,
    pointers_per_block: u64,
    metadata_path: Vec<AbsoluteBN>,
}

impl<'fs, 'dev, B: BlockIo> LegacyBlockReader<'fs, 'dev, B> {
    fn new(
        filesystem: &'fs Ext4FileSystem,
        device: &'dev mut Jbd2Dev<B>,
        inode_number: InodeNumber,
    ) -> Ext4Result<Self> {
        let block_size = filesystem.block_size();
        if block_size < size_of::<u32>() || !block_size.is_multiple_of(size_of::<u32>()) {
            return Err(Ext4Error::bad_superblock().with_operation("indirect:pointers_per_block"));
        }
        Ok(Self {
            filesystem,
            device,
            inode_number,
            pointers_per_block: (block_size / size_of::<u32>()) as u64,
            metadata_path: Vec::with_capacity(3),
        })
    }

    fn maximum_logical_blocks(&self) -> Ext4Result<u64> {
        let double = self
            .pointers_per_block
            .checked_mul(self.pointers_per_block)
            .ok_or_else(Ext4Error::overflow)?;
        let triple = double
            .checked_mul(self.pointers_per_block)
            .ok_or_else(Ext4Error::overflow)?;
        (DIRECT_BLOCKS as u64)
            .checked_add(self.pointers_per_block)
            .and_then(|blocks| blocks.checked_add(double))
            .and_then(|blocks| blocks.checked_add(triple))
            .ok_or_else(Ext4Error::overflow)
    }

    fn collect_inode_mappings(
        &mut self,
        inode: &Ext4Inode,
        logical_limit: u64,
    ) -> Ext4Result<BTreeMap<u32, AbsoluteBN>> {
        let mut mappings = BTreeMap::new();
        let direct_limit = logical_limit.min(DIRECT_BLOCKS as u64) as usize;
        for (logical, &pointer) in inode.i_block.iter().enumerate().take(direct_limit) {
            if pointer != 0 {
                let physical = AbsoluteBN::from(pointer);
                self.validate_data_block(physical)?;
                mappings.insert(logical as u32, physical);
            }
        }

        let double_capacity = self
            .pointers_per_block
            .checked_mul(self.pointers_per_block)
            .ok_or_else(Ext4Error::overflow)?;
        let roots = [
            (SINGLE_INDIRECT_SLOT, 1usize, DIRECT_BLOCKS as u64, 1u64),
            (
                DOUBLE_INDIRECT_SLOT,
                2,
                DIRECT_BLOCKS as u64 + self.pointers_per_block,
                self.pointers_per_block,
            ),
            (
                TRIPLE_INDIRECT_SLOT,
                3,
                DIRECT_BLOCKS as u64 + self.pointers_per_block + double_capacity,
                double_capacity,
            ),
        ];
        for (slot, depth, logical_base, stride) in roots {
            if logical_base >= logical_limit {
                break;
            }
            let pointer = inode.i_block[slot];
            if pointer != 0 {
                self.collect_subtree(
                    AbsoluteBN::from(pointer),
                    depth,
                    logical_base,
                    logical_limit,
                    stride,
                    &mut mappings,
                )?;
            }
        }
        Ok(mappings)
    }

    fn mapping_state(
        &mut self,
        inode: &Ext4Inode,
        path: IndirectPath,
    ) -> Ext4Result<LegacyMappingState> {
        let root_slot = path.offsets[0];
        let mut pointer = inode.i_block[root_slot];
        if pointer == 0 {
            return Ok(LegacyMappingState::Hole(MissingLegacyBranch {
                owner: LegacyPointerOwner::Inode { slot: root_slot },
                metadata_offsets: path.offsets[1..path.depth].to_vec(),
            }));
        }

        for path_index in 1..path.depth {
            let metadata = AbsoluteBN::from(pointer);
            self.enter_metadata_block(metadata)?;
            let index = path.offsets[path_index];
            let pointers = self.read_pointer_block(metadata)?;
            pointer = *pointers
                .get(index)
                .ok_or_else(|| Ext4Error::corrupted().with_operation("indirect:pointer_offset"))?;
            if pointer == 0 {
                return Ok(LegacyMappingState::Hole(MissingLegacyBranch {
                    owner: LegacyPointerOwner::Indirect {
                        block: metadata,
                        index,
                    },
                    metadata_offsets: path.offsets[path_index + 1..path.depth].to_vec(),
                }));
            }
        }

        let physical = AbsoluteBN::from(pointer);
        self.validate_data_block(physical)?;
        Ok(LegacyMappingState::Mapped(physical))
    }

    fn replace_pointer(
        &mut self,
        metadata: AbsoluteBN,
        index: usize,
        expected: u32,
        replacement: u32,
    ) -> Ext4Result<()> {
        self.enter_metadata_block(metadata)?;
        let pointers = self.read_pointer_block(metadata)?;
        if pointers.get(index).copied() != Some(expected) {
            return Err(Ext4Error::corrupted().with_operation("indirect:pointer_changed"));
        }
        let start = index
            .checked_mul(size_of::<u32>())
            .ok_or_else(Ext4Error::overflow)?;
        let end = start
            .checked_add(size_of::<u32>())
            .ok_or_else(Ext4Error::overflow)?;
        self.device.update_block(metadata, true, |buffer| {
            let target = buffer
                .get_mut(start..end)
                .ok_or_else(|| Ext4Error::corrupted().with_operation("indirect:pointer_offset"))?;
            write_u32_le(replacement, target);
            Ok(())
        })?;
        self.metadata_path.pop();
        Ok(())
    }

    fn restore_pointer(
        &mut self,
        metadata: AbsoluteBN,
        index: usize,
        published: u32,
        previous: u32,
    ) -> Ext4Result<()> {
        self.enter_metadata_block(metadata)?;
        let pointers = self.read_pointer_block(metadata)?;
        let current = pointers
            .get(index)
            .copied()
            .ok_or_else(|| Ext4Error::corrupted().with_operation("indirect:pointer_offset"))?;
        if current == previous {
            self.metadata_path.pop();
            return Ok(());
        }
        if current != published {
            return Err(Ext4Error::corrupted().with_operation("indirect:restore_pointer_changed"));
        }

        let start = index
            .checked_mul(size_of::<u32>())
            .ok_or_else(Ext4Error::overflow)?;
        let end = start
            .checked_add(size_of::<u32>())
            .ok_or_else(Ext4Error::overflow)?;
        self.device.update_block(metadata, true, |buffer| {
            let target = buffer
                .get_mut(start..end)
                .ok_or_else(|| Ext4Error::corrupted().with_operation("indirect:pointer_offset"))?;
            write_u32_le(previous, target);
            Ok(())
        })?;
        self.metadata_path.pop();
        Ok(())
    }

    fn resolve_path(
        &mut self,
        inode: &Ext4Inode,
        path: IndirectPath,
    ) -> Ext4Result<Option<AbsoluteBN>> {
        let mut pointer = inode.i_block[path.offsets[0]];
        if pointer == 0 {
            return Ok(None);
        }

        for &offset in &path.offsets[1..path.depth] {
            let metadata = AbsoluteBN::from(pointer);
            self.enter_metadata_block(metadata)?;
            pointer = self.read_pointer(metadata, offset)?;
            if pointer == 0 {
                return Ok(None);
            }
        }

        let data = AbsoluteBN::from(pointer);
        self.validate_data_block(data)?;
        Ok(Some(data))
    }

    fn collect_subtree(
        &mut self,
        metadata: AbsoluteBN,
        depth: usize,
        logical_base: u64,
        logical_limit: u64,
        stride: u64,
        mappings: &mut BTreeMap<u32, AbsoluteBN>,
    ) -> Ext4Result<()> {
        self.enter_metadata_block(metadata)?;
        let pointers = self.read_pointer_block(metadata)?;
        for (index, pointer) in pointers.into_iter().enumerate() {
            let logical = logical_base
                .checked_add(
                    u64::try_from(index)
                        .map_err(|_| Ext4Error::overflow())?
                        .checked_mul(stride)
                        .ok_or_else(Ext4Error::overflow)?,
                )
                .ok_or_else(Ext4Error::overflow)?;
            if logical >= logical_limit {
                break;
            }
            if pointer == 0 {
                continue;
            }

            let physical = AbsoluteBN::from(pointer);
            if depth == 1 {
                self.validate_data_block(physical)?;
                let logical = u32::try_from(logical).map_err(|_| Ext4Error::overflow())?;
                if mappings.insert(logical, physical).is_some() {
                    return Err(
                        Ext4Error::corrupted().with_operation("indirect:duplicate_logical_block")
                    );
                }
            } else {
                let next_stride = stride
                    .checked_div(self.pointers_per_block)
                    .ok_or_else(Ext4Error::overflow)?;
                self.collect_subtree(
                    physical,
                    depth - 1,
                    logical,
                    logical_limit,
                    next_stride,
                    mappings,
                )?;
            }
        }
        self.metadata_path.pop();
        Ok(())
    }

    fn collect_owned_subtree(
        &mut self,
        metadata: AbsoluteBN,
        depth: usize,
        ownership: &mut LegacyInodeOwnership,
        claimed: &mut BTreeSet<AbsoluteBN>,
    ) -> Ext4Result<()> {
        self.enter_metadata_block(metadata)?;
        Self::claim_owned_block(claimed, metadata)?;
        let pointers = self.read_pointer_block(metadata)?;
        for pointer in pointers {
            if pointer == 0 {
                continue;
            }
            let physical = AbsoluteBN::from(pointer);
            if depth == 1 {
                self.collect_owned_data(physical, ownership, claimed)?;
            } else {
                self.collect_owned_subtree(physical, depth - 1, ownership, claimed)?;
            }
        }
        self.metadata_path.pop();
        ownership.metadata_blocks.push(metadata);
        Ok(())
    }

    fn collect_owned_data(
        &self,
        data: AbsoluteBN,
        ownership: &mut LegacyInodeOwnership,
        claimed: &mut BTreeSet<AbsoluteBN>,
    ) -> Ext4Result<()> {
        self.validate_data_block(data)?;
        Self::claim_owned_block(claimed, data)?;
        ownership.data_blocks.push(data);
        Ok(())
    }

    fn claim_owned_block(claimed: &mut BTreeSet<AbsoluteBN>, block: AbsoluteBN) -> Ext4Result<()> {
        if !claimed.insert(block) {
            return Err(Ext4Error::corrupted().with_operation("indirect:duplicate_physical_block"));
        }
        Ok(())
    }

    fn enter_metadata_block(&mut self, block: AbsoluteBN) -> Ext4Result<()> {
        self.validate_physical_block(block)?;
        if self.metadata_path.contains(&block) {
            return Err(Ext4Error::corrupted().with_operation("indirect:cycle"));
        }
        self.metadata_path.push(block);
        Ok(())
    }

    fn validate_data_block(&self, block: AbsoluteBN) -> Ext4Result<()> {
        self.validate_physical_block(block)?;
        if self.metadata_path.contains(&block) {
            return Err(Ext4Error::corrupted().with_operation("indirect:cycle"));
        }
        Ok(())
    }

    fn validate_physical_block(&self, block: AbsoluteBN) -> Ext4Result<()> {
        let raw = block.raw();
        let limit = self
            .filesystem
            .superblock
            .blocks_count()
            .min(self.device.total_blocks());
        if raw <= u64::from(self.filesystem.superblock.s_first_data_block) || raw >= limit {
            return Err(Ext4Error::corrupted().with_operation("indirect:physical_range"));
        }
        if !self.filesystem.system_zones.is_empty()
            && !self
                .filesystem
                .system_zones
                .allows_range(raw, 1, self.inode_number)
        {
            return Err(Ext4Error::corrupted().with_operation("indirect:system_metadata"));
        }
        Ok(())
    }

    fn read_pointer(&mut self, metadata: AbsoluteBN, index: usize) -> Ext4Result<u32> {
        self.read_pointer_block(metadata)?
            .get(index)
            .copied()
            .ok_or_else(|| Ext4Error::corrupted().with_operation("indirect:pointer_offset"))
    }

    fn read_pointer_block(&mut self, metadata: AbsoluteBN) -> Ext4Result<Vec<u32>> {
        self.device.read_block(metadata)?;
        let pointers: Vec<u32> = self
            .device
            .buffer()
            .as_chunks::<{ size_of::<u32>() }>()
            .0
            .iter()
            .map(|bytes| read_u32_le(bytes))
            .collect();

        // Linux validates every non-zero entry when an indirect block is
        // read, not only the entry selected by the current lookup. Otherwise
        // latent corruption can be hidden behind a hole or the current inode
        // size and become visible only after a later mutation.
        for &pointer in &pointers {
            if pointer != 0 {
                self.validate_physical_block(AbsoluteBN::from(pointer))?;
            }
        }
        Ok(pointers)
    }
}

fn block_to_path(block_size: usize, logical_block: u32) -> Ext4Result<IndirectPath> {
    if block_size < size_of::<u32>() || !block_size.is_multiple_of(size_of::<u32>()) {
        return Err(Ext4Error::bad_superblock().with_operation("indirect:pointers_per_block"));
    }
    let pointers = (block_size / size_of::<u32>()) as u64;
    let direct = DIRECT_BLOCKS as u64;
    let logical = u64::from(logical_block);
    if logical < direct {
        return Ok(IndirectPath::direct(logical as usize));
    }

    let mut relative = logical - direct;
    if relative < pointers {
        return Ok(IndirectPath::nested(&[
            SINGLE_INDIRECT_SLOT,
            relative as usize,
        ]));
    }

    relative -= pointers;
    let double = pointers
        .checked_mul(pointers)
        .ok_or_else(Ext4Error::overflow)?;
    if relative < double {
        return Ok(IndirectPath::nested(&[
            DOUBLE_INDIRECT_SLOT,
            (relative / pointers) as usize,
            (relative % pointers) as usize,
        ]));
    }

    relative -= double;
    let triple = double
        .checked_mul(pointers)
        .ok_or_else(Ext4Error::overflow)?;
    if relative < triple {
        return Ok(IndirectPath::nested(&[
            TRIPLE_INDIRECT_SLOT,
            (relative / double) as usize,
            ((relative / pointers) % pointers) as usize,
            (relative % pointers) as usize,
        ]));
    }

    Err(Ext4Error::file_too_large().with_operation("indirect:logical_block"))
}

fn is_fast_symlink(filesystem: &Ext4FileSystem, inode: &Ext4Inode) -> bool {
    let huge_file = filesystem
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
    inode.is_symlink()
        && filesystem.inode_size(inode) <= 60
        && inode.blocks_count(filesystem.block_size() as u32, huge_file) == 0
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use core::{cell::Cell, mem::size_of};

    use super::{
        DIRECT_BLOCKS, DOUBLE_INDIRECT_SLOT, IndirectPath, SINGLE_INDIRECT_SLOT,
        TRIPLE_INDIRECT_SLOT, allocate_legacy_inode_block, block_to_path,
        collect_legacy_inode_ownership, has_legacy_indirect_mapping,
    };
    use crate::{
        BLOCK_SIZE, BlockIo, DeviceCapabilities, DeviceGeometry, ErrorContext, Ext4Error,
        Ext4ErrorKind, Ext4FileSystem, Ext4Result, Ext4Timestamp, Jbd2Dev, SectorId,
        bmalloc::{AbsoluteBN, InodeNumber},
        disknode::Ext4Inode,
        endian::write_u32_le,
        ext4::mkfs,
        loopfile::{resolve_inode_block, resolve_inode_blocks},
    };

    struct MemBlockDevice {
        bytes: alloc::vec::Vec<u8>,
        block_count: u64,
        now: Cell<i64>,
    }

    impl MemBlockDevice {
        fn new(block_count: u64) -> Self {
            Self {
                bytes: vec![0; block_count as usize * BLOCK_SIZE],
                block_count,
                now: Cell::new(1_700_000_000),
            }
        }
    }

    impl BlockIo for MemBlockDevice {
        fn read(&mut self, buffer: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
            let required = BLOCK_SIZE * count as usize;
            let start = sector.as_usize()? * BLOCK_SIZE;
            let end = start
                .checked_add(required)
                .ok_or_else(Ext4Error::overflow)?;
            let source = self
                .bytes
                .get(start..end)
                .ok_or_else(Ext4Error::invalid_input)?;
            let provided = buffer.len();
            buffer
                .get_mut(..required)
                .ok_or_else(|| Ext4Error::buffer_too_small(provided, required))?
                .copy_from_slice(source);
            Ok(())
        }

        fn write(&mut self, buffer: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
            let required = BLOCK_SIZE * count as usize;
            let start = sector.as_usize()? * BLOCK_SIZE;
            let end = start
                .checked_add(required)
                .ok_or_else(Ext4Error::overflow)?;
            let target = self
                .bytes
                .get_mut(start..end)
                .ok_or_else(Ext4Error::invalid_input)?;
            target.copy_from_slice(
                buffer
                    .get(..required)
                    .ok_or_else(|| Ext4Error::buffer_too_small(buffer.len(), required))?,
            );
            Ok(())
        }

        fn geometry(&self) -> DeviceGeometry {
            DeviceGeometry::new(BLOCK_SIZE as u32, self.block_count)
        }

        fn capabilities(&self) -> DeviceCapabilities {
            DeviceCapabilities {
                flush: true,
                ..DeviceCapabilities::default()
            }
        }

        fn flush(&mut self) -> Ext4Result<()> {
            Ok(())
        }
    }

    impl crate::Clock for MemBlockDevice {
        fn now(&self) -> Ext4Result<Ext4Timestamp> {
            let seconds = self.now.get();
            self.now.set(seconds + 1);
            Ok(Ext4Timestamp::new(seconds, 0))
        }
    }

    fn setup_filesystem() -> (Jbd2Dev<MemBlockDevice>, Ext4FileSystem) {
        let device = MemBlockDevice::new(16 * 1024);
        let mut device = Jbd2Dev::initial_jbd2dev(0, device, false);
        mkfs(&mut device).unwrap();
        let filesystem = Ext4FileSystem::mount(&mut device).unwrap();
        (device, filesystem)
    }

    fn set_inode_size(inode: &mut Ext4Inode, blocks: u64) {
        inode.i_mode = Ext4Inode::S_IFREG;
        inode.set_size(blocks * BLOCK_SIZE as u64);
    }

    fn write_pointer(
        device: &mut Jbd2Dev<MemBlockDevice>,
        metadata: AbsoluteBN,
        index: usize,
        target: AbsoluteBN,
    ) {
        device.read_block(metadata).unwrap();
        device.buffer_mut().fill(0);
        let start = index * size_of::<u32>();
        write_u32_le(
            target.to_u32().unwrap(),
            &mut device.buffer_mut()[start..start + size_of::<u32>()],
        );
        device.write_block(metadata, true).unwrap();
    }

    #[test]
    fn resolves_legacy_direct_block() {
        let (mut device, mut filesystem) = setup_filesystem();
        let physical = filesystem.alloc_block(&mut device).unwrap();
        let inode_number = InodeNumber::new(12).unwrap();
        let mut inode = Ext4Inode {
            i_size_lo: BLOCK_SIZE as u32,
            ..Default::default()
        };
        inode.i_block[0] = physical.to_u32().unwrap();

        let resolved = resolve_inode_block(&filesystem, &mut device, inode_number, &mut inode, 0)
            .expect("legacy direct pointer should resolve");

        assert_eq!(resolved, Some(physical));
    }

    #[test]
    fn block_to_path_covers_single_double_and_triple_boundaries() {
        let pointers = BLOCK_SIZE / size_of::<u32>();
        let double = pointers * pointers;

        assert_eq!(
            block_to_path(BLOCK_SIZE, 11).unwrap(),
            IndirectPath::direct(11)
        );
        assert_eq!(
            block_to_path(BLOCK_SIZE, 12).unwrap(),
            IndirectPath::nested(&[SINGLE_INDIRECT_SLOT, 0])
        );
        assert_eq!(
            block_to_path(BLOCK_SIZE, (12 + pointers) as u32).unwrap(),
            IndirectPath::nested(&[DOUBLE_INDIRECT_SLOT, 0, 0])
        );
        assert_eq!(
            block_to_path(BLOCK_SIZE, (12 + pointers + double) as u32).unwrap(),
            IndirectPath::nested(&[TRIPLE_INDIRECT_SLOT, 0, 0, 0])
        );
        let maximum = 12u64 + pointers as u64 + double as u64 + double as u64 * pointers as u64;
        assert_eq!(
            block_to_path(BLOCK_SIZE, maximum as u32)
                .unwrap_err()
                .kind(),
            Ext4ErrorKind::FileTooLarge
        );
    }

    #[test]
    fn resolves_and_collects_all_legacy_indirect_levels() {
        let (mut device, mut filesystem) = setup_filesystem();
        let inode_number = InodeNumber::new(12).unwrap();
        let pointers = BLOCK_SIZE / size_of::<u32>();
        let single_lbn = DIRECT_BLOCKS as u32;
        let double_lbn = (DIRECT_BLOCKS + pointers) as u32;
        let triple_lbn = (DIRECT_BLOCKS + pointers + pointers * pointers) as u32;

        let direct_data = filesystem.alloc_block(&mut device).unwrap();
        let single_root = filesystem.alloc_block(&mut device).unwrap();
        let single_data = filesystem.alloc_block(&mut device).unwrap();
        let double_root = filesystem.alloc_block(&mut device).unwrap();
        let double_leaf = filesystem.alloc_block(&mut device).unwrap();
        let double_data = filesystem.alloc_block(&mut device).unwrap();
        let triple_root = filesystem.alloc_block(&mut device).unwrap();
        let triple_middle = filesystem.alloc_block(&mut device).unwrap();
        let triple_leaf = filesystem.alloc_block(&mut device).unwrap();
        let triple_data = filesystem.alloc_block(&mut device).unwrap();

        write_pointer(&mut device, single_root, 0, single_data);
        write_pointer(&mut device, double_root, 0, double_leaf);
        write_pointer(&mut device, double_leaf, 0, double_data);
        write_pointer(&mut device, triple_root, 0, triple_middle);
        write_pointer(&mut device, triple_middle, 0, triple_leaf);
        write_pointer(&mut device, triple_leaf, 0, triple_data);

        let mut inode = Ext4Inode::default();
        inode.i_block[0] = direct_data.to_u32().unwrap();
        inode.i_block[SINGLE_INDIRECT_SLOT] = single_root.to_u32().unwrap();
        inode.i_block[DOUBLE_INDIRECT_SLOT] = double_root.to_u32().unwrap();
        inode.i_block[TRIPLE_INDIRECT_SLOT] = triple_root.to_u32().unwrap();
        set_inode_size(&mut inode, u64::from(triple_lbn) + 1);

        for (logical, expected) in [
            (0, direct_data),
            (single_lbn, single_data),
            (double_lbn, double_data),
            (triple_lbn, triple_data),
        ] {
            assert_eq!(
                resolve_inode_block(&filesystem, &mut device, inode_number, &mut inode, logical,)
                    .unwrap(),
                Some(expected)
            );
        }

        let mappings =
            resolve_inode_blocks(&mut filesystem, &mut device, inode_number, &mut inode).unwrap();
        assert_eq!(mappings.len(), 4);
        assert_eq!(mappings.get(&0), Some(&direct_data));
        assert_eq!(mappings.get(&single_lbn), Some(&single_data));
        assert_eq!(mappings.get(&double_lbn), Some(&double_data));
        assert_eq!(mappings.get(&triple_lbn), Some(&triple_data));
    }

    #[test]
    fn collects_complete_legacy_ownership_beyond_inode_size() {
        let (mut device, mut filesystem) = setup_filesystem();
        let inode_number = InodeNumber::new(12).unwrap();

        let direct_data = filesystem.alloc_block(&mut device).unwrap();
        let single_root = filesystem.alloc_block(&mut device).unwrap();
        let single_data = filesystem.alloc_block(&mut device).unwrap();
        let double_root = filesystem.alloc_block(&mut device).unwrap();
        let double_leaf = filesystem.alloc_block(&mut device).unwrap();
        let double_data = filesystem.alloc_block(&mut device).unwrap();
        let triple_root = filesystem.alloc_block(&mut device).unwrap();
        let triple_middle = filesystem.alloc_block(&mut device).unwrap();
        let triple_leaf = filesystem.alloc_block(&mut device).unwrap();
        let triple_data = filesystem.alloc_block(&mut device).unwrap();

        write_pointer(&mut device, single_root, 1, single_data);
        write_pointer(&mut device, double_root, 1, double_leaf);
        write_pointer(&mut device, double_leaf, 2, double_data);
        write_pointer(&mut device, triple_root, 1, triple_middle);
        write_pointer(&mut device, triple_middle, 2, triple_leaf);
        write_pointer(&mut device, triple_leaf, 3, triple_data);

        let mut inode = Ext4Inode::default();
        inode.i_block[0] = direct_data.to_u32().unwrap();
        inode.i_block[SINGLE_INDIRECT_SLOT] = single_root.to_u32().unwrap();
        inode.i_block[DOUBLE_INDIRECT_SLOT] = double_root.to_u32().unwrap();
        inode.i_block[TRIPLE_INDIRECT_SLOT] = triple_root.to_u32().unwrap();
        set_inode_size(&mut inode, 1);

        let ownership =
            collect_legacy_inode_ownership(&filesystem, &mut device, inode_number, &inode).unwrap();
        assert_eq!(
            ownership.data_blocks,
            vec![direct_data, single_data, double_data, triple_data]
        );
        assert_eq!(
            ownership.metadata_blocks,
            vec![
                single_root,
                double_leaf,
                double_root,
                triple_leaf,
                triple_middle,
                triple_root,
            ]
        );
    }

    #[test]
    fn rejects_duplicate_physical_blocks_in_legacy_ownership() {
        let (mut device, mut filesystem) = setup_filesystem();
        let inode_number = InodeNumber::new(12).unwrap();
        let single_root = filesystem.alloc_block(&mut device).unwrap();
        let shared_data = filesystem.alloc_block(&mut device).unwrap();
        write_pointer(&mut device, single_root, 0, shared_data);

        let mut inode = Ext4Inode::default();
        inode.i_block[0] = shared_data.to_u32().unwrap();
        inode.i_block[SINGLE_INDIRECT_SLOT] = single_root.to_u32().unwrap();
        set_inode_size(&mut inode, 1);

        let error = collect_legacy_inode_ownership(&filesystem, &mut device, inode_number, &inode)
            .unwrap_err();
        assert_eq!(
            error.context(),
            Some(ErrorContext::Operation {
                op: "indirect:duplicate_physical_block",
            })
        );
    }

    #[test]
    fn rejects_invalid_legacy_ownership_beyond_inode_size() {
        let (mut device, mut filesystem) = setup_filesystem();
        let inode_number = InodeNumber::new(12).unwrap();
        let single_root = filesystem.alloc_block(&mut device).unwrap();

        device.read_block(single_root).unwrap();
        device.buffer_mut().fill(0);
        write_u32_le(u32::MAX, &mut device.buffer_mut()[0..size_of::<u32>()]);
        device.write_block(single_root, true).unwrap();

        let mut inode = Ext4Inode::default();
        inode.i_block[SINGLE_INDIRECT_SLOT] = single_root.to_u32().unwrap();
        set_inode_size(&mut inode, 1);

        let error = collect_legacy_inode_ownership(&filesystem, &mut device, inode_number, &inode)
            .unwrap_err();
        assert_eq!(
            error.context(),
            Some(ErrorContext::Operation {
                op: "indirect:physical_range",
            })
        );
    }

    #[test]
    fn rejects_invalid_or_cyclic_legacy_pointers_and_preserves_holes() {
        let (mut device, mut filesystem) = setup_filesystem();
        let inode_number = InodeNumber::new(12).unwrap();
        let mut inode = Ext4Inode::default();
        set_inode_size(&mut inode, 13);

        let single_root = filesystem.alloc_block(&mut device).unwrap();
        inode.i_block[SINGLE_INDIRECT_SLOT] = single_root.to_u32().unwrap();
        assert_eq!(
            resolve_inode_block(
                &filesystem,
                &mut device,
                inode_number,
                &mut inode,
                DIRECT_BLOCKS as u32,
            )
            .unwrap(),
            None
        );

        inode.i_block[0] = filesystem.group_descs[0].block_bitmap() as u32;
        let system_error =
            resolve_inode_block(&filesystem, &mut device, inode_number, &mut inode, 0).unwrap_err();
        assert_eq!(
            system_error.context(),
            Some(ErrorContext::Operation {
                op: "indirect:system_metadata",
            })
        );

        inode.i_block[0] = u32::MAX;
        let range_error =
            resolve_inode_block(&filesystem, &mut device, inode_number, &mut inode, 0).unwrap_err();
        assert_eq!(
            range_error.context(),
            Some(ErrorContext::Operation {
                op: "indirect:physical_range",
            })
        );

        write_pointer(&mut device, single_root, 0, single_root);
        inode.i_block[0] = 0;
        let cycle_error = resolve_inode_block(
            &filesystem,
            &mut device,
            inode_number,
            &mut inode,
            DIRECT_BLOCKS as u32,
        )
        .unwrap_err();
        assert_eq!(
            cycle_error.context(),
            Some(ErrorContext::Operation {
                op: "indirect:cycle",
            })
        );

        assert!(has_legacy_indirect_mapping(&filesystem, &inode));
    }

    #[test]
    fn rejects_invalid_sibling_in_indirect_block() {
        let (mut device, mut filesystem) = setup_filesystem();
        let inode_number = InodeNumber::new(12).unwrap();
        let single_root = filesystem.alloc_block(&mut device).unwrap();
        let single_data = filesystem.alloc_block(&mut device).unwrap();
        write_pointer(&mut device, single_root, 0, single_data);

        device.read_block(single_root).unwrap();
        write_u32_le(
            u32::MAX,
            &mut device.buffer_mut()[size_of::<u32>()..2 * size_of::<u32>()],
        );
        device.write_block(single_root, true).unwrap();

        let mut inode = Ext4Inode::default();
        inode.i_block[SINGLE_INDIRECT_SLOT] = single_root.to_u32().unwrap();
        set_inode_size(&mut inode, 13);

        let error = resolve_inode_block(
            &filesystem,
            &mut device,
            inode_number,
            &mut inode,
            DIRECT_BLOCKS as u32,
        )
        .unwrap_err();
        assert_eq!(
            error.context(),
            Some(ErrorContext::Operation {
                op: "indirect:physical_range",
            })
        );
    }

    #[test]
    fn prepared_legacy_branch_rolls_back_all_blocks_and_accounting() {
        let (mut device, mut filesystem) = setup_filesystem();
        let inode_number = InodeNumber::new(12).unwrap();
        let logical = (DIRECT_BLOCKS + BLOCK_SIZE / size_of::<u32>()) as u32;
        let mut inode = Ext4Inode::default();
        let free_blocks_before = filesystem.superblock.free_blocks_count();

        let allocation = allocate_legacy_inode_block(
            &mut filesystem,
            &mut device,
            inode_number,
            &mut inode,
            logical,
        )
        .unwrap();
        assert!(allocation.is_new());
        assert_eq!(
            inode.blocks_count(BLOCK_SIZE as u32, true),
            3 * (BLOCK_SIZE / 512) as u64
        );
        assert_eq!(
            resolve_inode_block(&filesystem, &mut device, inode_number, &mut inode, logical,)
                .unwrap(),
            Some(allocation.physical())
        );

        allocation
            .rollback(&mut filesystem, &mut device, inode_number, &mut inode)
            .unwrap();

        assert_eq!(inode.i_block, [0; 15]);
        assert_eq!(inode.blocks_count(BLOCK_SIZE as u32, true), 0);
        assert_eq!(
            filesystem.superblock.free_blocks_count(),
            free_blocks_before
        );
        assert_eq!(
            resolve_inode_block(&filesystem, &mut device, inode_number, &mut inode, logical,)
                .unwrap(),
            None
        );
    }
}
