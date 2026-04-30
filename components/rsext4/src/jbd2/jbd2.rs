//! JBD2 transaction commit and replay logic.

use alloc::{collections::BTreeSet, vec, vec::Vec};

use log::{debug, info, warn};

use crate::{
    blockdev::*,
    bmalloc::{AbsoluteBN, InodeNumber},
    checksum::jbd2_update_superblock_checksum,
    config::*,
    crc32c::crc32c::ext4_superblock_has_metadata_csum,
    disknode::*,
    endian::*,
    error::*,
    ext4::*,
    file::*,
    jbd2::jbdstruct::*,
    loopfile::*,
    metadata::Ext4InodeMetadataUpdate,
};

#[derive(Debug, Clone, Copy)]
struct ReplayTag {
    block: AbsoluteBN,
    flags: u32,
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
            Some(len) => self.jbd2_super_block.s_maxlen.min(len),
        };

        let last = total_blocks.checked_sub(1)?;
        if last < self.jbd2_super_block.s_first {
            None
        } else {
            Some(last)
        }
    }

    fn parse_replay_tags(&self, desc_buf: &[u8; BLOCK_SIZE], tid: u32) -> Option<Vec<ReplayTag>> {
        let has_csum_v3 = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_CSUM_V3);
        let has_64bit = self.has_incompat_feature(JBD2_FEATURE_INCOMPAT_64BIT);
        let mut tags = Vec::new();
        let mut off = JBD2_DESCRIPTOR_HEADER_SIZE;
        let mut tag_idx = 0usize;

        while off < BLOCK_SIZE {
            let parsed = if has_csum_v3 {
                if off + JBD2_TAG3_SIZE > BLOCK_SIZE {
                    debug!("[JBD2 replay] descriptor tag3 truncated: tid={tid} tag_idx={tag_idx}");
                    return None;
                }
                let tag = JournalBlockTag3S::from_disk_bytes(&desc_buf[off..off + JBD2_TAG3_SIZE]);
                let block = (u64::from(tag.t_blocknr_high) << 32) | u64::from(tag.t_blocknr);
                let all_zero = tag.t_blocknr == 0
                    && tag.t_flags == 0
                    && tag.t_blocknr_high == 0
                    && tag.t_checksum == 0;
                off += JBD2_TAG3_SIZE;
                (block, tag.t_flags, all_zero)
            } else {
                if off + JBD2_TAG_SIZE > BLOCK_SIZE {
                    debug!("[JBD2 replay] descriptor tag truncated: tid={tid} tag_idx={tag_idx}");
                    return None;
                }
                let tag = JournalBlockTagS::from_disk_bytes(&desc_buf[off..off + JBD2_TAG_SIZE]);
                off += JBD2_TAG_SIZE;

                let mut block_high = 0u32;
                if has_64bit {
                    if off + JBD2_TAG_BLOCKNR_HIGH_SIZE > BLOCK_SIZE {
                        debug!(
                            "[JBD2 replay] descriptor tag high block truncated: tid={tid} \
                             tag_idx={tag_idx}"
                        );
                        return None;
                    }
                    block_high = u32::from_be_bytes(
                        desc_buf[off..off + JBD2_TAG_BLOCKNR_HIGH_SIZE]
                            .try_into()
                            .unwrap(),
                    );
                    off += JBD2_TAG_BLOCKNR_HIGH_SIZE;
                }

                let block = (u64::from(block_high) << 32) | u64::from(tag.t_blocknr);
                let all_zero = tag.t_blocknr == 0
                    && tag.t_checksum == 0
                    && tag.t_flags == 0
                    && block_high == 0;
                (block, u32::from(tag.t_flags), all_zero)
            };

            let (block, flags, all_zero) = parsed;
            if all_zero && desc_buf[off..].iter().all(|b| *b == 0) {
                break;
            }

            debug!(
                "[JBD2 replay] tid={} tag_idx={} block={} flags=0x{:x}",
                tid, tag_idx, block, flags
            );

            let last = (flags & u32::from(JBD2_FLAG_LAST_TAG)) != 0;
            let same_uuid = (flags & u32::from(JBD2_FLAG_SAME_UUID)) != 0;
            tags.push(ReplayTag {
                block: AbsoluteBN::new(block),
                flags,
            });

            if !same_uuid {
                if off + JBD2_UUID_SIZE > BLOCK_SIZE {
                    debug!(
                        "[JBD2 replay] descriptor uuid truncated: tid={} tag_idx={}",
                        tid, tag_idx
                    );
                    return None;
                }
                off += JBD2_UUID_SIZE;
            }
            tag_idx += 1;

            if last {
                break;
            }
        }

        Some(tags)
    }

    fn parse_revoke_blocks(
        &self,
        revoke_buf: &[u8; BLOCK_SIZE],
        tid: u32,
    ) -> Option<Vec<AbsoluteBN>> {
        let revoke = Jbd2JournalRevokeHeadS::from_disk_bytes(&revoke_buf[0..16]);
        let count = usize::try_from(revoke.r_count).ok()?;
        if !(16..=BLOCK_SIZE).contains(&count) {
            debug!("[JBD2 replay] revoke block has invalid count: tid={tid} count={count}");
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
                debug!("[JBD2 replay] revoke entry truncated: tid={tid} off={off} count={count}");
                return None;
            }

            let block = if entry_size == 8 {
                u64::from_be_bytes(revoke_buf[off..off + 8].try_into().unwrap())
            } else {
                u64::from(u32::from_be_bytes(
                    revoke_buf[off..off + 4].try_into().unwrap(),
                ))
            };
            blocks.push(AbsoluteBN::new(block));
            off += entry_size;
        }

        Some(blocks)
    }

    fn write_journal_superblock_with_mapping<B: BlockDevice>(
        &mut self,
        block_dev: &mut B,
        journal_blocks: &[AbsoluteBN],
    ) {
        let sb_block = self
            .journal_phys_block(journal_blocks, 0)
            .expect("journal superblock block is invalid");
        let mut sb_data = [0u8; BLOCK_SIZE];
        block_dev
            .read(&mut sb_data, sb_block, 1)
            .expect("Read journal superblock failed");
        jbd2_update_superblock_checksum(&mut self.jbd2_super_block);
        self.jbd2_super_block.to_disk_bytes(&mut sb_data[0..1024]);
        block_dev
            .write(&sb_data, sb_block, 1)
            .expect("Write journal superblock failed");
    }

    /// Returns the next writable journal block, handling wrap-around.
    pub fn set_next_log_block<B: BlockDevice>(
        &mut self,
        block_dev: &mut B,
    ) -> Ext4Result<AbsoluteBN> {
        self.set_next_log_block_with_mapping(block_dev, &[])
    }

    /// Returns the next writable journal block using the journal inode mapping.
    pub(crate) fn set_next_log_block_with_mapping<B: BlockDevice>(
        &mut self,
        block_dev: &mut B,
        journal_blocks: &[AbsoluteBN],
    ) -> Ext4Result<AbsoluteBN> {
        let last_rel = self
            .last_logical_block(journal_blocks)
            .ok_or_else(Ext4Error::corrupted)?;

        // The first commit initializes `s_start` in the journal superblock.
        if self.jbd2_super_block.s_start == 0 {
            self.jbd2_super_block.s_start = self.jbd2_super_block.s_first;
            self.write_journal_superblock_with_mapping(block_dev, journal_blocks);
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
    /// Commits the currently queued metadata updates as one transaction.
    pub fn commit_transaction<B: BlockDevice>(&mut self, block_dev: &mut B) -> Ext4Result<bool> {
        self.commit_transaction_with_mapping(block_dev, &[])
    }

    /// Commits the currently queued metadata updates using the journal inode mapping.
    pub(crate) fn commit_transaction_with_mapping<B: BlockDevice>(
        &mut self,
        block_dev: &mut B,
        journal_blocks: &[AbsoluteBN],
    ) -> Ext4Result<bool> {
        let tid = self.sequence;
        debug!(
            "[JBD2 commit] begin: tid={} updates_len={} head={} start_block={} max_len={} \
             seq_in_superblock={} s_start={}",
            tid,
            self.commit_queue.len(),
            self.head,
            self.start_block,
            self.max_len,
            self.jbd2_super_block.s_sequence,
            self.jbd2_super_block.s_start,
        );

        if self.commit_queue.is_empty() {
            warn!("no metadata updates queued for journal commit");
            return Ok(false);
        }

        let mut desc_buffer = vec![0; BLOCK_SIZE];

        // Build the descriptor block in memory first.
        let new_jbd_header = JournalHeaderS {
            h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
            h_sequence: tid,
            ..Default::default()
        };
        new_jbd_header.to_disk_bytes(&mut desc_buffer[0..JournalHeaderS::disk_size()]);

        let mut current_offset = JBD2_DESCRIPTOR_HEADER_SIZE;
        let mut first_tag = true;
        // Emit one tag per metadata block queued for this transaction.
        for (idx, update) in self.commit_queue.iter().enumerate() {
            // Metadata blocks that begin with the journal magic must be escaped
            // so replay never mistakes them for journal headers.
            let mut tag = JournalBlockTagS {
                t_blocknr: update.0.to_u32()?,
                t_checksum: 0,
                t_flags: 0,
            };
            let magic: u32 = u32::from_le_bytes(update.1[0..4].try_into().unwrap());
            if magic == JBD2_MAGIC {
                tag.t_flags |= JOURNAL_ESCAPE;
                debug!("[JBD2 commit] escaping metadata block that begins with journal magic");
            }

            if idx == self.commit_queue.len() - 1 {
                tag.t_flags |= JBD2_FLAG_LAST_TAG;
            }

            if !first_tag {
                tag.t_flags |= JBD2_FLAG_SAME_UUID;
            }

            debug!(
                "[JBD2 commit] tid={} tag_idx={} t_blocknr={} t_flags=0x{:x}",
                tid, idx, tag.t_blocknr, tag.t_flags,
            );
            tag.to_disk_bytes(&mut desc_buffer[current_offset..current_offset + JBD2_TAG_SIZE]);
            current_offset += JBD2_TAG_SIZE;

            if first_tag {
                desc_buffer[current_offset..current_offset + JBD2_UUID_SIZE]
                    .copy_from_slice(&self.jbd2_super_block.s_uuid);
                current_offset += JBD2_UUID_SIZE;
                first_tag = false;
            }
        }

        // Persist the descriptor first.
        let block_id = self.set_next_log_block_with_mapping(block_dev, journal_blocks)?;
        debug!("[JBD2 commit] tid={tid} descriptor_block_id={block_id} (absolute)");
        block_dev.write(&desc_buffer, block_id, 1)?;

        let mut no_escape: Vec<(AbsoluteBN, [u8; BLOCK_SIZE])> = Vec::new();
        for update in self.commit_queue.iter() {
            let mut check_data: [u8; BLOCK_SIZE] = [0; BLOCK_SIZE];
            check_data.copy_from_slice(&*update.1);
            let magic = u32::from_le_bytes(check_data[0..4].try_into().unwrap());
            if magic == JBD2_MAGIC {
                debug!("[JBD2 commit] zero escaped journal magic in payload copy");
                check_data[0..4].fill(0);
            }
            no_escape.push((update.0, check_data));
        }

        // Then write the journaled metadata payload blocks.
        for (idx, up) in no_escape.iter().enumerate() {
            let metadata_journal_block_id =
                self.set_next_log_block_with_mapping(block_dev, journal_blocks)?;
            debug!(
                "[JBD2 commit] tid={} meta_idx={} journal_block_id={} (absolute) \
                 target_phys_block={}",
                tid, idx, metadata_journal_block_id, up.0
            );
            block_dev.write(&up.1, metadata_journal_block_id, 1)?;
        }

        block_dev.flush()?;

        // Write the commit block BEFORE checkpointing so that a crash during
        // checkpoint still leaves a valid committed transaction in the journal
        // for replay on the next mount.
        let mut commit_buffer = [0_u8; BLOCK_SIZE];

        let commit_block = CommitHeader {
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
        let commit_block_id = self.set_next_log_block_with_mapping(block_dev, journal_blocks)?;
        debug!("[JBD2 commit] tid={tid} commit_block_id={commit_block_id} (absolute)");
        block_dev.write(&commit_buffer, commit_block_id, 1)?;
        block_dev.flush()?;
        self.sequence += 1;

        // Checkpoint: write metadata back to home blocks now that the commit
        // record is safely on disk. If the system crashes here the journal
        // replay will redo these writes, so partial checkpoints are safe.
        for update in self.commit_queue.iter() {
            debug!("[JBD2 checkpoint] tid={} home_phys_block={}", tid, update.0);
            block_dev.write(&update.1[..], update.0, 1)?;
        }
        block_dev.flush()?;

        self.commit_queue.clear();
        debug!("[JBD2 buffer] commit queue cleared");

        self.jbd2_super_block.s_sequence = self.sequence;
        self.jbd2_super_block.s_start = 0;
        self.head = 0;
        self.write_journal_superblock_with_mapping(block_dev, journal_blocks);
        block_dev.flush()?;
        debug!(
            "[JBD2 commit] end: tid={} new_sequence={}",
            tid, self.sequence
        );

        Ok(true)
    }

    /// Replays as many complete committed transactions as possible.
    pub fn replay<B: BlockDevice>(&mut self, block_dev: &mut B) {
        let _ = self.replay_with_mapping(block_dev, &[]);
    }

    fn replay_one_transaction<B: BlockDevice>(
        &self,
        block_dev: &mut B,
        ring: &ReplayRing<'_>,
        start_rel: u32,
        expect_seq: u32,
    ) -> ReplayScan {
        let mut record_rel = start_rel;
        let mut meta_blocks: Vec<(ReplayTag, [u8; BLOCK_SIZE])> = Vec::new();
        let mut revoked_blocks = BTreeSet::new();

        loop {
            let record_phys = match ring.phys(record_rel) {
                Ok(block) => block,
                Err(_) => {
                    return ReplayScan::Incomplete {
                        restart_rel: start_rel,
                    };
                }
            };
            let mut record_buf = [0u8; BLOCK_SIZE];
            if let Err(e) = block_dev.read(&mut record_buf, record_phys, 1) {
                debug!(
                    "[JBD2 replay] read record failed at rel_block={record_rel} \
                     phys_block={record_phys} err={e:?}"
                );
                return ReplayScan::Incomplete {
                    restart_rel: start_rel,
                };
            }

            let hdr = JournalHeaderS::from_disk_bytes(&record_buf[0..JBD2_DESCRIPTOR_HEADER_SIZE]);
            debug!(
                "[JBD2 replay] record: phys_block={} h_magic=0x{:x} h_blocktype={} h_sequence={} \
                 expect_seq={}",
                record_phys, hdr.h_magic, hdr.h_blocktype, hdr.h_sequence, expect_seq
            );

            if hdr.h_magic != JBD2_MAGIC || hdr.h_sequence != expect_seq {
                return if record_rel == start_rel {
                    ReplayScan::CleanEnd
                } else {
                    ReplayScan::Incomplete {
                        restart_rel: start_rel,
                    }
                };
            }

            match hdr.h_blocktype {
                JBD2_BLOCKTYPE_DESCRIPTOR => {
                    let tags = match self.parse_replay_tags(&record_buf, expect_seq) {
                        Some(tags) if !tags.is_empty() => tags,
                        _ => {
                            return ReplayScan::Incomplete {
                                restart_rel: start_rel,
                            };
                        }
                    };

                    for (idx, tag) in tags.iter().enumerate() {
                        ring.advance(&mut record_rel);
                        let meta_phys = match ring.phys(record_rel) {
                            Ok(block) => block,
                            Err(_) => {
                                return ReplayScan::Incomplete {
                                    restart_rel: start_rel,
                                };
                            }
                        };
                        let mut mbuf = [0u8; BLOCK_SIZE];
                        if let Err(e) = block_dev.read(&mut mbuf, meta_phys, 1) {
                            debug!(
                                "[JBD2 replay] read meta block failed: idx={idx} \
                                 rel_block={record_rel} phys_block={meta_phys} err={e:?}"
                            );
                            return ReplayScan::Incomplete {
                                restart_rel: start_rel,
                            };
                        }
                        debug!(
                            "[JBD2 replay] tid={expect_seq} loaded meta_idx={idx} from \
                             rel_block={record_rel} phys_block={meta_phys}"
                        );
                        meta_blocks.push((*tag, mbuf));
                    }
                }
                JBD2_BLOCKTYPE_COMMIT => {
                    for (idx, (tag, data)) in meta_blocks.iter_mut().enumerate() {
                        let phys = tag.block;
                        if revoked_blocks.contains(&phys) {
                            debug!(
                                "[JBD2 replay] tid={expect_seq} skip revoked meta_idx={idx} \
                                 phys_block={phys}"
                            );
                            continue;
                        }

                        if (tag.flags & u32::from(JOURNAL_ESCAPE)) != 0 {
                            data[0..4].copy_from_slice(&JBD2_MAGIC.to_be_bytes());
                            debug!("[JBD2 replay] restored escaped journal magic for block {phys}");
                        }
                        debug!(
                            "[JBD2 replay] tid={expect_seq} apply meta_idx={idx} phys_block={phys}"
                        );

                        if let Err(e) = block_dev.write(data, phys, 1) {
                            debug!(
                                "[JBD2 replay] write meta block failed: idx={idx} \
                                 phys_block={phys} err={e:?}"
                            );
                            return ReplayScan::Incomplete {
                                restart_rel: start_rel,
                            };
                        }
                    }
                    if let Err(e) = block_dev.flush() {
                        debug!("[JBD2 replay] flush after transaction failed: err={e:?}");
                        return ReplayScan::Incomplete {
                            restart_rel: start_rel,
                        };
                    }

                    let mut next_rel = record_rel;
                    ring.advance(&mut next_rel);
                    return ReplayScan::Applied {
                        next_rel,
                        next_seq: expect_seq.wrapping_add(1),
                    };
                }
                JBD2_BLOCKTYPE_REVOKE => {
                    let blocks = match self.parse_revoke_blocks(&record_buf, expect_seq) {
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
                    return if record_rel == start_rel {
                        ReplayScan::CleanEnd
                    } else {
                        ReplayScan::Incomplete {
                            restart_rel: start_rel,
                        }
                    };
                }
            }

            ring.advance(&mut record_rel);
        }
    }

    /// Replays committed transactions using the journal inode logical-block map.
    pub(crate) fn replay_with_mapping<B: BlockDevice>(
        &mut self,
        block_dev: &mut B,
        journal_blocks: &[AbsoluteBN],
    ) -> ReplayStatus {
        let mut journal_rel = self.jbd2_super_block.s_start;
        if journal_rel == 0 {
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

        debug!(
            "[JBD2 replay] begin: journal_sb_phys={} first_rel={} last_rel={} s_start(rel)={} \
             maxlen={} expect_seq={}",
            self.start_block, ring.first_rel, ring.last_rel, journal_rel, maxlen, expect_seq,
        );

        let status = loop {
            match self.replay_one_transaction(block_dev, &ring, journal_rel, expect_seq) {
                ReplayScan::Applied { next_rel, next_seq } => {
                    journal_rel = next_rel;
                    expect_seq = next_seq;
                    self.jbd2_super_block.s_start = journal_rel;
                    self.jbd2_super_block.s_sequence = expect_seq;
                    self.sequence = expect_seq;
                    debug!(
                        "[JBD2 replay] transaction applied: new_sequence={} new_s_start(rel)={}",
                        self.jbd2_super_block.s_sequence, self.jbd2_super_block.s_start
                    );
                }
                ReplayScan::CleanEnd => {
                    self.jbd2_super_block.s_start = 0;
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
        if sb_block.raw() != 0 {
            debug!(
                "[JBD2 replay] write journal superblock to block={} (sequence={} s_start={})",
                sb_block, self.jbd2_super_block.s_sequence, self.jbd2_super_block.s_start
            );
            self.write_journal_superblock_with_mapping(block_dev, journal_blocks);
            let _ = block_dev.flush();
        }
        debug!(
            "[JBD2 replay] end: final_sequence={} final_s_start={} ",
            self.jbd2_super_block.s_sequence, self.jbd2_super_block.s_start
        );

        status
    }
}

/// Debug helper that dumps the journal inode and journal superblock.
pub fn dump_journal_inode<B: BlockDevice>(fs: &mut Ext4FileSystem, block_dev: &mut Jbd2Dev<B>) {
    let journal_ino = InodeNumber::new(8).expect("valid journal inode number");
    let mut indo = fs
        .get_inode_by_num(block_dev, journal_ino)
        .expect("journal");
    let datablock = resolve_inode_block(block_dev, &mut indo, 0)
        .unwrap()
        .unwrap();
    let journal_data = fs
        .datablock_cache
        .get_or_load(block_dev, datablock)
        .unwrap()
        .data
        .clone();
    let sb = JournalSuperBllockS::from_disk_bytes(&journal_data);
    debug!("Journal Superblock:{sb:?}");
    debug!("Journal Inode:{indo:?}");
}

/// Creates the journal inode and writes its initial journal superblock.
pub fn create_journal_entry<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
) -> Ext4Result<()> {
    // Allocate the journal area. Block 0 stores the journal superblock and the
    // remaining blocks hold descriptor/data/commit traffic.
    let journal_inode_num = JOURNAL_FILE_INODE;
    let free_block = fs
        .alloc_blocks(block_dev, 4096)
        .expect("No enough block can alloc out!");

    // Ensure journal area starts clean: otherwise old image contents could look like valid
    // descriptor/commit blocks and replay would corrupt filesystem metadata.
    let zero = [0u8; BLOCK_SIZE];
    for &b in free_block.iter() {
        block_dev.write_blocks(&zero, b, 1, true)?;
    }
    // Build the journal inode metadata and map the allocated journal blocks.
    let mut jour_inode = Ext4Inode::empty_for_reuse(fs.default_inode_extra_isize());
    jour_inode.i_links_count = 1;
    debug!(
        "When creating journal inode: iblock={:?}",
        jour_inode.i_block
    );
    let inode_size: usize = BLOCK_SIZE * free_block.len();
    jour_inode.i_size_lo = inode_size as u32;
    jour_inode.i_size_high = 0;
    jour_inode.i_blocks_lo = (inode_size / 512) as u32;
    jour_inode.l_i_blocks_high = 0;
    jour_inode.write_extend_header();
    build_file_block_mapping(fs, &mut jour_inode, &free_block, block_dev);
    debug!(
        "When creating journal inode: iblock={:?}",
        jour_inode.i_block
    );
    fs.finalize_inode_update(
        block_dev,
        InodeNumber::new(journal_inode_num as u32)?,
        &mut jour_inode,
        Ext4InodeMetadataUpdate::create(Ext4Inode::S_IFREG | 0o600),
    )
    .expect("journal inode creation failed");

    let mut jbd2_sb = JournalSuperBllockS::default();

    if ext4_superblock_has_metadata_csum(&fs.superblock) {
        jbd2_sb.s_checksum_type = JBD2_CRC32C_CHKSUM;
    } else {
        jbd2_sb.s_checksum_type = 0;
    }

    // The first allocated block stores the journal superblock itself, so the
    // usable log length excludes that block and starts at relative block 1.
    jbd2_sb.s_maxlen = (free_block.len() - 1) as u32;
    jbd2_sb.s_start = 0;
    jbd2_sb.s_blocksize = BLOCK_SIZE_U32;
    jbd2_sb.s_sequence = 1;
    jbd2_sb.s_first = 1;
    jbd2_sb.s_uuid = fs.superblock.s_uuid;
    jbd2_update_superblock_checksum(&mut jbd2_sb);

    fs.datablock_cache
        .modify_new(block_dev, free_block[0], |data| {
            jbd2_sb.to_disk_bytes(data);
        })?;
    info!("Journal inode created!");
    Ok(())
}
