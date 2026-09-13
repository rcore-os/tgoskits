//! Resize intent, orphan recovery and final inode mapping retirement.

use super::{
    legacy_removal::{
        build_legacy_mapping_transaction, commit_legacy_mapping_removal,
        legacy_mapping_restart_limit, remove_legacy_mapping_with_restarts,
    },
    removal::{
        commit_extent_mapping_removal, extent_removal_restart_limit,
        prepare_extent_mapping_removal, remove_extent_mapping_with_restarts,
    },
    transaction::{
        MetadataTransactionStart, MetadataTransactionStep, finalize_restarted_inode_update,
    },
    zero::zero_mapped_inode_tail,
    *,
};

/// One resize intent retained across lock-external journal progress.
///
/// The embedding VFS must exclude other content/size mutations of this inode
/// until completion. Namespace unlink may still run: cleanup retains the
/// orphan entry of a zero-link inode for its eventual final reap.
#[derive(Debug)]
pub struct InodeResize {
    inode: InodeNumber,
    target_size: u64,
    original_size: Option<u64>,
    complete: bool,
}

impl InodeResize {
    pub(crate) const fn inode_number(&self) -> InodeNumber {
        self.inode
    }

    /// Starts a resize request without accessing filesystem state.
    pub const fn new(inode: InodeNumber, target_size: u64) -> Self {
        Self {
            inode,
            target_size,
            original_size: None,
            complete: false,
        }
    }

    pub(crate) fn resume<B: BlockIo>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        fs: &mut Ext4FileSystem,
    ) -> Ext4Result<()> {
        device.ensure_mutation_admitted()?;
        if self.complete {
            return Ok(());
        }
        let inode = fs.get_inode_by_num(device, self.inode)?;
        let original_size = *self.original_size.get_or_insert(inode.size());
        if self.target_size < original_size && inode.size() == self.target_size {
            // Size publication starts a bounded shrink; it is not evidence
            // that all mappings have been detached. Do not infer intent from
            // the current link count, which can change while awaiting I/O.
            truncate_inode_mapping(
                device,
                fs,
                self.inode,
                self.target_size,
                TruncatePurpose::OrphanRecovery,
            )?;
            if inode.i_links_count != 0 && fs.orphan_contains(device, self.inode)? {
                finish_orphaned_truncate(device, fs, self.inode)?;
            }
        } else {
            truncate_inode(device, fs, self.inode, self.target_size)?;
        }
        self.complete = true;
        Ok(())
    }
}

pub fn truncate_inode<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    truncate_size: u64,
) -> Ext4Result<()> {
    let inode = fs.get_inode_by_num(device, inode_num)?;
    if inode.i_links_count != 0 && fs.orphan_contains(device, inode_num)? {
        // A prior bounded truncate may have published its new size before
        // yielding for journal space. Finish that exact orphan intent before
        // interpreting another resize; size equality alone is not completion.
        recover_linked_truncate_inode(device, fs, inode_num, inode.size())?;
    }
    truncate_inode_mapping(
        device,
        fs,
        inode_num,
        truncate_size,
        TruncatePurpose::UserResize,
    )
}

pub(crate) fn recover_linked_truncate_inode<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    truncate_size: u64,
) -> Ext4Result<()> {
    truncate_inode_mapping(
        device,
        fs,
        inode_num,
        truncate_size,
        TruncatePurpose::OrphanRecovery,
    )?;
    finish_orphaned_truncate(device, fs, inode_num)
}

pub(crate) fn truncate_inode_for_reap<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
) -> Ext4Result<()> {
    truncate_inode_mapping(device, fs, inode_num, 0, TruncatePurpose::FinalReap)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TruncatePurpose {
    UserResize,
    OrphanRecovery,
    FinalReap,
}

impl TruncatePurpose {
    const fn force_mapping_cleanup(self) -> bool {
        !matches!(self, Self::UserResize)
    }

    const fn accepts_non_file(self) -> bool {
        matches!(self, Self::FinalReap)
    }
}

fn truncate_inode_mapping<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    truncate_size: u64,
    purpose: TruncatePurpose,
) -> Ext4Result<()> {
    let mut inode = fs.get_inode_by_num(device, inode_num)?;

    if inode.is_symlink() && !purpose.accepts_non_file() {
        return Err(Ext4Error::unsupported());
    } else if !inode.is_file() && !purpose.accepts_non_file() {
        return Err(Ext4Error::invalid_input());
    }

    let old_size = inode.size();
    if truncate_size == old_size && !purpose.force_mapping_cleanup() {
        return Ok(());
    }

    let block_bytes = fs.block_size() as u64;
    let new_blocks = if truncate_size == 0 {
        0u64
    } else {
        truncate_size.div_ceil(block_bytes)
    };

    // ext4 logical block numbers are u32; reject sizes that need more blocks.
    if new_blocks > u32::MAX as u64 {
        return Err(Ext4Error::file_too_large());
    }

    if truncate_size > old_size {
        if !inode.uses_extents() {
            crate::indirect::validate_legacy_block_count(fs.block_size(), new_blocks)?;
        }
        // Linux clears the old partial EOF before publishing a larger size so
        // bytes hidden by an earlier shrink can never become visible again.
        zero_mapped_inode_tail(device, fs, inode_num, &mut inode, old_size)?;
        inode.i_size_lo = truncate_size as u32;
        inode.i_size_high = (truncate_size >> 32) as u32;
        fs.finalize_inode_update(
            device,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::truncate_access(),
        )?;
        return Ok(());
    }

    // Extent-backed files handle extent-aware shrinking here.
    if fs.superblock.has_extents() && inode.uses_extents() {
        if truncate_size < old_size || purpose.force_mapping_cleanup() {
            // Validate and plan the complete initialized/unwritten removal
            // before changing the retained tail or any filesystem metadata.
            let removal = prepare_extent_mapping_removal(
                device,
                fs,
                inode_num,
                &inode,
                new_blocks,
                u64::from(u32::MAX) + 1,
            )?;
            let restart_limit = extent_removal_restart_limit(
                device,
                fs,
                inode_num,
                &inode,
                new_blocks,
                u64::from(u32::MAX) + 1,
                &removal,
            )?;
            zero_mapped_inode_tail(device, fs, inode_num, &mut inode, truncate_size)?;
            if let Some(credit_limit) = restart_limit {
                if purpose == TruncatePurpose::UserResize {
                    begin_restarted_truncate(device, fs, inode_num, &mut inode, truncate_size)?;
                }
                remove_extent_mapping_with_restarts(
                    device,
                    fs,
                    inode_num,
                    &mut inode,
                    new_blocks,
                    u64::from(u32::MAX) + 1,
                    credit_limit,
                )?;
                return if purpose == TruncatePurpose::UserResize {
                    finish_orphaned_truncate(device, fs, inode_num)
                } else {
                    inode.i_size_lo = truncate_size as u32;
                    inode.i_size_high = (truncate_size >> 32) as u32;
                    finalize_restarted_inode_update(
                        device,
                        fs,
                        inode_num,
                        &mut inode,
                        Ext4InodeMetadataUpdate::truncate_access(),
                    )
                };
            } else {
                return commit_extent_mapping_removal(
                    device,
                    fs,
                    inode_num,
                    &mut inode,
                    Ext4InodeMetadataUpdate::truncate_access(),
                    Some(truncate_size),
                    MetadataTransactionStep {
                        start: MetadataTransactionStart::Join,
                        payload: removal,
                    },
                );
            }
        }

        inode.i_size_lo = (truncate_size & 0xffff_ffff) as u32;
        inode.i_size_high = (truncate_size >> 32) as u32;
        return fs.finalize_inode_update(
            device,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::truncate_access(),
        );
    }

    let truncate_plan =
        crate::indirect::plan_legacy_inode_truncate(fs, device, inode_num, &inode, new_blocks)?;
    let transaction = build_legacy_mapping_transaction(fs, truncate_plan)?;
    let restart_limit = legacy_mapping_restart_limit(
        device,
        fs,
        inode_num,
        &inode,
        new_blocks,
        u64::from(u32::MAX) + 1,
        &transaction,
    )?;
    // Linux zeros the retained partial EOF block before detaching later
    // mappings. A corrupt hidden branch therefore still fails during the plan
    // preflight before any data or metadata is changed.
    zero_mapped_inode_tail(device, fs, inode_num, &mut inode, truncate_size)?;
    if let Some(credit_limit) = restart_limit {
        if purpose == TruncatePurpose::UserResize {
            begin_restarted_truncate(device, fs, inode_num, &mut inode, truncate_size)?;
        }
        remove_legacy_mapping_with_restarts(
            device,
            fs,
            inode_num,
            &mut inode,
            new_blocks,
            u64::from(u32::MAX) + 1,
            credit_limit,
        )?;
        if purpose == TruncatePurpose::UserResize {
            finish_orphaned_truncate(device, fs, inode_num)
        } else {
            inode.i_size_lo = truncate_size as u32;
            inode.i_size_high = (truncate_size >> 32) as u32;
            finalize_restarted_inode_update(
                device,
                fs,
                inode_num,
                &mut inode,
                Ext4InodeMetadataUpdate::truncate_access(),
            )
        }
    } else {
        commit_legacy_mapping_removal(
            device,
            fs,
            inode_num,
            &mut inode,
            Ext4InodeMetadataUpdate::truncate_access(),
            Some(truncate_size),
            MetadataTransactionStep {
                start: MetadataTransactionStart::Join,
                payload: transaction,
            },
        )
    }
}

pub fn truncate<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    truncate_size: u64,
) -> Ext4Result<()> {
    let norm_path = normalize_path(path);

    // Resolve the target inode once, then delegate to the inode-based helper.
    let (inode_num, _inode) = match get_inode_with_num(fs, device, &norm_path)? {
        Some(v) => v,
        None => return Err(Ext4Error::not_found()),
    };

    truncate_inode(device, fs, inode_num, truncate_size)
}

fn begin_restarted_truncate<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    truncate_size: u64,
) -> Ext4Result<()> {
    let original_inode = *inode;
    let updated = fs.with_metadata_transaction(device, 2, |fs, device| {
        let mut updated = original_inode;
        updated.i_size_lo = truncate_size as u32;
        updated.i_size_high = (truncate_size >> 32) as u32;
        fs.finalize_inode_update(
            device,
            inode_num,
            &mut updated,
            Ext4InodeMetadataUpdate::truncate_access(),
        )?;
        fs.add_orphan(device, inode_num)?;
        updated = fs.get_inode_by_num(device, inode_num)?;
        fs.inodetable_cache.flush(device, inode_num)?;
        fs.sync_superblock(device)?;
        Ok(updated)
    })?;
    *inode = updated;
    Ok(())
}

fn finish_orphaned_truncate<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: InodeNumber,
) -> Ext4Result<()> {
    // An unlinked inode still needs the on-disk orphan chain until its final
    // open reference is reaped, even when this resize freed every data block.
    if fs.get_inode_by_num(device, inode_num)?.i_links_count == 0 {
        return Ok(());
    }
    // Removing a non-head orphan can dirty the predecessor inode-table block,
    // the target inode-table block, and the superblock.
    fs.with_metadata_transaction(device, 3, |fs, device| {
        let predecessor = fs.remove_orphan(device, inode_num)?;
        fs.inodetable_cache.flush(device, inode_num)?;
        if let Some(predecessor) = predecessor {
            fs.inodetable_cache.flush(device, predecessor)?;
        }
        fs.sync_superblock(device)?;
        Ok(())
    })
}
