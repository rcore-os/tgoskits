//! JBD2 transaction commit and replay logic.

use alloc::{collections::BTreeMap, vec, vec::Vec};

use crate::{
    blockdev::*,
    bmalloc::{AbsoluteBN, InodeNumber},
    checksum::{
        jbd2_commit_block_csum32, jbd2_descriptor_block_csum32, jbd2_tag_csum32,
        jbd2_update_superblock_checksum,
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

#[derive(Debug, Clone, Copy)]
struct ReplayTag {
    block: AbsoluteBN,
    flags: u32,
    checksum: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct ReplayPayload {
    tag: ReplayTag,
    journal_rel: u32,
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

    fn parse_replay_tags(&self, desc_buf: &[u8]) -> Ext4Result<Vec<ReplayTag>> {
        let has_csum_v3 = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_CSUM_V3);
        let has_64bit = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_64BIT);
        let block_size = desc_buf.len();
        let descriptor_end = if has_csum_v3 {
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
            if stored != computed {
                return Err(Ext4Error::checksum().with_operation("jbd2:replay_descriptor_checksum"));
            }
            checksum_offset
        } else {
            block_size
        };
        let mut tags = Vec::new();
        let mut off = JBD2_DESCRIPTOR_HEADER_SIZE;

        while off < descriptor_end {
            let parsed = if has_csum_v3 {
                let tag_end = off
                    .checked_add(JBD2_TAG3_SIZE)
                    .ok_or_else(Ext4Error::overflow)?;
                if tag_end > descriptor_end {
                    return Err(Ext4Error::corrupted().with_operation("jbd2:replay_tag3_truncated"));
                }
                let tag = JournalBlockTag3S::from_disk_bytes(&desc_buf[off..tag_end]);
                let block = (u64::from(tag.t_blocknr_high) << 32) | u64::from(tag.t_blocknr);
                let all_zero = tag.t_blocknr == 0
                    && tag.t_flags == 0
                    && tag.t_blocknr_high == 0
                    && tag.t_checksum == 0;
                off = tag_end;
                (block, tag.t_flags, Some(tag.t_checksum), all_zero)
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

                let block = (u64::from(block_high) << 32) | u64::from(tag.t_blocknr);
                let all_zero = tag.t_blocknr == 0
                    && tag.t_checksum == 0
                    && tag.t_flags == 0
                    && block_high == 0;
                (block, u32::from(tag.t_flags), None, all_zero)
            };

            let (block, flags, checksum, all_zero) = parsed;
            if all_zero && desc_buf[off..descriptor_end].iter().all(|b| *b == 0) {
                break;
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
                break;
            }
        }

        Ok(tags)
    }

    fn parse_revoke_blocks(&self, revoke_buf: &[u8]) -> Ext4Result<Vec<AbsoluteBN>> {
        if revoke_buf.len() < 16 {
            return Err(Ext4Error::corrupted().with_operation("jbd2:replay_revoke_header"));
        }
        let record_end = if self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_CSUM_V3) {
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
            if stored != computed {
                return Err(Ext4Error::checksum().with_operation("jbd2:replay_revoke_checksum"));
            }
            checksum_offset
        } else {
            revoke_buf.len()
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

        Ok(blocks)
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

        // The first commit initializes `s_start` in the journal superblock.
        if self.jbd2_super_block.s_start == 0 {
            let previous_superblock = self.jbd2_super_block;
            self.jbd2_super_block.s_start = self.jbd2_super_block.s_first;
            if let Err(error) =
                self.write_journal_superblock_with_mapping(block_dev, journal_blocks)
            {
                self.jbd2_super_block = previous_superblock;
                return Err(error);
            }
            self.head += 1;
            let mut rel = self
                .jbd2_super_block
                .s_start
                .checked_add(self.head)
                .and_then(|v| v.checked_sub(1))
                .ok_or_else(Ext4Error::invalid_input)?;
            // Wrap when the cursor runs past the end of the journal ring.
            if rel > last_rel {
                self.head = 0;
                rel = self.jbd2_super_block.s_start;
            }
            let target_use = self.journal_phys_block(journal_blocks, rel)?;
            self.used_log_records = self
                .used_log_records
                .checked_add(1)
                .ok_or_else(Ext4Error::overflow)?;
            Ok(target_use)
        } else {
            self.head += 1;
            let mut rel = self
                .jbd2_super_block
                .s_start
                .checked_add(self.head)
                .and_then(|v| v.checked_sub(1))
                .ok_or_else(Ext4Error::invalid_input)?;
            if rel > last_rel {
                self.head = 0;
                rel = self.jbd2_super_block.s_start;
            }
            let target_use = self.journal_phys_block(journal_blocks, rel)?;
            self.used_log_records = self
                .used_log_records
                .checked_add(1)
                .ok_or_else(Ext4Error::overflow)?;
            Ok(target_use)
        }
    }

    fn write_revoke_records<D: FilesystemBlockIo>(
        &mut self,
        block_dev: &mut D,
        journal_blocks: &[AbsoluteBN],
        tid: u32,
        block_size: usize,
    ) -> Ext4Result<()> {
        let has_csum_v3 = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_CSUM_V3);
        let has_64bit = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_64BIT);
        let entry_size = if has_64bit { 8 } else { 4 };
        let record_end = if has_csum_v3 {
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
        if !self.revoke_queue.is_empty() && capacity == 0 {
            return Err(Ext4Error::no_space().with_operation("jbd2:revoke_capacity"));
        }

        let revoke_entries = self.revoke_queue.clone();
        for revoked in revoke_entries.chunks(capacity.max(1)) {
            let mut revoke_buffer = vec![0_u8; block_size];
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
            if has_csum_v3 {
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

    /// Commits the currently queued metadata updates using the journal inode mapping.
    pub(crate) fn commit_transaction_with_mapping<D: FilesystemBlockIo>(
        &mut self,
        block_dev: &mut D,
        journal_blocks: &[AbsoluteBN],
    ) -> Ext4Result<bool> {
        let tid = self.sequence;

        if self.commit_queue.is_empty() && self.revoke_queue.is_empty() {
            return Ok(false);
        }

        let block_size = block_dev.block_size();
        let has_csum_v3 = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_CSUM_V3);
        let has_64bit = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_64BIT);
        let descriptor_end = if has_csum_v3 {
            block_size
                .checked_sub(4)
                .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:descriptor_size"))?
        } else {
            block_size
        };
        let descriptor_capacity = self.jbd2_super_block.descriptor_tag_capacity(block_size)?;
        let revoke_entry_size = if has_64bit { 8 } else { 4 };
        let revoke_end = if has_csum_v3 {
            block_size
                .checked_sub(core::mem::size_of::<u32>())
                .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:revoke_size"))?
        } else {
            block_size
        };
        let revoke_capacity = revoke_end
            .checked_sub(core::mem::size_of::<Jbd2JournalRevokeHeadS>())
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:revoke_size"))?
            / revoke_entry_size;
        if !self.revoke_queue.is_empty() && revoke_capacity == 0 {
            return Err(Ext4Error::no_space().with_operation("jbd2:revoke_capacity"));
        }
        let descriptor_records = if self.commit_queue.is_empty() {
            0
        } else {
            self.commit_queue.len().div_ceil(descriptor_capacity)
        };
        let revoke_records = if self.revoke_queue.is_empty() {
            0
        } else {
            self.revoke_queue.len().div_ceil(revoke_capacity)
        };
        let required_log_records = self
            .commit_queue
            .len()
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
        self.write_revoke_records(block_dev, journal_blocks, tid, block_size)?;

        let mut journal_payloads: Vec<(AbsoluteBN, Vec<u8>, bool)> = Vec::new();
        for update in &self.commit_queue {
            if update.1.len() != block_size || update.1.len() < 4 {
                return Err(Ext4Error::corrupted().with_operation("jbd2:update_block_size"));
            }
            let mut journal_data = update.1.to_vec();
            let escaped = journal_data.starts_with(&JBD2_MAGIC.to_be_bytes());
            if escaped {
                journal_data[0..4].fill(0);
            }
            journal_payloads.push((update.0, journal_data, escaped));
        }

        let mut payload_offset = 0usize;
        while payload_offset < journal_payloads.len() {
            let payload_end = payload_offset
                .checked_add(descriptor_capacity)
                .ok_or_else(Ext4Error::overflow)?
                .min(journal_payloads.len());
            let payload_chunk = &journal_payloads[payload_offset..payload_end];
            let mut desc_buffer = vec![0; block_size];
            JournalHeaderS {
                h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
                h_sequence: tid,
                ..Default::default()
            }
            .to_disk_bytes(&mut desc_buffer[0..JournalHeaderS::disk_size()]);

            let mut current_offset = JBD2_DESCRIPTOR_HEADER_SIZE;
            for (index, (target, journal_data, escaped)) in payload_chunk.iter().enumerate() {
                let target_raw = target.raw();
                let block_high = (target_raw >> 32) as u32;
                if !has_64bit && block_high != 0 {
                    return Err(Ext4Error::unsupported().with_operation("jbd2:64bit_block_number"));
                }
                let mut flags = if *escaped {
                    u32::from(JOURNAL_ESCAPE)
                } else {
                    0
                };
                if index + 1 == payload_chunk.len() {
                    flags |= u32::from(JBD2_FLAG_LAST_TAG);
                }
                if index != 0 {
                    flags |= u32::from(JBD2_FLAG_SAME_UUID);
                }

                if has_csum_v3 {
                    JournalBlockTag3S {
                        t_blocknr: target_raw as u32,
                        t_flags: flags,
                        t_blocknr_high: block_high,
                        t_checksum: jbd2_tag_csum32(
                            &self.jbd2_super_block.s_uuid,
                            tid,
                            journal_data,
                        ),
                    }
                    .to_disk_bytes(
                        &mut desc_buffer[current_offset..current_offset + JBD2_TAG3_SIZE],
                    );
                    current_offset += JBD2_TAG3_SIZE;
                } else {
                    JournalBlockTagS {
                        t_blocknr: target_raw as u32,
                        t_checksum: 0,
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
                }

                if index == 0 {
                    desc_buffer[current_offset..current_offset + JBD2_UUID_SIZE]
                        .copy_from_slice(&self.jbd2_super_block.s_uuid);
                    current_offset += JBD2_UUID_SIZE;
                }
            }

            if current_offset > descriptor_end {
                return Err(Ext4Error::no_space().with_operation("jbd2:descriptor_full"));
            }
            if has_csum_v3 {
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
            block_dev.write(&desc_buffer, descriptor_block, 1)?;
            for payload in payload_chunk {
                let payload_block =
                    self.set_next_log_block_with_mapping(block_dev, journal_blocks)?;
                block_dev.write(&payload.1, payload_block, 1)?;
            }
            payload_offset = payload_end;
        }

        block_dev.flush()?;

        // Write the commit block BEFORE checkpointing so that a crash during
        // checkpoint still leaves a valid committed transaction in the journal
        // for replay on the next mount.
        let mut commit_buffer = vec![0_u8; block_size];

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
            h_commit_sec: 0,
            h_commit_nsec: 0,
        };

        commit_block.to_disk_bytes(&mut commit_buffer);
        if has_csum_v3 {
            let checksum = jbd2_commit_block_csum32(&self.jbd2_super_block.s_uuid, &commit_buffer)
                .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:commit_checksum"))?;
            commit_block.h_chksum[0] = checksum;
            commit_block.to_disk_bytes(&mut commit_buffer);
        }
        let commit_block_id = self.set_next_log_block_with_mapping(block_dev, journal_blocks)?;

        block_dev.write_with_flags(
            &commit_buffer,
            commit_block_id,
            1,
            WriteFlags::METADATA | WriteFlags::FUA,
        )?;
        block_dev.flush()?;
        self.sequence = self.sequence.wrapping_add(1);

        self.checkpoint_transactions
            .push(Jbd2CheckpointTransaction {
                sequence: tid,
                updates: core::mem::take(&mut self.commit_queue),
                revoked_blocks: core::mem::take(&mut self.revoke_queue),
            });

        Ok(true)
    }

    /// Writes every committed transaction to its home locations and advances
    /// the on-disk journal tail only after those writes are durable.
    pub(crate) fn checkpoint_transactions_with_mapping<D: FilesystemBlockIo>(
        &mut self,
        block_dev: &mut D,
        journal_blocks: &[AbsoluteBN],
    ) -> Ext4Result<bool> {
        if self.checkpoint_transactions.is_empty() {
            return Ok(false);
        }

        for transaction in &self.checkpoint_transactions {
            for update in &transaction.updates {
                let superseded = self.checkpoint_transactions.iter().any(|later| {
                    Self::transaction_id_after(later.sequence, transaction.sequence)
                        && later.revoked_blocks.contains(&update.0)
                });
                if !superseded {
                    block_dev.write(&update.1[..], update.0, 1)?;
                }
            }
            block_dev.flush()?;
        }

        self.checkpoint_transactions.clear();
        self.jbd2_super_block.s_sequence = self.sequence;
        self.jbd2_super_block.s_start = 0;
        self.head = 0;
        self.used_log_records = 0;
        self.write_journal_superblock_with_mapping_flags(
            block_dev,
            journal_blocks,
            WriteFlags::METADATA | WriteFlags::FUA,
        )?;
        block_dev.flush()?;

        Ok(true)
    }

    fn scan_one_transaction<D: FilesystemBlockIo>(
        &self,
        block_dev: &mut D,
        ring: &ReplayRing<'_>,
        start_rel: u32,
        expect_seq: u32,
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
        if block_size < JBD2_DESCRIPTOR_HEADER_SIZE {
            return ReplayScan::Incomplete(ReplayFailure::at(
                JournalReplayPhase::Initialize,
                Ext4Error::corrupted().with_operation("jbd2:replay_block_size"),
                start_rel,
            ));
        }

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
                    let tags = match self.parse_replay_tags(&record_buf) {
                        Ok(tags) => tags,
                        Err(error) => {
                            return ReplayScan::Incomplete(ReplayFailure::at(
                                JournalReplayPhase::Scan,
                                error,
                                start_rel,
                            ));
                        }
                    };

                    for tag in tags {
                        ring.advance(&mut record_rel);
                        if let Err(error) = ring.phys(record_rel) {
                            return ReplayScan::Incomplete(ReplayFailure::at(
                                JournalReplayPhase::Scan,
                                error,
                                start_rel,
                            ));
                        }
                        payloads.push(ReplayPayload {
                            tag,
                            journal_rel: record_rel,
                        });
                    }
                }
                JBD2_BLOCKTYPE_COMMIT => {
                    if self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_CSUM_V3) {
                        let commit = CommitHeader::from_disk_bytes(&record_buf);
                        let computed =
                            jbd2_commit_block_csum32(&self.jbd2_super_block.s_uuid, &record_buf);
                        if computed != Some(commit.h_chksum[0]) {
                            return ReplayScan::Incomplete(ReplayFailure::at(
                                JournalReplayPhase::Replay,
                                Ext4Error::checksum().with_operation("jbd2:replay_commit_checksum"),
                                start_rel,
                            ));
                        }
                    }

                    let mut next_rel = record_rel;
                    ring.advance(&mut next_rel);
                    return ReplayScan::Committed(ReplayTransaction {
                        start_rel,
                        sequence: expect_seq,
                        next_rel,
                        payloads,
                        revoked_blocks,
                    });
                }
                JBD2_BLOCKTYPE_REVOKE => {
                    let blocks = match self.parse_revoke_blocks(&record_buf) {
                        Ok(blocks) => blocks,
                        Err(error) => {
                            return ReplayScan::Incomplete(ReplayFailure::at(
                                JournalReplayPhase::Revoke,
                                error,
                                start_rel,
                            ));
                        }
                    };
                    revoked_blocks.extend(blocks);
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

    fn replay_transaction<D: FilesystemBlockIo>(
        &self,
        block_dev: &mut D,
        ring: &ReplayRing<'_>,
        transaction: &ReplayTransaction,
        revoke_table: &BTreeMap<AbsoluteBN, u32>,
    ) -> Result<(), ReplayFailure> {
        let block_size = block_dev.block_size();
        // Validate every non-revoked payload in this transaction before the
        // first home-block write. Corruption must not partially replay one
        // transaction.
        let mut committed: Vec<(ReplayPayload, Vec<u8>)> = Vec::new();
        for payload in transaction.payloads.iter().copied() {
            if Self::payload_is_revoked(revoke_table, payload.tag.block, transaction.sequence) {
                continue;
            }

            let meta_phys = ring.phys(payload.journal_rel).map_err(|error| {
                ReplayFailure::at(JournalReplayPhase::Replay, error, transaction.start_rel)
            })?;
            let mut data = vec![0u8; block_size];
            block_dev.read(&mut data, meta_phys, 1).map_err(|error| {
                ReplayFailure::at(JournalReplayPhase::Replay, error, transaction.start_rel)
            })?;
            if let Some(stored) = payload.tag.checksum
                && jbd2_tag_csum32(&self.jbd2_super_block.s_uuid, transaction.sequence, &data)
                    != stored
            {
                return Err(ReplayFailure::at(
                    JournalReplayPhase::Replay,
                    Ext4Error::checksum().with_operation("jbd2:replay_payload_checksum"),
                    transaction.start_rel,
                ));
            }
            committed.push((payload, data));
        }

        let mut pos = 0usize;
        while pos < committed.len() {
            let run_start = pos;
            let mut run_end = pos + 1;
            while run_end < committed.len()
                && committed[run_end].0.tag.block.raw()
                    == committed[run_end - 1].0.tag.block.raw().saturating_add(1)
            {
                run_end += 1;
            }

            let run_len = run_end - run_start;
            let first_home = committed[run_start].0.tag.block;
            let run_bytes = run_len.checked_mul(block_size).ok_or_else(|| {
                ReplayFailure::at(
                    JournalReplayPhase::Replay,
                    Ext4Error::overflow(),
                    transaction.start_rel,
                )
            })?;
            let mut data = Vec::with_capacity(run_bytes);
            for (payload, payload_data) in &committed[run_start..run_end] {
                let offset = data.len();
                data.extend_from_slice(payload_data);
                if (payload.tag.flags & u32::from(JOURNAL_ESCAPE)) != 0 {
                    data[offset..offset + 4].copy_from_slice(&JBD2_MAGIC.to_be_bytes());
                }
            }
            let run_count = u32::try_from(run_len).map_err(|_| {
                ReplayFailure::at(
                    JournalReplayPhase::Replay,
                    Ext4Error::overflow(),
                    transaction.start_rel,
                )
            })?;
            block_dev
                .write(&data, first_home, run_count)
                .map_err(|error| {
                    ReplayFailure::at(JournalReplayPhase::Replay, error, transaction.start_rel)
                })?;

            pos = run_end;
        }
        Ok(())
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

        // Pass 1: discover and validate the complete committed transaction
        // range. No home block is written in this pass.
        let scan_failure = loop {
            match self.scan_one_transaction(block_dev, &ring, journal_rel, expect_seq) {
                ReplayScan::Committed(transaction) => {
                    journal_rel = transaction.next_rel;
                    expect_seq = transaction.sequence.wrapping_add(1);
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

        self.head = 0;

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

    fn replay_superblock() -> JournalSuperBlock {
        JournalSuperBlock {
            s_maxlen: JOURNAL_LEN,
            s_first: 1,
            s_start: 1,
            s_sequence: 1,
            ..JournalSuperBlock::default()
        }
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
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        let journal_blocks = (JOURNAL_START..JOURNAL_START + u64::from(JOURNAL_LEN))
            .map(AbsoluteBN::new)
            .collect();
        journal
            .set_journal_superblock_with_mapping(replay_superblock(), journal_blocks)
            .expect("install replay journal");
        let status = journal.journal_replay_checked();
        (status, journal.into_inner())
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
    fn transaction_id_order_wraps_like_linux_tid_gt() {
        assert!(JBD2DEVSYSTEM::transaction_id_after(0, u32::MAX));
        assert!(!JBD2DEVSYSTEM::transaction_id_after(u32::MAX, 0));
        assert!(!JBD2DEVSYSTEM::transaction_id_after(7, 7));
    }
}
