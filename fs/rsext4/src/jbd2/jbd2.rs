//! JBD2 transaction commit and replay logic.

use alloc::{collections::BTreeSet, vec, vec::Vec};

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
pub(crate) enum ReplayStatus {
    Complete,
    Incomplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplayScan {
    CleanEnd,
    Incomplete { restart_rel: u32 },
    Applied { next_rel: u32, next_seq: u32 },
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
        self.jbd2_super_block.s_feature_incompat & feature != 0
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

    fn parse_replay_tags(&self, desc_buf: &[u8]) -> Option<Vec<ReplayTag>> {
        let has_csum_v3 = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_CSUM_V3);
        let has_64bit = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_64BIT);
        let block_size = desc_buf.len();
        let descriptor_end = if has_csum_v3 {
            let checksum_offset = block_size.checked_sub(4)?;
            let stored = u32::from_be_bytes(desc_buf[checksum_offset..].try_into().ok()?);
            let computed = jbd2_descriptor_block_csum32(&self.jbd2_super_block.s_uuid, desc_buf)?;
            if stored != computed {
                return None;
            }
            checksum_offset
        } else {
            block_size
        };
        let mut tags = Vec::new();
        let mut off = JBD2_DESCRIPTOR_HEADER_SIZE;

        while off < descriptor_end {
            let parsed = if has_csum_v3 {
                if off + JBD2_TAG3_SIZE > descriptor_end {
                    return None;
                }
                let tag = JournalBlockTag3S::from_disk_bytes(&desc_buf[off..off + JBD2_TAG3_SIZE]);
                let block = (u64::from(tag.t_blocknr_high) << 32) | u64::from(tag.t_blocknr);
                let all_zero = tag.t_blocknr == 0
                    && tag.t_flags == 0
                    && tag.t_blocknr_high == 0
                    && tag.t_checksum == 0;
                off += JBD2_TAG3_SIZE;
                (block, tag.t_flags, Some(tag.t_checksum), all_zero)
            } else {
                if off + JBD2_TAG_SIZE > descriptor_end {
                    return None;
                }
                let tag = JournalBlockTagS::from_disk_bytes(&desc_buf[off..off + JBD2_TAG_SIZE]);
                off += JBD2_TAG_SIZE;

                let mut block_high = 0u32;
                if has_64bit {
                    if off + JBD2_TAG_BLOCKNR_HIGH_SIZE > descriptor_end {
                        return None;
                    }
                    block_high = u32::from_be_bytes(
                        desc_buf[off..off + JBD2_TAG_BLOCKNR_HIGH_SIZE]
                            .try_into()
                            .ok()?,
                    );
                    off += JBD2_TAG_BLOCKNR_HIGH_SIZE;
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
                if off + JBD2_UUID_SIZE > descriptor_end {
                    return None;
                }
                off += JBD2_UUID_SIZE;
            }
            if last {
                break;
            }
        }

        Some(tags)
    }

    fn parse_revoke_blocks(&self, revoke_buf: &[u8]) -> Option<Vec<AbsoluteBN>> {
        if revoke_buf.len() < 16 {
            return None;
        }
        let record_end = if self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_CSUM_V3) {
            let checksum_offset = revoke_buf.len().checked_sub(4)?;
            let stored = u32::from_be_bytes(revoke_buf[checksum_offset..].try_into().ok()?);
            let computed = jbd2_descriptor_block_csum32(&self.jbd2_super_block.s_uuid, revoke_buf)?;
            if stored != computed {
                return None;
            }
            checksum_offset
        } else {
            revoke_buf.len()
        };
        let revoke = Jbd2JournalRevokeHeadS::from_disk_bytes(&revoke_buf[0..16]);
        let count = usize::try_from(revoke.r_count).ok()?;
        if !(16..=record_end).contains(&count) {
            return None;
        }

        let entry_size = if self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_64BIT) {
            8
        } else {
            4
        };
        let mut blocks = Vec::new();
        let mut off = 16usize;
        while off < count {
            if off + entry_size > count {
                return None;
            }

            let block = if entry_size == 8 {
                u64::from_be_bytes(revoke_buf[off..off + 8].try_into().ok()?)
            } else {
                u64::from(u32::from_be_bytes(
                    revoke_buf[off..off + 4].try_into().ok()?,
                ))
            };
            blocks.push(AbsoluteBN::new(block));
            off += entry_size;
        }

        Some(blocks)
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
        self.jbd2_super_block.to_disk_bytes(&mut sb_data[0..1024]);
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
            self.jbd2_super_block.s_start = self.jbd2_super_block.s_first;
            self.write_journal_superblock_with_mapping(block_dev, journal_blocks)?;
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
            Ok(target_use)
        }
    }
    /// Commits the currently queued metadata updates using the journal inode mapping.
    pub(crate) fn commit_transaction_with_mapping<D: FilesystemBlockIo>(
        &mut self,
        block_dev: &mut D,
        journal_blocks: &[AbsoluteBN],
    ) -> Ext4Result<bool> {
        let tid = self.sequence;

        if self.commit_queue.is_empty() {
            return Ok(false);
        }

        let block_size = block_dev.block_size();
        let mut desc_buffer = vec![0; block_size];

        // Build the descriptor block in memory first.
        let new_jbd_header = JournalHeaderS {
            h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
            h_sequence: tid,
            ..Default::default()
        };
        new_jbd_header.to_disk_bytes(&mut desc_buffer[0..JournalHeaderS::disk_size()]);

        let has_csum_v3 = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_CSUM_V3);
        let has_64bit = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_64BIT);
        let descriptor_end = if has_csum_v3 {
            block_size
                .checked_sub(4)
                .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:descriptor_size"))?
        } else {
            block_size
        };

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

        let mut current_offset = JBD2_DESCRIPTOR_HEADER_SIZE;
        let mut first_tag = true;
        // Emit one tag per metadata block queued for this transaction.
        for (idx, (target, journal_data, escaped)) in journal_payloads.iter().enumerate() {
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
            if idx == journal_payloads.len() - 1 {
                flags |= u32::from(JBD2_FLAG_LAST_TAG);
            }
            if !first_tag {
                flags |= u32::from(JBD2_FLAG_SAME_UUID);
            }

            let tag_size = if has_csum_v3 {
                JBD2_TAG3_SIZE
            } else {
                JBD2_TAG_SIZE + usize::from(has_64bit) * JBD2_TAG_BLOCKNR_HIGH_SIZE
            };
            let uuid_len = if first_tag { JBD2_UUID_SIZE } else { 0 };
            let tag_end = current_offset
                .checked_add(tag_size + uuid_len)
                .ok_or_else(Ext4Error::overflow)?;
            if tag_end > descriptor_end {
                return Err(Ext4Error::no_space().with_operation("jbd2:descriptor_full"));
            }

            if has_csum_v3 {
                JournalBlockTag3S {
                    t_blocknr: target_raw as u32,
                    t_flags: flags,
                    t_blocknr_high: block_high,
                    t_checksum: jbd2_tag_csum32(&self.jbd2_super_block.s_uuid, tid, journal_data),
                }
                .to_disk_bytes(&mut desc_buffer[current_offset..current_offset + JBD2_TAG3_SIZE]);
            } else {
                JournalBlockTagS {
                    t_blocknr: target_raw as u32,
                    t_checksum: 0,
                    t_flags: flags as u16,
                }
                .to_disk_bytes(&mut desc_buffer[current_offset..current_offset + JBD2_TAG_SIZE]);
                if has_64bit {
                    let high_offset = current_offset + JBD2_TAG_SIZE;
                    desc_buffer[high_offset..high_offset + JBD2_TAG_BLOCKNR_HIGH_SIZE]
                        .copy_from_slice(&block_high.to_be_bytes());
                }
            }
            current_offset += tag_size;

            if first_tag {
                desc_buffer[current_offset..current_offset + JBD2_UUID_SIZE]
                    .copy_from_slice(&self.jbd2_super_block.s_uuid);
                current_offset += JBD2_UUID_SIZE;
                first_tag = false;
            }
        }

        if has_csum_v3 {
            let checksum =
                jbd2_descriptor_block_csum32(&self.jbd2_super_block.s_uuid, &desc_buffer)
                    .ok_or_else(|| {
                        Ext4Error::corrupted().with_operation("jbd2:descriptor_checksum")
                    })?;
            desc_buffer[descriptor_end..].copy_from_slice(&checksum.to_be_bytes());
        }

        // Persist the descriptor first.
        let block_id = self.set_next_log_block_with_mapping(block_dev, journal_blocks)?;

        block_dev.write(&desc_buffer, block_id, 1)?;

        // Then write the journaled metadata payload blocks.
        for up in &journal_payloads {
            let metadata_journal_block_id =
                self.set_next_log_block_with_mapping(block_dev, journal_blocks)?;

            block_dev.write(&up.1, metadata_journal_block_id, 1)?;
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

        block_dev.write(&commit_buffer, commit_block_id, 1)?;
        block_dev.flush()?;
        self.sequence = self.sequence.wrapping_add(1);

        // Checkpoint: write metadata back to home blocks now that the commit
        // record is safely on disk. If the system crashes here the journal
        // replay will redo these writes, so partial checkpoints are safe.
        for update in self.commit_queue.iter() {
            block_dev.write(&update.1[..], update.0, 1)?;
        }
        block_dev.flush()?;

        self.commit_queue.clear();

        self.jbd2_super_block.s_sequence = self.sequence;
        self.jbd2_super_block.s_start = 0;
        self.head = 0;
        self.write_journal_superblock_with_mapping(block_dev, journal_blocks)?;
        block_dev.flush()?;

        Ok(true)
    }

    fn replay_one_transaction<D: FilesystemBlockIo>(
        &self,
        block_dev: &mut D,
        ring: &ReplayRing<'_>,
        start_rel: u32,
        expect_seq: u32,
    ) -> ReplayScan {
        let mut record_rel = start_rel;
        let mut payloads: Vec<ReplayPayload> = Vec::new();
        let mut revoked_blocks = BTreeSet::new();
        let max_records = ring.last_rel - ring.first_rel + 1;

        for _ in 0..max_records {
            let record_phys = match ring.phys(record_rel) {
                Ok(block) => block,
                Err(_) => {
                    return ReplayScan::Incomplete {
                        restart_rel: start_rel,
                    };
                }
            };
            let block_size = block_dev.block_size();
            if block_size < JBD2_DESCRIPTOR_HEADER_SIZE {
                return ReplayScan::Incomplete {
                    restart_rel: start_rel,
                };
            }
            let mut record_buf = vec![0u8; block_size];
            if block_dev.read(&mut record_buf, record_phys, 1).is_err() {
                return ReplayScan::Incomplete {
                    restart_rel: start_rel,
                };
            }

            let hdr = JournalHeaderS::from_disk_bytes(&record_buf[0..JBD2_DESCRIPTOR_HEADER_SIZE]);

            if hdr.h_magic != JBD2_MAGIC || hdr.h_sequence != expect_seq {
                return ReplayScan::CleanEnd;
            }

            match hdr.h_blocktype {
                JBD2_BLOCKTYPE_DESCRIPTOR => {
                    let tags = match self.parse_replay_tags(&record_buf) {
                        Some(tags) if !tags.is_empty() => tags,
                        Some(tags) => tags,
                        None => {
                            return ReplayScan::Incomplete {
                                restart_rel: start_rel,
                            };
                        }
                    };

                    for tag in tags {
                        ring.advance(&mut record_rel);
                        if ring.phys(record_rel).is_err() {
                            return ReplayScan::Incomplete {
                                restart_rel: start_rel,
                            };
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
                            return ReplayScan::Incomplete {
                                restart_rel: start_rel,
                            };
                        }
                    }

                    // Validate every payload in the committed transaction before
                    // issuing the first home-block write. A later corrupt payload
                    // must not leave a partially replayed transaction behind.
                    let mut committed: Vec<(ReplayPayload, Vec<u8>)> = Vec::new();
                    for payload in payloads.iter().copied() {
                        if revoked_blocks.contains(&payload.tag.block) {
                            continue;
                        }

                        let meta_phys = match ring.phys(payload.journal_rel) {
                            Ok(block) => block,
                            Err(_) => {
                                return ReplayScan::Incomplete {
                                    restart_rel: start_rel,
                                };
                            }
                        };
                        let mut data = vec![0u8; block_size];
                        if block_dev.read(&mut data, meta_phys, 1).is_err() {
                            return ReplayScan::Incomplete {
                                restart_rel: start_rel,
                            };
                        }
                        if let Some(stored) = payload.tag.checksum
                            && jbd2_tag_csum32(&self.jbd2_super_block.s_uuid, expect_seq, &data)
                                != stored
                        {
                            return ReplayScan::Incomplete {
                                restart_rel: start_rel,
                            };
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
                        let mut data = Vec::with_capacity(run_len * block_size);
                        for (payload, payload_data) in &committed[run_start..run_end] {
                            let offset = data.len();
                            data.extend_from_slice(payload_data);
                            if (payload.tag.flags & u32::from(JOURNAL_ESCAPE)) != 0 {
                                data[offset..offset + 4].copy_from_slice(&JBD2_MAGIC.to_be_bytes());
                            }
                        }
                        if block_dev.write(&data, first_home, run_len as u32).is_err() {
                            return ReplayScan::Incomplete {
                                restart_rel: start_rel,
                            };
                        }

                        pos = run_end;
                    }

                    let mut next_rel = record_rel;
                    ring.advance(&mut next_rel);
                    return ReplayScan::Applied {
                        next_rel,
                        next_seq: expect_seq.wrapping_add(1),
                    };
                }
                JBD2_BLOCKTYPE_REVOKE => {
                    let blocks = match self.parse_revoke_blocks(&record_buf) {
                        Some(blocks) => blocks,
                        None => {
                            return ReplayScan::Incomplete {
                                restart_rel: start_rel,
                            };
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

        ReplayScan::Incomplete {
            restart_rel: start_rel,
        }
    }

    /// Replays committed transactions using the journal inode logical-block map.
    pub(crate) fn replay_with_mapping<D: FilesystemBlockIo>(
        &mut self,
        block_dev: &mut D,
        journal_blocks: &[AbsoluteBN],
    ) -> ReplayStatus {
        let mut journal_rel = self.jbd2_super_block.s_start;
        if journal_rel == 0 {
            return ReplayStatus::Complete;
        }

        // If s_start points beyond the physical journal extent the on-disk
        // journal superblock is inconsistent (e.g. corrupted by a crash that
        // also mangled s_maxlen). There are no valid transactions to replay.
        if !journal_blocks.is_empty() && journal_rel as usize >= journal_blocks.len() {
            self.jbd2_super_block.s_start = 0;
            return ReplayStatus::Complete;
        }

        let maxlen = self.jbd2_super_block.s_maxlen;
        if maxlen == 0 {
            return ReplayStatus::Incomplete;
        }
        let Some(ring) = ReplayRing::new(self, journal_blocks) else {
            return ReplayStatus::Incomplete;
        };
        let mut expect_seq = self.jbd2_super_block.s_sequence;

        let status = loop {
            match self.replay_one_transaction(block_dev, &ring, journal_rel, expect_seq) {
                ReplayScan::Applied { next_rel, next_seq } => {
                    journal_rel = next_rel;
                    expect_seq = next_seq;
                    self.jbd2_super_block.s_start = journal_rel;
                    self.jbd2_super_block.s_sequence = expect_seq;
                    self.sequence = expect_seq;
                }
                ReplayScan::CleanEnd => {
                    self.jbd2_super_block.s_start = 0;
                    self.jbd2_super_block.s_sequence = expect_seq;
                    self.sequence = expect_seq;
                    break ReplayStatus::Complete;
                }
                ReplayScan::Incomplete { restart_rel } => {
                    self.jbd2_super_block.s_start = restart_rel;
                    self.jbd2_super_block.s_sequence = expect_seq;
                    self.sequence = expect_seq;
                    break ReplayStatus::Incomplete;
                }
            }
        };

        self.head = 0;

        // Write back the updated journal superblock without disturbing the rest
        // of the containing block.
        let sb_block = self
            .journal_phys_block(journal_blocks, 0)
            .unwrap_or(self.start_block);
        if sb_block.raw() != 0
            && self
                .write_journal_superblock_with_mapping(block_dev, journal_blocks)
                .and_then(|()| block_dev.flush())
                .is_err()
        {
            return ReplayStatus::Incomplete;
        }

        status
    }
}

/// Creates the journal inode and writes its initial journal superblock.
pub fn create_journal_entry<B: BlockIo + crate::runtime::Clock>(
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

    let mut jbd2_sb = JournalSuperBllockS::default();

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

    fs.datablock_cache
        .modify_new(block_dev, free_block[0], |data| {
            jbd2_sb.to_disk_bytes(data);
        })?;

    Ok(())
}
