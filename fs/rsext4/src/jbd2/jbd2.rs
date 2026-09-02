//! JBD2 transaction commit and replay logic.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec,
    vec::Vec,
};

use crate::{
    blockdev::*,
    bmalloc::{AbsoluteBN, InodeNumber},
    checksum::{
        jbd2_commit_block_csum32, jbd2_compat_checksum_append, jbd2_descriptor_block_csum32,
        jbd2_partial_commit_block_csum32, jbd2_tag_csum32, jbd2_update_superblock_checksum,
    },
    crc32c::crc32c::ext4_superblock_has_metadata_csum,
    disknode::*,
    endian::*,
    error::*,
    ext4::*,
    file::*,
    io::WriteFlags,
    jbd2::jbdstruct::*,
    metadata::Ext4InodeMetadataUpdate,
    runtime::JournalReplayPhase,
};

/// Two's-complement JBD2 on-disk representation of a generic I/O abort.
///
/// This is a private wire-format value, not an OS errno exposed by the core.
const JBD2_DISK_ERROR_IO: u32 = 0xffff_fffb;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Jbd2CommitTimestamp {
    seconds: u64,
    nanoseconds: u32,
}

impl TryFrom<Ext4Timestamp> for Jbd2CommitTimestamp {
    type Error = Ext4Error;

    fn try_from(timestamp: Ext4Timestamp) -> Ext4Result<Self> {
        let seconds = u64::try_from(timestamp.sec)
            .map_err(|_| Ext4Error::invalid_input().with_operation("jbd2:commit_time_seconds"))?;
        if timestamp.nsec > Ext4Timestamp::MAX_NSEC {
            return Err(Ext4Error::invalid_input().with_operation("jbd2:commit_time_nanoseconds"));
        }
        Ok(Self {
            seconds,
            nanoseconds: timestamp.nsec,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct ReplayTag {
    block: AbsoluteBN,
    flags: u32,
    checksum: Option<ReplayChecksum>,
}

#[derive(Debug, Clone, Copy)]
enum ReplayChecksum {
    CsumV2(u16),
    CsumV3(u32),
}

#[derive(Debug, Clone, Copy)]
struct ReplayPayload {
    tag: ReplayTag,
    journal_rel: u32,
}

#[derive(Debug)]
enum ReplayDescriptor {
    EmptyTail,
    Tagged {
        tags: Vec<ReplayTag>,
        checksum_valid: bool,
    },
}

struct ReplayRevoke {
    blocks: Vec<AbsoluteBN>,
    checksum_valid: bool,
}

enum JournalPayload<'a> {
    Borrowed(&'a [u8]),
    Escaped(Vec<u8>),
}

impl JournalPayload<'_> {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Escaped(bytes) => bytes,
        }
    }

    fn is_escaped(&self) -> bool {
        matches!(self, Self::Escaped(_))
    }
}

fn journal_payload<'a>(
    update: &'a Jbd2Update,
    block_size: usize,
) -> Ext4Result<JournalPayload<'a>> {
    if update.1.len() != block_size || update.1.len() < 4 {
        return Err(Ext4Error::corrupted().with_operation("jbd2:update_block_size"));
    }
    if !update.1.starts_with(&JBD2_MAGIC.to_be_bytes()) {
        return Ok(JournalPayload::Borrowed(&update.1));
    }

    let mut escaped = update.1.to_vec();
    escaped[..4].fill(0);
    Ok(JournalPayload::Escaped(escaped))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplayFailure {
    phase: JournalReplayPhase,
    cause: Ext4Error,
    restart_rel: Option<u32>,
    persistence_error: Option<Ext4Error>,
}

impl ReplayFailure {
    fn at(phase: JournalReplayPhase, cause: Ext4Error, restart_rel: u32) -> Self {
        Self {
            phase,
            cause,
            restart_rel: Some(restart_rel),
            persistence_error: None,
        }
    }

    pub(crate) const fn without_restart(phase: JournalReplayPhase, cause: Ext4Error) -> Self {
        Self {
            phase,
            cause,
            restart_rel: None,
            persistence_error: None,
        }
    }

    pub(crate) const fn phase(self) -> JournalReplayPhase {
        self.phase
    }

    pub(crate) const fn cause(self) -> Ext4Error {
        self.cause
    }

    pub(crate) const fn restart_rel(self) -> Option<u32> {
        self.restart_rel
    }

    pub(crate) const fn persistence_error(self) -> Option<Ext4Error> {
        self.persistence_error
    }

    fn with_persistence_error(mut self, error: Ext4Error) -> Self {
        self.persistence_error = Some(error);
        self
    }

    fn with_restart_rel(mut self, restart_rel: u32) -> Self {
        self.restart_rel = Some(restart_rel);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplayStatus {
    Complete,
    Incomplete(ReplayFailure),
}

impl ReplayStatus {
    #[cfg(test)]
    pub(crate) const fn failure(self) -> Option<ReplayFailure> {
        match self {
            Self::Complete => None,
            Self::Incomplete(failure) => Some(failure),
        }
    }
}

#[derive(Debug)]
struct ReplayTransaction {
    start_rel: u32,
    sequence: u32,
    next_rel: u32,
    commit_time: u64,
    payloads: Vec<ReplayPayload>,
    revoked_blocks: Vec<AbsoluteBN>,
}

#[derive(Debug)]
enum ReplayScan {
    CleanEnd,
    Incomplete(ReplayFailure),
    Committed(ReplayTransaction),
}

struct ReplayRing<'a> {
    blocks: &'a [AbsoluteBN],
    start_block: AbsoluteBN,
    first_rel: u32,
    last_rel: u32,
}

impl<'a> ReplayRing<'a> {
    fn new(system: &JBD2DEVSYSTEM, blocks: &'a [AbsoluteBN]) -> Option<Self> {
        let last_rel = system.last_logical_block(blocks)?;
        Some(Self {
            blocks,
            start_block: system.start_block,
            first_rel: system.jbd2_super_block.s_first,
            last_rel,
        })
    }

    fn phys(&self, rel: u32) -> Ext4Result<AbsoluteBN> {
        if self.blocks.is_empty() {
            return self.start_block.checked_add(rel);
        }

        self.blocks
            .get(rel as usize)
            .copied()
            .ok_or_else(Ext4Error::corrupted)
    }

    fn advance(&self, rel: &mut u32) {
        if *rel >= self.last_rel {
            *rel = self.first_rel;
        } else {
            *rel = rel.saturating_add(1);
        }
    }
}

impl JBD2DEVSYSTEM {
    fn has_incompat_feature(&self, feature: u32) -> bool {
        !self.jbd2_super_block.is_v1() && self.jbd2_super_block.s_feature_incompat & feature != 0
    }

    fn checksum_mode(&self) -> Ext4Result<Jbd2ChecksumMode> {
        self.jbd2_super_block.checksum_mode()
    }

    fn journal_phys_block(
        &self,
        journal_blocks: &[AbsoluteBN],
        logical_block: u32,
    ) -> Ext4Result<AbsoluteBN> {
        if journal_blocks.is_empty() {
            return self.start_block.checked_add(logical_block);
        }

        journal_blocks
            .get(logical_block as usize)
            .copied()
            .ok_or_else(Ext4Error::corrupted)
    }

    fn last_logical_block(&self, journal_blocks: &[AbsoluteBN]) -> Option<u32> {
        let mapped_len = u32::try_from(journal_blocks.len()).ok();
        let total_blocks = match mapped_len {
            Some(0) | None => self.jbd2_super_block.s_maxlen,
            // When s_maxlen from the on-disk journal superblock is smaller than
            // the actual number of journal blocks (e.g. s_maxlen=1 due to an
            // unclean shutdown that corrupted the journal superblock), trust the
            // physical extent mapping rather than the stale s_maxlen.
            Some(len) => {
                let sb_maxlen = self.jbd2_super_block.s_maxlen;
                if sb_maxlen > 0 && sb_maxlen >= self.jbd2_super_block.s_first {
                    sb_maxlen.min(len)
                } else {
                    // s_maxlen is 0 or smaller than s_first — it is corrupted.
                    // Fall back to the physical extent length.

                    len
                }
            }
        };

        let last = total_blocks.checked_sub(1)?;
        if last < self.jbd2_super_block.s_first {
            None
        } else {
            Some(last)
        }
    }

    fn parse_replay_tags(&self, desc_buf: &[u8]) -> Ext4Result<ReplayDescriptor> {
        let checksum_mode = self.checksum_mode()?;
        let has_block_checksums = checksum_mode.has_block_checksums();
        let has_64bit = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_64BIT);
        let block_size = desc_buf.len();
        let (descriptor_end, checksum_valid) = if has_block_checksums {
            let checksum_offset = block_size.checked_sub(4).ok_or_else(|| {
                Ext4Error::corrupted().with_operation("jbd2:replay_descriptor_size")
            })?;
            let stored =
                u32::from_be_bytes(desc_buf[checksum_offset..].try_into().map_err(|_| {
                    Ext4Error::corrupted().with_operation("jbd2:replay_descriptor_checksum_field")
                })?);
            let computed = jbd2_descriptor_block_csum32(&self.jbd2_super_block.s_uuid, desc_buf)
                .ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("jbd2:replay_descriptor_checksum_size")
                })?;
            (checksum_offset, stored == computed)
        } else {
            (block_size, true)
        };
        let mut tags = Vec::new();
        let mut off = JBD2_DESCRIPTOR_HEADER_SIZE;
        let mut saw_last_tag = false;

        while off < descriptor_end {
            let parsed = if checksum_mode == Jbd2ChecksumMode::CsumV3 {
                let tag_end = off
                    .checked_add(JBD2_TAG3_SIZE)
                    .ok_or_else(Ext4Error::overflow)?;
                if tag_end > descriptor_end {
                    return Err(Ext4Error::corrupted().with_operation("jbd2:replay_tag3_truncated"));
                }
                let tag = JournalBlockTag3S::from_disk_bytes(&desc_buf[off..tag_end]);
                if !has_64bit && tag.t_blocknr_high != 0 {
                    return Err(
                        Ext4Error::corrupted().with_operation("jbd2:replay_tag3_block_high")
                    );
                }
                let block = (u64::from(tag.t_blocknr_high) << 32) | u64::from(tag.t_blocknr);
                let all_zero = tag.t_blocknr == 0
                    && tag.t_flags == 0
                    && tag.t_blocknr_high == 0
                    && tag.t_checksum == 0;
                off = tag_end;
                (
                    block,
                    tag.t_flags,
                    Some(ReplayChecksum::CsumV3(tag.t_checksum)),
                    all_zero,
                )
            } else {
                let tag_end = off
                    .checked_add(JBD2_TAG_SIZE)
                    .ok_or_else(Ext4Error::overflow)?;
                if tag_end > descriptor_end {
                    return Err(Ext4Error::corrupted().with_operation("jbd2:replay_tag_truncated"));
                }
                let tag = JournalBlockTagS::from_disk_bytes(&desc_buf[off..tag_end]);
                off = tag_end;

                let mut block_high = 0u32;
                if has_64bit {
                    let high_end = off
                        .checked_add(JBD2_TAG_BLOCKNR_HIGH_SIZE)
                        .ok_or_else(Ext4Error::overflow)?;
                    if high_end > descriptor_end {
                        return Err(
                            Ext4Error::corrupted().with_operation("jbd2:replay_tag_high_truncated")
                        );
                    }
                    block_high =
                        u32::from_be_bytes(desc_buf[off..high_end].try_into().map_err(|_| {
                            Ext4Error::corrupted().with_operation("jbd2:replay_tag_high_field")
                        })?);
                    off = high_end;
                }

                if checksum_mode == Jbd2ChecksumMode::CsumV2 {
                    let padding_end = off
                        .checked_add(core::mem::size_of::<u16>())
                        .ok_or_else(Ext4Error::overflow)?;
                    if padding_end > descriptor_end {
                        return Err(Ext4Error::corrupted()
                            .with_operation("jbd2:replay_tag_csum_v2_truncated"));
                    }
                    off = padding_end;
                }

                let block = (u64::from(block_high) << 32) | u64::from(tag.t_blocknr);
                let all_zero = tag.t_blocknr == 0
                    && tag.t_checksum == 0
                    && tag.t_flags == 0
                    && block_high == 0;
                let checksum = (checksum_mode == Jbd2ChecksumMode::CsumV2)
                    .then_some(ReplayChecksum::CsumV2(tag.t_checksum));
                (block, u32::from(tag.t_flags), checksum, all_zero)
            };

            let (block, flags, checksum, all_zero) = parsed;
            if all_zero && desc_buf[off..descriptor_end].iter().all(|b| *b == 0) {
                if tags.is_empty() {
                    return Ok(ReplayDescriptor::EmptyTail);
                }
                return Err(
                    Ext4Error::corrupted().with_operation("jbd2:replay_descriptor_missing_last")
                );
            }

            let last = (flags & u32::from(JBD2_FLAG_LAST_TAG)) != 0;
            let same_uuid = (flags & u32::from(JBD2_FLAG_SAME_UUID)) != 0;
            tags.push(ReplayTag {
                block: AbsoluteBN::new(block),
                flags,
                checksum,
            });

            if !same_uuid {
                let uuid_end = off
                    .checked_add(JBD2_UUID_SIZE)
                    .ok_or_else(Ext4Error::overflow)?;
                if uuid_end > descriptor_end {
                    return Err(Ext4Error::corrupted().with_operation("jbd2:replay_uuid_truncated"));
                }
                off = uuid_end;
            }
            if last {
                saw_last_tag = true;
                break;
            }
        }

        if !saw_last_tag {
            return Err(
                Ext4Error::corrupted().with_operation("jbd2:replay_descriptor_missing_last")
            );
        }

        Ok(ReplayDescriptor::Tagged {
            tags,
            checksum_valid,
        })
    }

    fn parse_revoke_blocks(&self, revoke_buf: &[u8]) -> Ext4Result<ReplayRevoke> {
        if revoke_buf.len() < 16 {
            return Err(Ext4Error::corrupted().with_operation("jbd2:replay_revoke_header"));
        }
        let (record_end, checksum_valid) = if self.checksum_mode()?.has_block_checksums() {
            let checksum_offset = revoke_buf.len().checked_sub(4).ok_or_else(|| {
                Ext4Error::corrupted().with_operation("jbd2:replay_revoke_checksum_size")
            })?;
            let stored =
                u32::from_be_bytes(revoke_buf[checksum_offset..].try_into().map_err(|_| {
                    Ext4Error::corrupted().with_operation("jbd2:replay_revoke_checksum_field")
                })?);
            let computed = jbd2_descriptor_block_csum32(&self.jbd2_super_block.s_uuid, revoke_buf)
                .ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("jbd2:replay_revoke_checksum_size")
                })?;
            (checksum_offset, stored == computed)
        } else {
            (revoke_buf.len(), true)
        };
        let revoke = Jbd2JournalRevokeHeadS::from_disk_bytes(&revoke_buf[0..16]);
        let count = usize::try_from(revoke.r_count).map_err(|_| Ext4Error::overflow())?;
        if !(16..=record_end).contains(&count) {
            return Err(Ext4Error::corrupted().with_operation("jbd2:replay_revoke_count"));
        }

        let entry_size = if self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_64BIT) {
            8
        } else {
            4
        };
        let mut blocks = Vec::new();
        let mut off = 16usize;
        while off < count {
            let entry_end = off
                .checked_add(entry_size)
                .ok_or_else(Ext4Error::overflow)?;
            if entry_end > count {
                return Err(Ext4Error::corrupted().with_operation("jbd2:replay_revoke_entry"));
            }

            let block = if entry_size == 8 {
                u64::from_be_bytes(revoke_buf[off..entry_end].try_into().map_err(|_| {
                    Ext4Error::corrupted().with_operation("jbd2:replay_revoke_entry64")
                })?)
            } else {
                u64::from(u32::from_be_bytes(
                    revoke_buf[off..entry_end].try_into().map_err(|_| {
                        Ext4Error::corrupted().with_operation("jbd2:replay_revoke_entry32")
                    })?,
                ))
            };
            blocks.push(AbsoluteBN::new(block));
            off = entry_end;
        }

        Ok(ReplayRevoke {
            blocks,
            checksum_valid,
        })
    }

    fn write_journal_superblock_with_mapping<D: FilesystemBlockIo>(
        &mut self,
        block_dev: &mut D,
        journal_blocks: &[AbsoluteBN],
    ) -> Ext4Result<()> {
        self.write_journal_superblock_with_mapping_flags(
            block_dev,
            journal_blocks,
            WriteFlags::METADATA,
        )
    }

    fn write_journal_superblock_with_mapping_flags<D: FilesystemBlockIo>(
        &mut self,
        block_dev: &mut D,
        journal_blocks: &[AbsoluteBN],
        flags: WriteFlags,
    ) -> Ext4Result<()> {
        let sb_block = self.journal_phys_block(journal_blocks, 0)?;
        let block_size = block_dev.block_size();
        if block_size < 1024 {
            return Err(Ext4Error::corrupted().with_operation("jbd2:small_block"));
        }
        let mut sb_data = vec![0u8; block_size];
        block_dev.read(&mut sb_data, sb_block, 1)?;
        jbd2_update_superblock_checksum(&mut self.jbd2_super_block);
        self.jbd2_super_block.encode_checked(&mut sb_data)?;
        block_dev.write_with_flags(&sb_data, sb_block, 1, flags)
    }

    /// Records the first runtime journal abort in the on-disk superblock.
    pub(crate) fn record_abort_with_mapping<D: FilesystemBlockIo>(
        &mut self,
        block_dev: &mut D,
        journal_blocks: &[AbsoluteBN],
    ) -> Ext4Result<()> {
        if self.jbd2_super_block.s_errno == 0 {
            self.jbd2_super_block.s_errno = JBD2_DISK_ERROR_IO;
        }
        self.write_journal_superblock_with_mapping_flags(
            block_dev,
            journal_blocks,
            WriteFlags::METADATA | WriteFlags::FUA,
        )
    }

    /// Returns the next writable journal block using the journal inode mapping.
    pub(crate) fn set_next_log_block_with_mapping<D: FilesystemBlockIo>(
        &mut self,
        block_dev: &mut D,
        journal_blocks: &[AbsoluteBN],
    ) -> Ext4Result<AbsoluteBN> {
        let last_rel = self
            .last_logical_block(journal_blocks)
            .ok_or_else(Ext4Error::corrupted)?;

        // The first record of a clean journal initializes the durable tail at
        // the current head. The head remains an absolute ring cursor, so a
        // later tail advance never changes the next writable record.
        if self.jbd2_super_block.s_start == 0 {
            let previous_superblock = self.jbd2_super_block;
            self.jbd2_super_block.s_start = self.head;
            self.jbd2_super_block.s_sequence = self.sequence;
            if let Err(error) =
                self.write_journal_superblock_with_mapping(block_dev, journal_blocks)
            {
                self.jbd2_super_block = previous_superblock;
                return Err(error);
            }
        }
        let rel = self.head;
        let target_use = self.journal_phys_block(journal_blocks, rel)?;
        self.head = if rel >= last_rel {
            self.jbd2_super_block.s_first
        } else {
            rel.checked_add(1).ok_or_else(Ext4Error::overflow)?
        };
        self.used_log_records = self
            .used_log_records
            .checked_add(1)
            .ok_or_else(Ext4Error::overflow)?;
        Ok(target_use)
    }

    fn write_revoke_records<D: FilesystemBlockIo>(
        &mut self,
        block_dev: &mut D,
        journal_blocks: &[AbsoluteBN],
        tid: u32,
        block_size: usize,
        revoked_blocks: &[AbsoluteBN],
    ) -> Ext4Result<()> {
        let has_block_checksums = self.checksum_mode()?.has_block_checksums();
        let has_64bit = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_64BIT);
        let entry_size = if has_64bit { 8 } else { 4 };
        let record_end = if has_block_checksums {
            block_size
                .checked_sub(core::mem::size_of::<u32>())
                .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:revoke_size"))?
        } else {
            block_size
        };
        let capacity = record_end
            .checked_sub(core::mem::size_of::<Jbd2JournalRevokeHeadS>())
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:revoke_size"))?
            / entry_size;
        if revoked_blocks.is_empty() {
            return Ok(());
        }
        if capacity == 0 {
            return Err(Ext4Error::no_space().with_operation("jbd2:revoke_capacity"));
        }

        let mut revoke_buffer = vec![0_u8; block_size];
        for (record_index, revoked) in revoked_blocks.chunks(capacity).enumerate() {
            if record_index != 0 {
                revoke_buffer.fill(0);
            }
            let record_bytes = revoked
                .len()
                .checked_mul(entry_size)
                .and_then(|bytes| bytes.checked_add(core::mem::size_of::<Jbd2JournalRevokeHeadS>()))
                .ok_or_else(Ext4Error::overflow)?;
            Jbd2JournalRevokeHeadS {
                r_header: JournalHeaderS {
                    h_magic: JBD2_MAGIC,
                    h_blocktype: JBD2_BLOCKTYPE_REVOKE,
                    h_sequence: tid,
                },
                r_count: u32::try_from(record_bytes).map_err(|_| Ext4Error::overflow())?,
            }
            .to_disk_bytes(&mut revoke_buffer[..core::mem::size_of::<Jbd2JournalRevokeHeadS>()]);
            let mut offset = core::mem::size_of::<Jbd2JournalRevokeHeadS>();
            for block in revoked {
                let raw = block.raw();
                if has_64bit {
                    revoke_buffer[offset..offset + 8].copy_from_slice(&raw.to_be_bytes());
                } else {
                    let block32 = u32::try_from(raw).map_err(|_| {
                        Ext4Error::unsupported().with_operation("jbd2:64bit_revoke")
                    })?;
                    revoke_buffer[offset..offset + 4].copy_from_slice(&block32.to_be_bytes());
                }
                offset += entry_size;
            }
            if has_block_checksums {
                let checksum =
                    jbd2_descriptor_block_csum32(&self.jbd2_super_block.s_uuid, &revoke_buffer)
                        .ok_or_else(|| {
                            Ext4Error::corrupted().with_operation("jbd2:revoke_checksum")
                        })?;
                revoke_buffer[record_end..].copy_from_slice(&checksum.to_be_bytes());
            }
            let revoke_block = self.set_next_log_block_with_mapping(block_dev, journal_blocks)?;
            block_dev.write(&revoke_buffer, revoke_block, 1)?;
        }
        Ok(())
    }

    fn transition_running_transaction(
        &mut self,
        from: Jbd2RunningTransactionPhase,
        to: Jbd2RunningTransactionPhase,
    ) -> Ext4Result<()> {
        if self.running_transaction.phase != from {
            return Err(Ext4Error::corrupted().with_operation("jbd2:running_transaction_phase"));
        }
        self.running_transaction.phase = to;
        Ok(())
    }

    fn start_committing_transaction(&mut self) -> Ext4Result<bool> {
        if self.committing_transaction.is_some() {
            return Err(Ext4Error::busy().with_operation("jbd2:commit_already_running"));
        }
        if self.running_transaction.phase != Jbd2RunningTransactionPhase::Running {
            return Err(Ext4Error::corrupted().with_operation("jbd2:commit_running_phase"));
        }
        if self.running_transaction.updates.is_empty()
            && self.running_transaction.revoked_blocks.is_empty()
        {
            return Ok(false);
        }

        // The Jbd2Dev owner rejects commit while a scoped handle is active.
        // Exclusive `&mut` access therefore drains the Linux T_LOCKED phase
        // without an OS waitqueue, then closes admission at T_SWITCH before
        // transferring the transaction to the committing owner.
        self.transition_running_transaction(
            Jbd2RunningTransactionPhase::Running,
            Jbd2RunningTransactionPhase::Locked,
        )?;
        self.transition_running_transaction(
            Jbd2RunningTransactionPhase::Locked,
            Jbd2RunningTransactionPhase::Switch,
        )?;
        let running = core::mem::take(&mut self.running_transaction);
        self.committing_transaction = Some(Jbd2CommittingTransaction {
            sequence: self.sequence,
            log_start: self.head,
            phase: Jbd2CommitPhase::Flush,
            updates: running.updates,
            revoked_blocks: running.revoked_blocks,
        });
        Ok(true)
    }

    fn transition_committing_transaction(
        &mut self,
        from: Jbd2CommitPhase,
        to: Jbd2CommitPhase,
    ) -> Ext4Result<()> {
        let transaction = self.committing_transaction.as_mut().ok_or_else(|| {
            Ext4Error::corrupted().with_operation("jbd2:missing_committing_transaction")
        })?;
        if transaction.phase != from {
            return Err(Ext4Error::corrupted().with_operation("jbd2:commit_phase"));
        }
        transaction.phase = to;
        Ok(())
    }

    /// Commits the currently queued metadata updates using the journal inode mapping.
    pub(crate) fn commit_transaction_with_mapping<D: FilesystemBlockIo>(
        &mut self,
        block_dev: &mut D,
        journal_blocks: &[AbsoluteBN],
        commit_time: Jbd2CommitTimestamp,
    ) -> Ext4Result<bool> {
        if !self.start_committing_transaction()? {
            return Ok(false);
        }
        let (tid, update_count, revoked_blocks) = {
            let transaction = self.committing_transaction.as_ref().ok_or_else(|| {
                Ext4Error::corrupted().with_operation("jbd2:missing_committing_transaction")
            })?;
            (
                transaction.sequence,
                transaction.updates.len(),
                transaction.revoked_blocks.clone(),
            )
        };

        let block_size = block_dev.block_size();
        let checksum_mode = self.checksum_mode()?;
        let has_block_checksums = checksum_mode.has_block_checksums();
        let has_64bit = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_64BIT);
        let descriptor_end = if has_block_checksums {
            block_size
                .checked_sub(4)
                .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:descriptor_size"))?
        } else {
            block_size
        };
        let descriptor_capacity = self.jbd2_super_block.descriptor_tag_capacity(block_size)?;
        let revoke_capacity = self.jbd2_super_block.revoke_records_per_block(block_size)?;
        let descriptor_records = if update_count == 0 {
            0
        } else {
            update_count.div_ceil(descriptor_capacity)
        };
        let revoke_records = if revoked_blocks.is_empty() {
            0
        } else {
            revoked_blocks.len().div_ceil(revoke_capacity)
        };
        let required_log_records = update_count
            .checked_add(descriptor_records)
            .and_then(|records| records.checked_add(revoke_records))
            .and_then(|records| records.checked_add(1))
            .ok_or_else(Ext4Error::overflow)?;
        let ring_records = self
            .last_logical_block(journal_blocks)
            .and_then(|last| last.checked_sub(self.jbd2_super_block.s_first))
            .and_then(|records| records.checked_add(1))
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:ring_capacity"))?;
        let used_after_commit = self
            .used_log_records
            .checked_add(required_log_records)
            .ok_or_else(Ext4Error::overflow)?;
        if used_after_commit > usize::try_from(ring_records).map_err(|_| Ext4Error::overflow())? {
            return Err(Ext4Error::no_space().with_operation("jbd2:log_space"));
        }

        // Linux switches the committing transaction's revoke table first and
        // emits those records before descriptor/payload logging.
        self.write_revoke_records(block_dev, journal_blocks, tid, block_size, &revoked_blocks)?;
        self.transition_committing_transaction(Jbd2CommitPhase::Flush, Jbd2CommitPhase::Commit)?;

        let mut compat_checksum = u32::MAX;
        let mut payload_offset = 0usize;
        let mut desc_buffer = if update_count == 0 {
            Vec::new()
        } else {
            vec![0; block_size]
        };
        while payload_offset < update_count {
            let payload_end = payload_offset
                .checked_add(descriptor_capacity)
                .ok_or_else(Ext4Error::overflow)?
                .min(update_count);
            if payload_offset != 0 {
                desc_buffer.fill(0);
            }
            JournalHeaderS {
                h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
                h_sequence: tid,
                ..Default::default()
            }
            .to_disk_bytes(&mut desc_buffer[0..JournalHeaderS::disk_size()]);

            let mut current_offset = JBD2_DESCRIPTOR_HEADER_SIZE;
            for update_index in payload_offset..payload_end {
                let transaction = self.committing_transaction.as_ref().ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("jbd2:missing_committing_transaction")
                })?;
                let update = &transaction.updates[update_index];
                let payload = journal_payload(update, block_size)?;
                let target_raw = update.0.raw();
                let block_high = (target_raw >> 32) as u32;
                if !has_64bit && block_high != 0 {
                    return Err(Ext4Error::unsupported().with_operation("jbd2:64bit_block_number"));
                }
                let mut flags = if payload.is_escaped() {
                    u32::from(JOURNAL_ESCAPE)
                } else {
                    0
                };
                if update_index + 1 == payload_end {
                    flags |= u32::from(JBD2_FLAG_LAST_TAG);
                }
                if update_index != payload_offset {
                    flags |= u32::from(JBD2_FLAG_SAME_UUID);
                }

                if checksum_mode == Jbd2ChecksumMode::CsumV3 {
                    JournalBlockTag3S {
                        t_blocknr: target_raw as u32,
                        t_flags: flags,
                        t_blocknr_high: block_high,
                        t_checksum: jbd2_tag_csum32(
                            &self.jbd2_super_block.s_uuid,
                            tid,
                            payload.bytes(),
                        ),
                    }
                    .to_disk_bytes(
                        &mut desc_buffer[current_offset..current_offset + JBD2_TAG3_SIZE],
                    );
                    current_offset += JBD2_TAG3_SIZE;
                } else {
                    JournalBlockTagS {
                        t_blocknr: target_raw as u32,
                        t_checksum: if checksum_mode == Jbd2ChecksumMode::CsumV2 {
                            jbd2_tag_csum32(&self.jbd2_super_block.s_uuid, tid, payload.bytes())
                                as u16
                        } else {
                            0
                        },
                        t_flags: flags as u16,
                    }
                    .to_disk_bytes(
                        &mut desc_buffer[current_offset..current_offset + JBD2_TAG_SIZE],
                    );
                    current_offset += JBD2_TAG_SIZE;
                    if has_64bit {
                        desc_buffer[current_offset..current_offset + JBD2_TAG_BLOCKNR_HIGH_SIZE]
                            .copy_from_slice(&block_high.to_be_bytes());
                        current_offset += JBD2_TAG_BLOCKNR_HIGH_SIZE;
                    }
                    if checksum_mode == Jbd2ChecksumMode::CsumV2 {
                        current_offset = current_offset
                            .checked_add(core::mem::size_of::<u16>())
                            .ok_or_else(Ext4Error::overflow)?;
                    }
                }

                if update_index == payload_offset {
                    desc_buffer[current_offset..current_offset + JBD2_UUID_SIZE]
                        .copy_from_slice(&self.jbd2_super_block.s_uuid);
                    current_offset += JBD2_UUID_SIZE;
                }
            }

            if current_offset > descriptor_end {
                return Err(Ext4Error::no_space().with_operation("jbd2:descriptor_full"));
            }
            if has_block_checksums {
                let checksum =
                    jbd2_descriptor_block_csum32(&self.jbd2_super_block.s_uuid, &desc_buffer)
                        .ok_or_else(|| {
                            Ext4Error::corrupted().with_operation("jbd2:descriptor_checksum")
                        })?;
                desc_buffer[descriptor_end..].copy_from_slice(&checksum.to_be_bytes());
            }

            // Linux interleaves every descriptor with the payload blocks
            // described by that descriptor, then writes one final commit.
            let descriptor_block =
                self.set_next_log_block_with_mapping(block_dev, journal_blocks)?;
            if checksum_mode == Jbd2ChecksumMode::CompatChecksum {
                compat_checksum = jbd2_compat_checksum_append(compat_checksum, &desc_buffer);
            }
            block_dev.write(&desc_buffer, descriptor_block, 1)?;
            for update_index in payload_offset..payload_end {
                let payload_block =
                    self.set_next_log_block_with_mapping(block_dev, journal_blocks)?;
                let transaction = self.committing_transaction.as_ref().ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("jbd2:missing_committing_transaction")
                })?;
                let payload = journal_payload(&transaction.updates[update_index], block_size)?;
                if checksum_mode == Jbd2ChecksumMode::CompatChecksum {
                    compat_checksum = jbd2_compat_checksum_append(compat_checksum, payload.bytes());
                }
                block_dev.write(payload.bytes(), payload_block, 1)?;
            }
            payload_offset = payload_end;
        }

        self.transition_committing_transaction(
            Jbd2CommitPhase::Commit,
            Jbd2CommitPhase::DataFlush,
        )?;
        block_dev.flush()?;
        self.transition_committing_transaction(
            Jbd2CommitPhase::DataFlush,
            Jbd2CommitPhase::JournalFlush,
        )?;

        // Write the commit block BEFORE checkpointing so that a crash during
        // checkpoint still leaves a valid committed transaction in the journal
        // for replay on the next mount.
        let mut commit_buffer = if desc_buffer.is_empty() {
            vec![0_u8; block_size]
        } else {
            desc_buffer
        };
        commit_buffer.fill(0);

        let mut commit_block = CommitHeader {
            h_header: JournalHeaderS {
                h_magic: JBD2_MAGIC,
                h_blocktype: JBD2_BLOCKTYPE_COMMIT,
                h_sequence: tid,
            },
            h_chksum_type: 0,
            h_chksum_size: 0,
            h_padding: [0; 2],
            h_chksum: [0; 8],
            h_commit_sec: commit_time.seconds,
            h_commit_nsec: commit_time.nanoseconds,
        };

        commit_block.to_disk_bytes(&mut commit_buffer);
        match checksum_mode {
            Jbd2ChecksumMode::CompatChecksum => {
                commit_block.h_chksum_type = JBD2_CRC32_CHKSUM;
                commit_block.h_chksum_size = JBD2_CRC32_CHKSUM_SIZE;
                commit_block.h_chksum[0] = compat_checksum;
                commit_block.to_disk_bytes(&mut commit_buffer);
            }
            Jbd2ChecksumMode::CsumV2 | Jbd2ChecksumMode::CsumV3 => {
                let checksum =
                    jbd2_commit_block_csum32(&self.jbd2_super_block.s_uuid, &commit_buffer)
                        .ok_or_else(|| {
                            Ext4Error::corrupted().with_operation("jbd2:commit_checksum")
                        })?;
                commit_block.h_chksum[0] = checksum;
                commit_block.to_disk_bytes(&mut commit_buffer);
            }
            Jbd2ChecksumMode::None => {}
        }
        let commit_block_id = self.set_next_log_block_with_mapping(block_dev, journal_blocks)?;

        block_dev.write_with_flags(
            &commit_buffer,
            commit_block_id,
            1,
            WriteFlags::METADATA | WriteFlags::FUA,
        )?;
        let transaction = self.committing_transaction.take().ok_or_else(|| {
            Ext4Error::corrupted().with_operation("jbd2:missing_committing_transaction")
        })?;
        if transaction.phase != Jbd2CommitPhase::JournalFlush
            || transaction.sequence != self.sequence
        {
            return Err(Ext4Error::corrupted().with_operation("jbd2:commit_completion_state"));
        }
        self.sequence = transaction.sequence.wrapping_add(1);

        self.checkpoint_transactions
            .push(Jbd2CheckpointTransaction {
                sequence: transaction.sequence,
                log_start: transaction.log_start,
                log_records: required_log_records,
                updates: transaction.updates,
                revoked_blocks: transaction.revoked_blocks,
            });

        Ok(true)
    }

    /// Checkpoints an oldest-first prefix of committed transactions and
    /// advances the durable tail once while leaving every later transaction
    /// replayable.
    pub(crate) fn checkpoint_transactions_with_mapping<D: FilesystemBlockIo>(
        &mut self,
        block_dev: &mut D,
        journal_blocks: &[AbsoluteBN],
        max_transactions: usize,
    ) -> Ext4Result<bool> {
        let completed_transactions = max_transactions.min(self.checkpoint_transactions.len());
        if completed_transactions == 0 {
            return Ok(false);
        }

        let mut later_blocks = BTreeSet::new();
        for transaction in &self.checkpoint_transactions[completed_transactions..] {
            later_blocks.extend(transaction.updates.iter().map(|update| update.0));
            later_blocks.extend(transaction.revoked_blocks.iter().copied());
        }
        for (reverse_index, transaction) in self.checkpoint_transactions[..completed_transactions]
            .iter()
            .rev()
            .enumerate()
        {
            for update in &transaction.updates {
                if !later_blocks.contains(&update.0) {
                    block_dev.write(&update.1[..], update.0, 1)?;
                }
            }
            // The set only filters transactions that are still to be scanned.
            // Do not populate it after the oldest selected transaction: the
            // common single-transaction checkpoint has no earlier image that
            // could be hidden by these blocks.
            if reverse_index + 1 < completed_transactions {
                later_blocks.extend(transaction.updates.iter().map(|update| update.0));
                later_blocks.extend(transaction.revoked_blocks.iter().copied());
            }
        }
        block_dev.flush()?;

        let completed_records = self.checkpoint_transactions[..completed_transactions]
            .iter()
            .try_fold(0usize, |records, transaction| {
                records
                    .checked_add(transaction.log_records)
                    .ok_or_else(Ext4Error::overflow)
            })?;
        let remaining_records = self
            .used_log_records
            .checked_sub(completed_records)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:log_accounting"))?;
        let (next_sequence, next_start) = self
            .checkpoint_transactions
            .get(completed_transactions)
            .map_or((self.sequence, 0), |next| (next.sequence, next.log_start));
        let previous_superblock = self.jbd2_super_block;
        self.jbd2_super_block.s_sequence = next_sequence;
        self.jbd2_super_block.s_start = next_start;
        if let Err(error) = self.write_journal_superblock_with_mapping_flags(
            block_dev,
            journal_blocks,
            WriteFlags::METADATA | WriteFlags::FUA,
        ) {
            self.jbd2_super_block = previous_superblock;
            return Err(error);
        }

        self.checkpoint_transactions.drain(..completed_transactions);
        self.used_log_records = remaining_records;

        Ok(true)
    }

    fn checksum_failure_or_stale_end(
        failure: ReplayFailure,
        commit_time: u64,
        last_commit_time: u64,
    ) -> ReplayScan {
        if commit_time < last_commit_time {
            ReplayScan::CleanEnd
        } else {
            ReplayScan::Incomplete(failure)
        }
    }

    fn scan_one_transaction<D: FilesystemBlockIo>(
        &self,
        block_dev: &mut D,
        ring: &ReplayRing<'_>,
        start_rel: u32,
        expect_seq: u32,
        last_commit_time: u64,
    ) -> ReplayScan {
        let mut record_rel = start_rel;
        let mut payloads: Vec<ReplayPayload> = Vec::new();
        let mut revoked_blocks = Vec::new();
        let Some(max_records) = ring
            .last_rel
            .checked_sub(ring.first_rel)
            .and_then(|records| records.checked_add(1))
        else {
            return ReplayScan::Incomplete(ReplayFailure::at(
                JournalReplayPhase::Initialize,
                Ext4Error::corrupted().with_operation("jbd2:replay_ring_geometry"),
                start_rel,
            ));
        };
        let block_size = block_dev.block_size();
        if block_size < JBD2_COMMIT_HEADER_SIZE {
            return ReplayScan::Incomplete(ReplayFailure::at(
                JournalReplayPhase::Initialize,
                Ext4Error::corrupted().with_operation("jbd2:replay_block_size"),
                start_rel,
            ));
        }
        let checksum_mode = match self.checksum_mode() {
            Ok(mode) => mode,
            Err(error) => {
                return ReplayScan::Incomplete(ReplayFailure::at(
                    JournalReplayPhase::Initialize,
                    error,
                    start_rel,
                ));
            }
        };
        let mut compat_checksum = u32::MAX;
        let mut deferred_checksum_failure = None;

        for _ in 0..max_records {
            let record_phys = match ring.phys(record_rel) {
                Ok(block) => block,
                Err(error) => {
                    return ReplayScan::Incomplete(ReplayFailure::at(
                        JournalReplayPhase::Scan,
                        error,
                        start_rel,
                    ));
                }
            };
            let mut record_buf = vec![0u8; block_size];
            if let Err(error) = block_dev.read(&mut record_buf, record_phys, 1) {
                return ReplayScan::Incomplete(ReplayFailure::at(
                    JournalReplayPhase::Scan,
                    error,
                    start_rel,
                ));
            }

            let hdr = JournalHeaderS::from_disk_bytes(&record_buf[0..JBD2_DESCRIPTOR_HEADER_SIZE]);

            if hdr.h_magic != JBD2_MAGIC || hdr.h_sequence != expect_seq {
                return ReplayScan::CleanEnd;
            }

            match hdr.h_blocktype {
                JBD2_BLOCKTYPE_DESCRIPTOR => {
                    let descriptor = match self.parse_replay_tags(&record_buf) {
                        Ok(descriptor) => descriptor,
                        Err(error) => {
                            return ReplayScan::Incomplete(ReplayFailure::at(
                                JournalReplayPhase::Scan,
                                error,
                                start_rel,
                            ));
                        }
                    };
                    let ReplayDescriptor::Tagged {
                        tags,
                        checksum_valid,
                    } = descriptor
                    else {
                        return ReplayScan::CleanEnd;
                    };
                    if !checksum_valid && deferred_checksum_failure.is_none() {
                        deferred_checksum_failure = Some(ReplayFailure::at(
                            JournalReplayPhase::Scan,
                            Ext4Error::checksum().with_operation("jbd2:replay_descriptor_checksum"),
                            start_rel,
                        ));
                    }
                    if checksum_mode == Jbd2ChecksumMode::CompatChecksum {
                        compat_checksum = jbd2_compat_checksum_append(compat_checksum, &record_buf);
                    }

                    for tag in tags {
                        ring.advance(&mut record_rel);
                        let payload_phys = match ring.phys(record_rel) {
                            Ok(block) => block,
                            Err(error) => {
                                return ReplayScan::Incomplete(ReplayFailure::at(
                                    JournalReplayPhase::Scan,
                                    error,
                                    start_rel,
                                ));
                            }
                        };
                        if checksum_mode == Jbd2ChecksumMode::CompatChecksum {
                            let mut payload = vec![0u8; block_size];
                            if let Err(error) = block_dev.read(&mut payload, payload_phys, 1) {
                                return ReplayScan::Incomplete(ReplayFailure::at(
                                    JournalReplayPhase::Replay,
                                    error,
                                    start_rel,
                                ));
                            }
                            compat_checksum =
                                jbd2_compat_checksum_append(compat_checksum, &payload);
                        }
                        payloads.push(ReplayPayload {
                            tag,
                            journal_rel: record_rel,
                        });
                    }
                }
                JBD2_BLOCKTYPE_COMMIT => {
                    let commit = CommitHeader::from_disk_bytes(&record_buf);
                    if let Some(failure) = deferred_checksum_failure {
                        return Self::checksum_failure_or_stale_end(
                            failure,
                            commit.h_commit_sec,
                            last_commit_time,
                        );
                    }
                    match checksum_mode {
                        Jbd2ChecksumMode::CompatChecksum => {
                            let checked = commit.h_chksum_type == JBD2_CRC32_CHKSUM
                                && commit.h_chksum_size == JBD2_CRC32_CHKSUM_SIZE
                                && commit.h_chksum[0] == compat_checksum;
                            let unused = commit.h_chksum_type == 0
                                && commit.h_chksum_size == 0
                                && commit.h_chksum[0] == 0;
                            if !checked && !unused {
                                return Self::checksum_failure_or_stale_end(
                                    ReplayFailure::at(
                                        JournalReplayPhase::Replay,
                                        Ext4Error::checksum()
                                            .with_operation("jbd2:replay_compat_checksum"),
                                        start_rel,
                                    ),
                                    commit.h_commit_sec,
                                    last_commit_time,
                                );
                            }
                        }
                        Jbd2ChecksumMode::CsumV2 | Jbd2ChecksumMode::CsumV3 => {
                            let computed = jbd2_commit_block_csum32(
                                &self.jbd2_super_block.s_uuid,
                                &record_buf,
                            );
                            let stored = commit.h_chksum[0];
                            if computed != Some(stored)
                                && jbd2_partial_commit_block_csum32(
                                    &self.jbd2_super_block.s_uuid,
                                    &record_buf,
                                ) != Some(stored)
                            {
                                return Self::checksum_failure_or_stale_end(
                                    ReplayFailure::at(
                                        JournalReplayPhase::Replay,
                                        Ext4Error::checksum()
                                            .with_operation("jbd2:replay_commit_checksum"),
                                        start_rel,
                                    ),
                                    commit.h_commit_sec,
                                    last_commit_time,
                                );
                            }
                        }
                        Jbd2ChecksumMode::None => {}
                    }

                    let mut next_rel = record_rel;
                    ring.advance(&mut next_rel);
                    return ReplayScan::Committed(ReplayTransaction {
                        start_rel,
                        sequence: expect_seq,
                        next_rel,
                        commit_time: commit.h_commit_sec,
                        payloads,
                        revoked_blocks,
                    });
                }
                JBD2_BLOCKTYPE_REVOKE => {
                    let revoke = match self.parse_revoke_blocks(&record_buf) {
                        Ok(revoke) => revoke,
                        Err(error) => {
                            return ReplayScan::Incomplete(ReplayFailure::at(
                                JournalReplayPhase::Revoke,
                                error,
                                start_rel,
                            ));
                        }
                    };
                    if !revoke.checksum_valid && deferred_checksum_failure.is_none() {
                        deferred_checksum_failure = Some(ReplayFailure::at(
                            JournalReplayPhase::Revoke,
                            Ext4Error::checksum().with_operation("jbd2:replay_revoke_checksum"),
                            start_rel,
                        ));
                    }
                    revoked_blocks.extend(revoke.blocks);
                }
                _ => {
                    return ReplayScan::CleanEnd;
                }
            }

            ring.advance(&mut record_rel);
        }

        ReplayScan::Incomplete(ReplayFailure::at(
            JournalReplayPhase::Scan,
            Ext4Error::corrupted().with_operation("jbd2:replay_ring_exhausted"),
            start_rel,
        ))
    }

    fn transaction_id_after(candidate: u32, reference: u32) -> bool {
        (candidate.wrapping_sub(reference) as i32) > 0
    }

    fn build_revoke_table(transactions: &[ReplayTransaction]) -> BTreeMap<AbsoluteBN, u32> {
        let mut revoke_table = BTreeMap::new();
        for transaction in transactions {
            for &block in &transaction.revoked_blocks {
                revoke_table
                    .entry(block)
                    .and_modify(|sequence| {
                        if Self::transaction_id_after(transaction.sequence, *sequence) {
                            *sequence = transaction.sequence;
                        }
                    })
                    .or_insert(transaction.sequence);
            }
        }
        revoke_table
    }

    fn payload_is_revoked(
        revoke_table: &BTreeMap<AbsoluteBN, u32>,
        block: AbsoluteBN,
        payload_sequence: u32,
    ) -> bool {
        revoke_table.get(&block).is_some_and(|revoke_sequence| {
            !Self::transaction_id_after(payload_sequence, *revoke_sequence)
        })
    }

    fn allocate_replay_buffer(
        byte_capacity: usize,
        restart_rel: u32,
        operation: &'static str,
    ) -> Result<Vec<u8>, ReplayFailure> {
        let mut buffer = Vec::new();
        buffer.try_reserve_exact(byte_capacity).map_err(|_| {
            ReplayFailure::at(
                JournalReplayPhase::Replay,
                Ext4Error::no_memory().with_operation(operation),
                restart_rel,
            )
        })?;
        Ok(buffer)
    }

    fn read_replay_payload<D: FilesystemBlockIo>(
        &self,
        block_dev: &mut D,
        ring: &ReplayRing<'_>,
        transaction: &ReplayTransaction,
        payload: ReplayPayload,
        data: &mut [u8],
    ) -> Result<(), ReplayFailure> {
        let meta_phys = ring.phys(payload.journal_rel).map_err(|error| {
            ReplayFailure::at(JournalReplayPhase::Replay, error, transaction.start_rel)
        })?;
        block_dev.read(data, meta_phys, 1).map_err(|error| {
            ReplayFailure::at(JournalReplayPhase::Replay, error, transaction.start_rel)
        })?;

        if let Some(stored) = payload.tag.checksum {
            let computed =
                jbd2_tag_csum32(&self.jbd2_super_block.s_uuid, transaction.sequence, data);
            let matches = match stored {
                ReplayChecksum::CsumV2(stored) => computed as u16 == stored,
                ReplayChecksum::CsumV3(stored) => computed == stored,
            };
            if !matches {
                return Err(ReplayFailure::at(
                    JournalReplayPhase::Replay,
                    Ext4Error::checksum().with_operation("jbd2:replay_payload_checksum"),
                    transaction.start_rel,
                ));
            }
        }
        Ok(())
    }

    fn write_replay_batch<D: FilesystemBlockIo>(
        block_dev: &mut D,
        first_home: Option<AbsoluteBN>,
        block_count: usize,
        data: &[u8],
        restart_rel: u32,
    ) -> Result<(), ReplayFailure> {
        if block_count == 0 {
            return Ok(());
        }
        let first_home = first_home.ok_or_else(|| {
            ReplayFailure::at(
                JournalReplayPhase::Replay,
                Ext4Error::corrupted().with_operation("jbd2:replay_batch_without_home"),
                restart_rel,
            )
        })?;
        let block_count = u32::try_from(block_count).map_err(|_| {
            ReplayFailure::at(
                JournalReplayPhase::Replay,
                Ext4Error::overflow(),
                restart_rel,
            )
        })?;
        block_dev
            .write(data, first_home, block_count)
            .map_err(|error| ReplayFailure::at(JournalReplayPhase::Replay, error, restart_rel))
    }

    fn replay_transaction<D: FilesystemBlockIo>(
        &self,
        block_dev: &mut D,
        ring: &ReplayRing<'_>,
        transaction: &ReplayTransaction,
        revoke_table: &BTreeMap<AbsoluteBN, u32>,
    ) -> Result<(), ReplayFailure> {
        let block_size = block_dev.block_size();
        let replayable_payloads = transaction
            .payloads
            .iter()
            .filter(|payload| {
                !Self::payload_is_revoked(revoke_table, payload.tag.block, transaction.sequence)
            })
            .count();
        if replayable_payloads == 0 {
            return Ok(());
        }

        // Validate every non-revoked payload in this transaction before the
        // first home-block write. Corruption must not partially replay one
        // transaction.
        let mut payload_data = Self::allocate_replay_buffer(
            block_size,
            transaction.start_rel,
            "jbd2:replay_payload_buffer",
        )?;
        payload_data.resize(block_size, 0);
        for payload in transaction.payloads.iter().copied() {
            if Self::payload_is_revoked(revoke_table, payload.tag.block, transaction.sequence) {
                continue;
            }
            self.read_replay_payload(block_dev, ring, transaction, payload, &mut payload_data)?;
        }

        let buffered_blocks = core::cmp::min(replayable_payloads, MAX_BUFFERED_WRITE_BLOCKS);
        let batch_capacity = block_size.checked_mul(buffered_blocks).ok_or_else(|| {
            ReplayFailure::at(
                JournalReplayPhase::Replay,
                Ext4Error::overflow(),
                transaction.start_rel,
            )
        })?;
        let mut batch = Self::allocate_replay_buffer(
            batch_capacity,
            transaction.start_rel,
            "jbd2:replay_batch_buffer",
        )?;
        let mut first_home: Option<AbsoluteBN> = None;
        let mut batch_blocks = 0usize;

        for payload in transaction.payloads.iter().copied() {
            if Self::payload_is_revoked(revoke_table, payload.tag.block, transaction.sequence) {
                continue;
            }

            let contiguous = if let Some(batch_start) = first_home {
                let expected = batch_start
                    .checked_add_usize(batch_blocks)
                    .map_err(|error| {
                        ReplayFailure::at(JournalReplayPhase::Replay, error, transaction.start_rel)
                    })?;
                payload.tag.block == expected
            } else {
                true
            };
            if batch_blocks == MAX_BUFFERED_WRITE_BLOCKS || !contiguous {
                Self::write_replay_batch(
                    block_dev,
                    first_home,
                    batch_blocks,
                    &batch,
                    transaction.start_rel,
                )?;
                batch.clear();
                first_home = None;
                batch_blocks = 0;
            }

            self.read_replay_payload(block_dev, ring, transaction, payload, &mut payload_data)?;
            if (payload.tag.flags & u32::from(JOURNAL_ESCAPE)) != 0 {
                let magic = payload_data.get_mut(..4).ok_or_else(|| {
                    ReplayFailure::at(
                        JournalReplayPhase::Replay,
                        Ext4Error::corrupted().with_operation("jbd2:replay_escape_block_size"),
                        transaction.start_rel,
                    )
                })?;
                magic.copy_from_slice(&JBD2_MAGIC.to_be_bytes());
            }
            if first_home.is_none() {
                first_home = Some(payload.tag.block);
            }
            batch.extend_from_slice(&payload_data);
            batch_blocks = batch_blocks.checked_add(1).ok_or_else(|| {
                ReplayFailure::at(
                    JournalReplayPhase::Replay,
                    Ext4Error::overflow(),
                    transaction.start_rel,
                )
            })?;
        }
        Self::write_replay_batch(
            block_dev,
            first_home,
            batch_blocks,
            &batch,
            transaction.start_rel,
        )
    }

    /// Replays committed transactions using the journal inode logical-block map.
    pub(crate) fn replay_with_mapping<D: FilesystemBlockIo>(
        &mut self,
        block_dev: &mut D,
        journal_blocks: &[AbsoluteBN],
    ) -> ReplayStatus {
        let initial_rel = self.jbd2_super_block.s_start;
        if initial_rel == 0 {
            return ReplayStatus::Complete;
        }

        if !journal_blocks.is_empty() && initial_rel as usize >= journal_blocks.len() {
            return ReplayStatus::Incomplete(ReplayFailure::at(
                JournalReplayPhase::Initialize,
                Ext4Error::corrupted().with_operation("jbd2:replay_start_mapping"),
                initial_rel,
            ));
        }

        let maxlen = self.jbd2_super_block.s_maxlen;
        if maxlen == 0 {
            return ReplayStatus::Incomplete(ReplayFailure::at(
                JournalReplayPhase::Initialize,
                Ext4Error::corrupted().with_operation("jbd2:replay_maxlen"),
                initial_rel,
            ));
        }
        let Some(ring) = ReplayRing::new(self, journal_blocks) else {
            return ReplayStatus::Incomplete(ReplayFailure::at(
                JournalReplayPhase::Initialize,
                Ext4Error::corrupted().with_operation("jbd2:replay_ring"),
                initial_rel,
            ));
        };
        let initial_sequence = self.jbd2_super_block.s_sequence;
        let mut journal_rel = initial_rel;
        let mut expect_seq = initial_sequence;
        let mut transactions = Vec::new();
        let mut last_commit_time = 0;

        // Pass 1: discover and validate the complete committed transaction
        // range. No home block is written in this pass.
        let scan_failure = loop {
            match self.scan_one_transaction(
                block_dev,
                &ring,
                journal_rel,
                expect_seq,
                last_commit_time,
            ) {
                ReplayScan::Committed(transaction) => {
                    journal_rel = transaction.next_rel;
                    expect_seq = transaction.sequence.wrapping_add(1);
                    last_commit_time = transaction.commit_time;
                    transactions.push(transaction);
                }
                ReplayScan::CleanEnd => break None,
                ReplayScan::Incomplete(failure) => break Some(failure),
            }
        };

        let status = if let Some(failure) = scan_failure {
            // None of the scanned transactions has reached its home block yet,
            // so persist the original restart point even when the diagnostic
            // failure belongs to a later transaction.
            self.jbd2_super_block.s_start = initial_rel;
            self.jbd2_super_block.s_sequence = initial_sequence;
            self.sequence = initial_sequence;
            ReplayStatus::Incomplete(failure.with_restart_rel(initial_rel))
        } else {
            // Pass 2: retain the latest revoke transaction for every block.
            let revoke_table = Self::build_revoke_table(&transactions);
            let mut replay_failure = None;

            // Pass 3: apply payloads in transaction order, consulting the
            // sequence-aware global revoke table built from the whole log.
            for transaction in &transactions {
                if let Err(failure) =
                    self.replay_transaction(block_dev, &ring, transaction, &revoke_table)
                {
                    self.jbd2_super_block.s_start =
                        failure.restart_rel().unwrap_or(transaction.start_rel);
                    self.jbd2_super_block.s_sequence = transaction.sequence;
                    self.sequence = transaction.sequence;
                    replay_failure = Some(failure);
                    break;
                }
            }

            if let Some(failure) = replay_failure {
                ReplayStatus::Incomplete(failure)
            } else {
                self.jbd2_super_block.s_start = 0;
                self.jbd2_super_block.s_sequence = expect_seq;
                self.sequence = expect_seq;
                ReplayStatus::Complete
            }
        };

        self.head = self.jbd2_super_block.s_first;

        // Preserve the replay cause if recording progress also fails. The
        // persistence error is secondary and must not replace the first error.
        match self
            .write_journal_superblock_with_mapping(block_dev, journal_blocks)
            .and_then(|()| block_dev.flush())
        {
            Ok(()) => status,
            Err(error) => match status {
                ReplayStatus::Complete => ReplayStatus::Incomplete(ReplayFailure::without_restart(
                    JournalReplayPhase::Persist,
                    error,
                )),
                ReplayStatus::Incomplete(failure) => {
                    ReplayStatus::Incomplete(failure.with_persistence_error(error))
                }
            },
        }
    }
}

/// Creates the journal inode and writes its initial journal superblock.
pub fn create_journal_entry<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
) -> Ext4Result<()> {
    // Allocate the journal area. Block 0 stores the journal superblock and the
    // remaining blocks hold descriptor/data/commit traffic.
    let journal_inode_num = JOURNAL_FILE_INODE;
    let block_size = fs.block_size();
    let free_block = fs.alloc_blocks(block_dev, CREATED_JOURNAL_BLOCK_COUNT)?;

    // Ensure journal area starts clean: otherwise old image contents could look like valid
    // descriptor/commit blocks and replay would corrupt filesystem metadata.
    let zero = vec![0u8; block_size];
    for &b in free_block.iter() {
        block_dev.write_blocks(&zero, b, 1, true)?;
    }
    // Build the journal inode metadata and map the allocated journal blocks.
    let mut jour_inode = Ext4Inode::empty_for_reuse(fs.default_inode_extra_isize());
    jour_inode.i_links_count = 1;

    let inode_size = block_size
        .checked_mul(free_block.len())
        .ok_or_else(Ext4Error::overflow)?;
    jour_inode.i_size_lo = inode_size as u32;
    jour_inode.i_size_high = 0;
    jour_inode.i_blocks_lo = (inode_size / 512) as u32;
    jour_inode.l_i_blocks_high = 0;
    jour_inode.write_extend_header();
    build_file_block_mapping_with_inode_num(
        fs,
        &mut jour_inode,
        InodeNumber::new(journal_inode_num as u32)?,
        &free_block,
        block_dev,
    )?;
    fs.finalize_inode_update(
        block_dev,
        InodeNumber::new(journal_inode_num as u32)?,
        &mut jour_inode,
        Ext4InodeMetadataUpdate::create(Ext4Inode::S_IFREG | 0o600),
    )?;

    let mut jbd2_sb = JournalSuperBlock::default();
    jbd2_sb.s_feature_incompat |= JBD2_FEATURE_INCOMPAT_REVOKE;

    if fs
        .superblock
        .has_feature_incompat(crate::superblock::Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT)
    {
        jbd2_sb.s_feature_incompat |= JBD2_FEATURE_INCOMPAT_64BIT;
    }
    if ext4_superblock_has_metadata_csum(&fs.superblock) {
        jbd2_sb.s_feature_incompat |= JBD2_FEATURE_INCOMPAT_CSUM_V3;
        jbd2_sb.s_checksum_type = JBD2_CRC32C_CHKSUM;
    } else {
        jbd2_sb.s_checksum_type = 0;
    }

    // The first allocated block stores the journal superblock itself. JBD2
    // counts it in `s_maxlen` and starts log traffic at relative block 1.
    jbd2_sb.s_maxlen = u32::try_from(free_block.len()).map_err(|_| Ext4Error::overflow())?;
    jbd2_sb.s_start = 0;
    jbd2_sb.s_blocksize = u32::try_from(block_size).map_err(|_| Ext4Error::overflow())?;
    jbd2_sb.s_sequence = 1;
    jbd2_sb.s_first = 1;
    jbd2_sb.s_uuid = fs.superblock.s_uuid;
    jbd2_update_superblock_checksum(&mut jbd2_sb);
    let mut journal_superblock_bytes = vec![0u8; block_size];
    jbd2_sb.encode_checked(&mut journal_superblock_bytes)?;

    fs.datablock_cache
        .modify_new(block_dev, free_block[0], |data| {
            data.copy_from_slice(&journal_superblock_bytes);
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;
    use crate::{
        config::BLOCK_SIZE,
        io::{DeviceCapabilities, DeviceGeometry, SectorId},
        runtime::Clock,
    };

    const JOURNAL_START: u64 = 128;
    const JOURNAL_LEN: u32 = 16;
    const HOME_BLOCK: u64 = 10;

    struct ReplayDevice {
        blocks: Vec<u8>,
    }

    struct SmallReplayIo {
        block_size: usize,
        blocks: Vec<u8>,
        writes: Vec<(AbsoluteBN, u32)>,
    }

    impl ReplayDevice {
        fn new(block_count: usize) -> Self {
            Self {
                blocks: vec![0; block_count * BLOCK_SIZE],
            }
        }

        fn block_mut(&mut self, block: u64) -> &mut [u8] {
            let start = usize::try_from(block).expect("test block fits usize") * BLOCK_SIZE;
            &mut self.blocks[start..start + BLOCK_SIZE]
        }

        fn block(&self, block: u64) -> &[u8] {
            let start = usize::try_from(block).expect("test block fits usize") * BLOCK_SIZE;
            &self.blocks[start..start + BLOCK_SIZE]
        }
    }

    impl BlockIo for ReplayDevice {
        fn read(&mut self, buffer: &mut [u8], block: SectorId, _count: u32) -> Ext4Result<()> {
            let start = block.as_usize()? * BLOCK_SIZE;
            let end = start
                .checked_add(buffer.len())
                .ok_or_else(Ext4Error::overflow)?;
            buffer.copy_from_slice(
                self.blocks
                    .get(start..end)
                    .ok_or_else(Ext4Error::invalid_input)?,
            );
            Ok(())
        }

        fn write(&mut self, buffer: &[u8], block: SectorId, _count: u32) -> Ext4Result<()> {
            let start = block.as_usize()? * BLOCK_SIZE;
            let end = start
                .checked_add(buffer.len())
                .ok_or_else(Ext4Error::overflow)?;
            self.blocks
                .get_mut(start..end)
                .ok_or_else(Ext4Error::invalid_input)?
                .copy_from_slice(buffer);
            Ok(())
        }

        fn geometry(&self) -> DeviceGeometry {
            DeviceGeometry::new(BLOCK_SIZE as u32, (self.blocks.len() / BLOCK_SIZE) as u64)
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

    impl Clock for ReplayDevice {
        fn now(&self) -> Ext4Result<Ext4Timestamp> {
            Ok(Ext4Timestamp::new(0, 0))
        }
    }

    impl FilesystemBlockIo for SmallReplayIo {
        fn block_size(&self) -> usize {
            self.block_size
        }

        fn read(&mut self, buffer: &mut [u8], block: AbsoluteBN, _count: u32) -> Ext4Result<()> {
            let start = block.as_usize()? * self.block_size;
            let end = start
                .checked_add(buffer.len())
                .ok_or_else(Ext4Error::overflow)?;
            buffer.copy_from_slice(
                self.blocks
                    .get(start..end)
                    .ok_or_else(Ext4Error::invalid_input)?,
            );
            Ok(())
        }

        fn write(&mut self, _buffer: &[u8], block: AbsoluteBN, count: u32) -> Ext4Result<()> {
            self.writes.push((block, count));
            Ok(())
        }

        fn write_with_flags(
            &mut self,
            buffer: &[u8],
            block: AbsoluteBN,
            count: u32,
            _flags: WriteFlags,
        ) -> Ext4Result<()> {
            self.write(buffer, block, count)
        }

        fn flush(&mut self) -> Ext4Result<()> {
            Ok(())
        }
    }

    fn replay_superblock() -> JournalSuperBlock {
        JournalSuperBlock {
            s_maxlen: JOURNAL_LEN,
            s_first: 1,
            s_start: 1,
            s_sequence: 1,
            ..JournalSuperBlock::default()
        }
    }

    fn replay_csum_v3_superblock() -> JournalSuperBlock {
        let mut superblock = replay_superblock();
        superblock.s_feature_incompat = JBD2_FEATURE_INCOMPAT_64BIT | JBD2_FEATURE_INCOMPAT_CSUM_V3;
        superblock.s_checksum_type = JBD2_CRC32C_CHKSUM;
        superblock.s_uuid = [0x5a; JBD2_UUID_SIZE];
        jbd2_update_superblock_checksum(&mut superblock);
        superblock
    }

    fn write_descriptor(device: &mut ReplayDevice, relative: u32, sequence: u32) {
        let block = device.block_mut(JOURNAL_START + u64::from(relative));
        JournalHeaderS {
            h_magic: JBD2_MAGIC,
            h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
            h_sequence: sequence,
        }
        .to_disk_bytes(&mut block[..JBD2_DESCRIPTOR_HEADER_SIZE]);
        JournalBlockTagS {
            t_blocknr: HOME_BLOCK as u32,
            t_checksum: 0,
            t_flags: JBD2_FLAG_LAST_TAG,
        }
        .to_disk_bytes(
            &mut block[JBD2_DESCRIPTOR_HEADER_SIZE..JBD2_DESCRIPTOR_HEADER_SIZE + JBD2_TAG_SIZE],
        );
    }

    fn write_commit(device: &mut ReplayDevice, relative: u32, sequence: u32) {
        JournalHeaderS {
            h_magic: JBD2_MAGIC,
            h_blocktype: JBD2_BLOCKTYPE_COMMIT,
            h_sequence: sequence,
        }
        .to_disk_bytes(
            &mut device.block_mut(JOURNAL_START + u64::from(relative))
                [..JBD2_DESCRIPTOR_HEADER_SIZE],
        );
    }

    fn write_revoke(device: &mut ReplayDevice, relative: u32, sequence: u32) {
        let block = device.block_mut(JOURNAL_START + u64::from(relative));
        Jbd2JournalRevokeHeadS {
            r_header: JournalHeaderS {
                h_magic: JBD2_MAGIC,
                h_blocktype: JBD2_BLOCKTYPE_REVOKE,
                h_sequence: sequence,
            },
            r_count: 20,
        }
        .to_disk_bytes(&mut block[..16]);
        block[16..20].copy_from_slice(&(HOME_BLOCK as u32).to_be_bytes());
    }

    fn replay_fixture(device: ReplayDevice) -> (ReplayStatus, ReplayDevice) {
        replay_fixture_with_superblock(device, replay_superblock())
    }

    fn replay_fixture_with_superblock(
        device: ReplayDevice,
        superblock: JournalSuperBlock,
    ) -> (ReplayStatus, ReplayDevice) {
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        let journal_blocks = (JOURNAL_START..JOURNAL_START + u64::from(JOURNAL_LEN))
            .map(AbsoluteBN::new)
            .collect();
        journal
            .set_journal_superblock_with_mapping(superblock, journal_blocks)
            .expect("install replay journal");
        let status = journal.journal_replay_checked();
        (status, journal.into_inner())
    }

    fn write_csum_v3_transaction(
        device: &mut ReplayDevice,
        superblock: &JournalSuperBlock,
        descriptor_rel: u32,
        sequence: u32,
        home_block: u64,
        payload_byte: u8,
        commit_seconds: u64,
    ) {
        let payload_rel = descriptor_rel + 1;
        let commit_rel = descriptor_rel + 2;
        let payload_checksum = {
            let payload = device.block_mut(JOURNAL_START + u64::from(payload_rel));
            payload.fill(payload_byte);
            jbd2_tag_csum32(&superblock.s_uuid, sequence, payload)
        };

        let descriptor = device.block_mut(JOURNAL_START + u64::from(descriptor_rel));
        JournalHeaderS {
            h_magic: JBD2_MAGIC,
            h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
            h_sequence: sequence,
        }
        .to_disk_bytes(&mut descriptor[..JBD2_DESCRIPTOR_HEADER_SIZE]);
        JournalBlockTag3S {
            t_blocknr: home_block as u32,
            t_flags: u32::from(JBD2_FLAG_LAST_TAG),
            t_blocknr_high: (home_block >> 32) as u32,
            t_checksum: payload_checksum,
        }
        .to_disk_bytes(
            &mut descriptor
                [JBD2_DESCRIPTOR_HEADER_SIZE..JBD2_DESCRIPTOR_HEADER_SIZE + JBD2_TAG3_SIZE],
        );
        let uuid_start = JBD2_DESCRIPTOR_HEADER_SIZE + JBD2_TAG3_SIZE;
        descriptor[uuid_start..uuid_start + JBD2_UUID_SIZE].copy_from_slice(&superblock.s_uuid);
        let descriptor_checksum = jbd2_descriptor_block_csum32(&superblock.s_uuid, descriptor)
            .expect("descriptor checksum");
        descriptor[BLOCK_SIZE - 4..].copy_from_slice(&descriptor_checksum.to_be_bytes());

        write_csum_v3_commit(device, superblock, commit_rel, sequence, commit_seconds);
    }

    fn write_csum_v3_commit(
        device: &mut ReplayDevice,
        superblock: &JournalSuperBlock,
        relative: u32,
        sequence: u32,
        commit_seconds: u64,
    ) {
        let commit = device.block_mut(JOURNAL_START + u64::from(relative));
        CommitHeader {
            h_header: JournalHeaderS {
                h_magic: JBD2_MAGIC,
                h_blocktype: JBD2_BLOCKTYPE_COMMIT,
                h_sequence: sequence,
            },
            h_chksum_type: JBD2_CRC32C_CHKSUM,
            h_chksum_size: JBD2_CRC32_CHKSUM_SIZE,
            h_padding: [0; 2],
            h_chksum: [0; 8],
            h_commit_sec: commit_seconds,
            h_commit_nsec: 0,
        }
        .to_disk_bytes(commit);
        let commit_checksum =
            jbd2_commit_block_csum32(&superblock.s_uuid, commit).expect("commit checksum");
        commit[16..20].copy_from_slice(&commit_checksum.to_be_bytes());
    }

    fn write_csum_v3_revoke_transaction(
        device: &mut ReplayDevice,
        superblock: &JournalSuperBlock,
        revoke_rel: u32,
        sequence: u32,
        revoked_block: u64,
        commit_seconds: u64,
    ) {
        let revoke = device.block_mut(JOURNAL_START + u64::from(revoke_rel));
        Jbd2JournalRevokeHeadS {
            r_header: JournalHeaderS {
                h_magic: JBD2_MAGIC,
                h_blocktype: JBD2_BLOCKTYPE_REVOKE,
                h_sequence: sequence,
            },
            r_count: 24,
        }
        .to_disk_bytes(&mut revoke[..16]);
        revoke[16..24].copy_from_slice(&revoked_block.to_be_bytes());
        let revoke_checksum =
            jbd2_descriptor_block_csum32(&superblock.s_uuid, revoke).expect("revoke checksum");
        revoke[BLOCK_SIZE - 4..].copy_from_slice(&revoke_checksum.to_be_bytes());
        write_csum_v3_commit(device, superblock, revoke_rel + 1, sequence, commit_seconds);
    }

    fn transaction_system(phase: Jbd2RunningTransactionPhase) -> JBD2DEVSYSTEM {
        JBD2DEVSYSTEM {
            jbd2_super_block: replay_superblock(),
            start_block: AbsoluteBN::new(JOURNAL_START),
            max_len: JOURNAL_LEN,
            head: 1,
            sequence: 1,
            running_transaction: Jbd2RunningTransaction {
                phase,
                updates: vec![Jbd2Update(
                    AbsoluteBN::new(HOME_BLOCK),
                    vec![0x5a; BLOCK_SIZE].into_boxed_slice(),
                )],
                revoked_blocks: Vec::new(),
            },
            committing_transaction: None,
            checkpoint_transactions: Vec::new(),
            used_log_records: 0,
        }
    }

    #[test]
    fn commit_start_rejects_a_transaction_that_is_already_locked_or_switching() {
        for phase in [
            Jbd2RunningTransactionPhase::Locked,
            Jbd2RunningTransactionPhase::Switch,
        ] {
            let mut system = transaction_system(phase);

            let error = system
                .start_committing_transaction()
                .expect_err("only a running transaction may begin commit");

            assert_eq!(error.kind(), Ext4ErrorKind::Corrupted);
            assert_eq!(system.running_transaction.phase, phase);
            assert_eq!(system.running_transaction.updates.len(), 1);
            assert!(system.committing_transaction.is_none());
        }
    }

    #[test]
    fn commit_start_switches_the_old_owner_and_opens_a_fresh_running_transaction() {
        let mut system = transaction_system(Jbd2RunningTransactionPhase::Running);

        assert!(system.start_committing_transaction().expect("start commit"));

        assert_eq!(
            system.running_transaction.phase,
            Jbd2RunningTransactionPhase::Running
        );
        assert!(system.running_transaction.updates.is_empty());
        let committing = system
            .committing_transaction
            .as_ref()
            .expect("old transaction must have a committing owner");
        assert_eq!(committing.sequence, 1);
        assert_eq!(committing.updates.len(), 1);
    }

    #[test]
    fn later_revoke_suppresses_earlier_transaction_payload() {
        let mut device = ReplayDevice::new(256);
        device.block_mut(HOME_BLOCK).fill(0x11);
        write_descriptor(&mut device, 1, 1);
        device.block_mut(JOURNAL_START + 2).fill(0xa5);
        write_commit(&mut device, 3, 1);
        write_revoke(&mut device, 4, 2);
        write_commit(&mut device, 5, 2);

        let (status, device) = replay_fixture(device);
        assert_eq!(status, ReplayStatus::Complete);
        assert!(device.block(HOME_BLOCK).iter().all(|byte| *byte == 0x11));
    }

    #[test]
    fn earlier_revoke_does_not_suppress_later_transaction_payload() {
        let mut device = ReplayDevice::new(256);
        device.block_mut(HOME_BLOCK).fill(0x11);
        write_revoke(&mut device, 1, 1);
        write_commit(&mut device, 2, 1);
        write_descriptor(&mut device, 3, 2);
        device.block_mut(JOURNAL_START + 4).fill(0xa5);
        write_commit(&mut device, 5, 2);

        let (status, device) = replay_fixture(device);
        assert_eq!(status, ReplayStatus::Complete);
        assert!(device.block(HOME_BLOCK).iter().all(|byte| *byte == 0xa5));
    }

    #[test]
    fn stale_descriptor_checksum_ends_scan_after_older_committed_transaction() {
        let superblock = replay_csum_v3_superblock();
        let mut device = ReplayDevice::new(256);
        device.block_mut(HOME_BLOCK).fill(0x11);
        write_csum_v3_transaction(&mut device, &superblock, 1, 1, HOME_BLOCK, 0xa5, 10);
        write_csum_v3_transaction(&mut device, &superblock, 4, 2, HOME_BLOCK + 1, 0x6d, 9);
        device.block_mut(JOURNAL_START + 4)[BLOCK_SIZE - 1] ^= 1;

        let (status, device) = replay_fixture_with_superblock(device, superblock);

        assert_eq!(status, ReplayStatus::Complete);
        assert!(device.block(HOME_BLOCK).iter().all(|byte| *byte == 0xa5));
        assert!(device.block(HOME_BLOCK + 1).iter().all(|byte| *byte == 0));
    }

    #[test]
    fn stale_commit_checksum_ends_scan_after_older_committed_transaction() {
        let superblock = replay_csum_v3_superblock();
        let mut device = ReplayDevice::new(256);
        device.block_mut(HOME_BLOCK).fill(0x11);
        write_csum_v3_transaction(&mut device, &superblock, 1, 1, HOME_BLOCK, 0xa5, 10);
        write_csum_v3_transaction(&mut device, &superblock, 4, 2, HOME_BLOCK + 1, 0x6d, 9);
        device.block_mut(JOURNAL_START + 6)[16] ^= 1;

        let (status, device) = replay_fixture_with_superblock(device, superblock);

        assert_eq!(status, ReplayStatus::Complete);
        assert!(device.block(HOME_BLOCK).iter().all(|byte| *byte == 0xa5));
        assert!(device.block(HOME_BLOCK + 1).iter().all(|byte| *byte == 0));
    }

    #[test]
    fn stale_revoke_checksum_ends_scan_after_older_committed_transaction() {
        let superblock = replay_csum_v3_superblock();
        let mut device = ReplayDevice::new(256);
        device.block_mut(HOME_BLOCK).fill(0x11);
        write_csum_v3_transaction(&mut device, &superblock, 1, 1, HOME_BLOCK, 0xa5, 10);
        write_csum_v3_revoke_transaction(&mut device, &superblock, 4, 2, HOME_BLOCK + 1, 9);
        device.block_mut(JOURNAL_START + 4)[BLOCK_SIZE - 1] ^= 1;

        let (status, device) = replay_fixture_with_superblock(device, superblock);

        assert_eq!(status, ReplayStatus::Complete);
        assert!(device.block(HOME_BLOCK).iter().all(|byte| *byte == 0xa5));
        assert!(device.block(HOME_BLOCK + 1).iter().all(|byte| *byte == 0));
    }

    #[test]
    fn equal_or_increasing_commit_time_rejects_every_checksum_failure_kind() {
        #[derive(Clone, Copy, Debug)]
        enum Corruption {
            Descriptor,
            Commit,
            Revoke,
        }

        for commit_seconds in [10, 11] {
            for corruption in [
                Corruption::Descriptor,
                Corruption::Commit,
                Corruption::Revoke,
            ] {
                let superblock = replay_csum_v3_superblock();
                let mut device = ReplayDevice::new(256);
                write_csum_v3_transaction(&mut device, &superblock, 1, 1, HOME_BLOCK, 0xa5, 10);
                let expected_phase = match corruption {
                    Corruption::Descriptor => {
                        write_csum_v3_transaction(
                            &mut device,
                            &superblock,
                            4,
                            2,
                            HOME_BLOCK + 1,
                            0x6d,
                            commit_seconds,
                        );
                        device.block_mut(JOURNAL_START + 4)[BLOCK_SIZE - 1] ^= 1;
                        JournalReplayPhase::Scan
                    }
                    Corruption::Commit => {
                        write_csum_v3_transaction(
                            &mut device,
                            &superblock,
                            4,
                            2,
                            HOME_BLOCK + 1,
                            0x6d,
                            commit_seconds,
                        );
                        device.block_mut(JOURNAL_START + 6)[16] ^= 1;
                        JournalReplayPhase::Replay
                    }
                    Corruption::Revoke => {
                        write_csum_v3_revoke_transaction(
                            &mut device,
                            &superblock,
                            4,
                            2,
                            HOME_BLOCK + 1,
                            commit_seconds,
                        );
                        device.block_mut(JOURNAL_START + 4)[BLOCK_SIZE - 1] ^= 1;
                        JournalReplayPhase::Revoke
                    }
                };

                let (status, device) = replay_fixture_with_superblock(device, superblock);
                let failure = status.failure().unwrap_or_else(|| {
                    panic!("{corruption:?} at commit time {commit_seconds} must remain corruption")
                });
                assert_eq!(failure.phase(), expected_phase);
                assert_eq!(failure.cause().kind(), Ext4ErrorKind::ChecksumMismatch);
                assert!(device.block(HOME_BLOCK).iter().all(|byte| *byte == 0));
                assert!(device.block(HOME_BLOCK + 1).iter().all(|byte| *byte == 0));
            }
        }
    }

    #[test]
    fn transaction_id_order_wraps_like_linux_tid_gt() {
        assert!(JBD2DEVSYSTEM::transaction_id_after(0, u32::MAX));
        assert!(!JBD2DEVSYSTEM::transaction_id_after(u32::MAX, 0));
        assert!(!JBD2DEVSYSTEM::transaction_id_after(7, 7));
    }

    #[test]
    fn replay_bounds_each_contiguous_home_block_write() {
        const REPLAY_PAYLOADS: usize = MAX_BUFFERED_WRITE_BLOCKS + 1;
        const JOURNAL_PAYLOAD_START: u64 = 64;

        let system = transaction_system(Jbd2RunningTransactionPhase::Running);
        let mut io = SmallReplayIo {
            block_size: BLOCK_SIZE,
            blocks: vec![0; 128 * BLOCK_SIZE],
            writes: Vec::new(),
        };
        let mut payloads = Vec::new();
        for index in 0..REPLAY_PAYLOADS {
            let journal_rel = u32::try_from(index + 1).expect("test journal index fits u32");
            let journal_block = JOURNAL_PAYLOAD_START as usize + journal_rel as usize;
            io.blocks[journal_block * BLOCK_SIZE..(journal_block + 1) * BLOCK_SIZE]
                .fill(index as u8);
            payloads.push(ReplayPayload {
                tag: ReplayTag {
                    block: AbsoluteBN::new(HOME_BLOCK + index as u64),
                    flags: 0,
                    checksum: None,
                },
                journal_rel,
            });
        }
        let transaction = ReplayTransaction {
            start_rel: 1,
            sequence: 1,
            next_rel: u32::try_from(REPLAY_PAYLOADS + 2).expect("test journal length fits u32"),
            commit_time: 0,
            payloads,
            revoked_blocks: Vec::new(),
        };
        let ring = ReplayRing {
            blocks: &[],
            start_block: AbsoluteBN::new(JOURNAL_PAYLOAD_START),
            first_rel: 1,
            last_rel: 63,
        };

        system
            .replay_transaction(&mut io, &ring, &transaction, &BTreeMap::new())
            .expect("bounded replay");

        assert_eq!(
            io.writes,
            [
                (AbsoluteBN::new(HOME_BLOCK), 16),
                (AbsoluteBN::new(HOME_BLOCK + 16), 1),
            ]
        );
    }

    #[test]
    fn replay_rejects_block_too_small_for_commit_header_without_panicking() {
        const SMALL_BLOCK_SIZE: usize = 32;

        let system = transaction_system(Jbd2RunningTransactionPhase::Running);
        let mut io = SmallReplayIo {
            block_size: SMALL_BLOCK_SIZE,
            blocks: vec![0; 32 * SMALL_BLOCK_SIZE],
            writes: Vec::new(),
        };
        JournalHeaderS {
            h_magic: JBD2_MAGIC,
            h_blocktype: JBD2_BLOCKTYPE_COMMIT,
            h_sequence: 1,
        }
        .to_disk_bytes(
            &mut io.blocks[SMALL_BLOCK_SIZE..SMALL_BLOCK_SIZE + JBD2_DESCRIPTOR_HEADER_SIZE],
        );
        let ring = ReplayRing {
            blocks: &[],
            start_block: AbsoluteBN::new(0),
            first_rel: 1,
            last_rel: 15,
        };

        let result = system.scan_one_transaction(&mut io, &ring, 1, 1, 0);

        assert!(matches!(result, ReplayScan::Incomplete(_)));
    }

    #[test]
    fn replay_rejects_descriptor_without_a_last_tag() {
        let system = transaction_system(Jbd2RunningTransactionPhase::Running);
        let mut descriptor = vec![0; BLOCK_SIZE];
        JournalHeaderS {
            h_magic: JBD2_MAGIC,
            h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
            h_sequence: 1,
        }
        .to_disk_bytes(&mut descriptor[..JBD2_DESCRIPTOR_HEADER_SIZE]);
        JournalBlockTagS {
            t_blocknr: HOME_BLOCK as u32,
            t_checksum: 0,
            t_flags: JBD2_FLAG_SAME_UUID,
        }
        .to_disk_bytes(
            &mut descriptor
                [JBD2_DESCRIPTOR_HEADER_SIZE..JBD2_DESCRIPTOR_HEADER_SIZE + JBD2_TAG_SIZE],
        );

        let error = system
            .parse_replay_tags(&descriptor)
            .expect_err("descriptor tags must terminate with LAST_TAG");

        assert_eq!(error.kind(), Ext4ErrorKind::Corrupted);
    }

    #[test]
    fn replay_discards_an_empty_descriptor_even_before_an_adjacent_commit() {
        let system = transaction_system(Jbd2RunningTransactionPhase::Running);
        let mut io = SmallReplayIo {
            block_size: BLOCK_SIZE,
            blocks: vec![0; 256 * BLOCK_SIZE],
            writes: Vec::new(),
        };
        let descriptor_start = (JOURNAL_START as usize + 1) * BLOCK_SIZE;
        let descriptor = &mut io.blocks[descriptor_start..descriptor_start + BLOCK_SIZE];
        JournalHeaderS {
            h_magic: JBD2_MAGIC,
            h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
            h_sequence: 1,
        }
        .to_disk_bytes(&mut descriptor[..JBD2_DESCRIPTOR_HEADER_SIZE]);
        let commit_start = (JOURNAL_START as usize + 2) * BLOCK_SIZE;
        let commit = &mut io.blocks[commit_start..commit_start + BLOCK_SIZE];
        CommitHeader {
            h_header: JournalHeaderS {
                h_magic: JBD2_MAGIC,
                h_blocktype: JBD2_BLOCKTYPE_COMMIT,
                h_sequence: 1,
            },
            h_chksum_type: 0,
            h_chksum_size: 0,
            h_padding: [0; 2],
            h_chksum: [0; 8],
            h_commit_sec: 0,
            h_commit_nsec: 0,
        }
        .to_disk_bytes(commit);
        let ring = ReplayRing {
            blocks: &[],
            start_block: AbsoluteBN::new(JOURNAL_START),
            first_rel: 1,
            last_rel: JOURNAL_LEN - 1,
        };

        let result = system.scan_one_transaction(&mut io, &ring, 1, 1, 0);

        assert!(matches!(result, ReplayScan::CleanEnd));
    }

    #[test]
    fn regular_journal_payload_borrows_transaction_buffer_and_magic_is_escaped() {
        let regular = Jbd2Update(
            AbsoluteBN::new(HOME_BLOCK),
            vec![0x5a; BLOCK_SIZE].into_boxed_slice(),
        );
        let regular_pointer = regular.1.as_ptr();
        let regular_payload = journal_payload(&regular, BLOCK_SIZE).expect("regular payload");
        assert!(!regular_payload.is_escaped());
        assert_eq!(regular_payload.bytes().as_ptr(), regular_pointer);

        let mut magic = vec![0x6b; BLOCK_SIZE];
        magic[..4].copy_from_slice(&JBD2_MAGIC.to_be_bytes());
        let escaped = Jbd2Update(AbsoluteBN::new(HOME_BLOCK), magic.into_boxed_slice());
        let escaped_pointer = escaped.1.as_ptr();
        let escaped_payload = journal_payload(&escaped, BLOCK_SIZE).expect("escaped payload");
        assert!(escaped_payload.is_escaped());
        assert_ne!(escaped_payload.bytes().as_ptr(), escaped_pointer);
        assert_eq!(&escaped_payload.bytes()[..4], &[0; 4]);
        assert_eq!(&escaped_payload.bytes()[4..], &escaped.1[4..]);
    }
}
