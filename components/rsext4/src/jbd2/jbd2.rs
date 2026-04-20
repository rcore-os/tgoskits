//! JBD2 transaction commit and replay logic.

use alloc::{vec, vec::Vec};

use log::{debug, info, warn};

use crate::{
    blockdev::*,
    bmalloc::{AbsoluteBN, InodeNumber},
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

impl JBD2DEVSYSTEM {
    fn write_journal_superblock<B: BlockDevice>(&self, block_dev: &mut B) {
        let sb_block = self.start_block;
        let mut sb_data = [0u8; BLOCK_SIZE];
        block_dev
            .read(&mut sb_data, sb_block, 1)
            .expect("Read journal superblock failed");
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
        // The first commit initializes `s_start` in the journal superblock.
        if self.jbd2_super_block.s_start == 0 {
            self.jbd2_super_block.s_start = self.jbd2_super_block.s_first;
            self.write_journal_superblock(block_dev);
            self.head += 1;
            let rel = self
                .jbd2_super_block
                .s_start
                .checked_add(self.head)
                .and_then(|v| v.checked_sub(1))
                .ok_or_else(Ext4Error::invalid_input)?;
            let mut target_use = self.start_block.checked_add(rel)?;
            // Wrap when the cursor runs past the end of the journal ring.
            if target_use.raw().saturating_sub(self.start_block.raw()) > u64::from(self.max_len) {
                self.head = 0;
                target_use = self
                    .start_block
                    .checked_add(self.jbd2_super_block.s_start)?;
            }
            Ok(target_use)
        } else {
            self.head += 1;
            let rel = self
                .jbd2_super_block
                .s_start
                .checked_add(self.head)
                .and_then(|v| v.checked_sub(1))
                .ok_or_else(Ext4Error::invalid_input)?;
            let mut target_use = self.start_block.checked_add(rel)?;
            if target_use.raw().saturating_sub(self.start_block.raw()) > u64::from(self.max_len) {
                self.head = 0;
                target_use = self
                    .start_block
                    .checked_add(self.jbd2_super_block.s_start)?;
            }
            Ok(target_use)
        }
    }
    /// Commits the currently queued metadata updates as one transaction.
    pub fn commit_transaction<B: BlockDevice>(&mut self, block_dev: &mut B) -> Ext4Result<bool> {
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
            warn!("No thing need to commit");
            return Ok(false);
        }

        let mut desc_buffer = vec![0; BLOCK_SIZE];

        // Build the descriptor block in memory first.
        let new_jbd_header = JournalHeaderS {
            h_blocktype: 1,
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
                tag.t_flags |= JOURANL_ESCAPE;
                debug!("JOURNAL ERROR ,Updates data escape!!!");
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
        let block_id = self.set_next_log_block(block_dev)?;
        debug!("[JBD2 commit] tid={tid} descriptor_block_id={block_id} (absolute)");
        block_dev.write(&desc_buffer, block_id, 1)?;

        let mut no_escape: Vec<(AbsoluteBN, [u8; BLOCK_SIZE])> = Vec::new();
        for update in self.commit_queue.iter() {
            let mut check_data: [u8; BLOCK_SIZE] = [0; BLOCK_SIZE];
            check_data.copy_from_slice(&*update.1);
            let magic = u32::from_le_bytes(check_data[0..4].try_into().unwrap());
            if magic == JBD2_MAGIC {
                debug!("Find excape data,will fill 0");
                check_data[0..4].fill(0);
            }
            no_escape.push((update.0, check_data));
        }

        // Then write the journaled metadata payload blocks.
        for (idx, up) in no_escape.iter().enumerate() {
            let metadata_journal_block_id = self.set_next_log_block(block_dev)?;
            debug!(
                "[JBD2 commit] tid={} meta_idx={} journal_block_id={} (absolute) \
                 target_phys_block={}",
                tid, idx, metadata_journal_block_id, up.0
            );
            block_dev.write(&up.1, metadata_journal_block_id, 1)?;
        }

        block_dev.flush()?;

        // Checkpoint: write metadata back to home blocks before marking journal clean.
        for update in self.commit_queue.iter() {
            debug!("[JBD2 checkpoint] tid={} home_phys_block={}", tid, update.0);
            block_dev.write(&update.1[..], update.0, 1)?;
        }
        block_dev.flush()?;

        self.commit_queue.clear();
        debug!("[JBD2 BUFFER] BUFFER ALREADY CLEA");

        let mut commit_buffer = [0_u8; BLOCK_SIZE];

        let commit_block = CommitHeader {
            h_header: JournalHeaderS {
                h_magic: JBD2_MAGIC,
                h_blocktype: 2,
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
        let commit_block_id = self.set_next_log_block(block_dev)?;
        debug!("[JBD2 commit] tid={tid} commit_block_id={commit_block_id} (absolute)");
        block_dev.write(&commit_buffer, commit_block_id, 1)?;
        block_dev.flush()?;
        self.sequence += 1;

        self.jbd2_super_block.s_sequence = self.sequence;
        self.jbd2_super_block.s_start = 0;
        self.head = 0;
        self.write_journal_superblock(block_dev);
        block_dev.flush()?;
        debug!(
            "[JBD2 commit] end: tid={} new_sequence={}",
            tid, self.sequence
        );

        Ok(true)
    }

    /// Replays as many complete committed transactions as possible.
    pub fn replay<B: BlockDevice>(&mut self, block_dev: &mut B) {
        // `s_first` and `s_start` are relative to the journal superblock block.
        let mut journal_rel = self.jbd2_super_block.s_start;
        if journal_rel == 0 {
            return;
        }

        let first_rel = self.jbd2_super_block.s_first;
        let maxlen = self.jbd2_super_block.s_maxlen;
        let last_rel = first_rel.saturating_add(maxlen.saturating_sub(1));
        let mut expect_seq = self.jbd2_super_block.s_sequence;

        if maxlen == 0 {
            return;
        }

        debug!(
            "[JBD2 replay] begin: journal_sb_phys={} first_rel={} last_rel={} s_start(rel)={} \
             maxlen={} expect_seq={}",
            self.start_block, first_rel, last_rel, journal_rel, maxlen, expect_seq,
        );

        // Advance one relative journal block, wrapping around the ring buffer.
        let advance_rel = |rel: &mut u32| {
            if *rel >= last_rel {
                *rel = first_rel;
            } else {
                *rel = rel.saturating_add(1);
            }
        };

        loop {
            // 1. Load and validate the descriptor block.
            let mut desc_buf = [0u8; BLOCK_SIZE];
            let desc_phys = match self.start_block.checked_add(journal_rel) {
                Ok(block) => block,
                Err(_) => break,
            };
            if let Err(e) = block_dev.read(&mut desc_buf, desc_phys, 1) {
                debug!(
                    "[JBD2 replay] read descriptor failed at rel_block={journal_rel} \
                     phys_block={desc_phys} err={e:?}"
                );
                break;
            }

            let hdr = JournalHeaderS::from_disk_bytes(&desc_buf[0..JBD2_DESCRIPTOR_HEADER_SIZE]);
            debug!(
                "[JBD2 replay] descriptor: phys_block={} h_magic=0x{:x} h_blocktype={} \
                 h_sequence={} expect_seq={}",
                desc_phys, hdr.h_magic, hdr.h_blocktype, hdr.h_sequence, expect_seq
            );
            if hdr.h_magic != JBD2_MAGIC || hdr.h_blocktype != 1 {
                break;
            }
            if hdr.h_sequence != expect_seq {
                break;
            }

            // 2. Parse tags out of the descriptor block.
            let mut tags: Vec<JournalBlockTagS> = Vec::new();
            let mut off = JBD2_DESCRIPTOR_HEADER_SIZE;
            let mut tag_idx = 0usize;
            while off + JBD2_TAG_SIZE <= BLOCK_SIZE {
                let tag = JournalBlockTagS::from_disk_bytes(&desc_buf[off..off + JBD2_TAG_SIZE]);

                // `t_blocknr == 0` is valid for metadata, so only treat an all-zero
                // tag plus all-zero trailing padding as end-of-descriptor.
                if tag.t_blocknr == 0
                    && tag.t_checksum == 0
                    && tag.t_flags == 0
                    && desc_buf[off + JBD2_TAG_SIZE..].iter().all(|b| *b == 0)
                {
                    break;
                }

                debug!(
                    "[JBD2 replay] tid={} tag_idx={} t_blocknr={} t_flags=0x{:x}",
                    expect_seq, tag_idx, tag.t_blocknr, tag.t_flags
                );

                let last = (tag.t_flags & JBD2_FLAG_LAST_TAG) != 0;
                let same_uuid = (tag.t_flags & JBD2_FLAG_SAME_UUID) != 0;
                tags.push(tag);
                off += JBD2_TAG_SIZE;
                if !same_uuid {
                    if off + JBD2_UUID_SIZE > BLOCK_SIZE {
                        debug!(
                            "[JBD2 replay] descriptor uuid truncated: tid={} tag_idx={}",
                            expect_seq, tag_idx
                        );
                        return;
                    }
                    off += JBD2_UUID_SIZE;
                }
                tag_idx += 1;

                if last {
                    break;
                }
            }

            if tags.is_empty() {
                break;
            }

            // 3. Load the journaled metadata payload blocks.
            let mut meta_blocks: Vec<[u8; BLOCK_SIZE]> = Vec::new();
            for (idx, _) in tags.iter().enumerate() {
                advance_rel(&mut journal_rel);
                let meta_phys = match self.start_block.checked_add(journal_rel) {
                    Ok(block) => block,
                    Err(_) => return,
                };
                let mut mbuf = [0u8; BLOCK_SIZE];
                if let Err(e) = block_dev.read(&mut mbuf, meta_phys, 1) {
                    debug!(
                        "[JBD2 replay] read meta block failed: idx={idx} rel_block={journal_rel} \
                         phys_block={meta_phys} err={e:?}"
                    );
                    return;
                }
                debug!(
                    "[JBD2 replay] tid={expect_seq} loaded meta_idx={idx} from \
                     rel_block={journal_rel} phys_block={meta_phys}"
                );
                meta_blocks.push(mbuf);
            }

            // 4. Read and validate the matching commit block.
            advance_rel(&mut journal_rel);
            let commit_rel = journal_rel;
            let commit_phys = match self.start_block.checked_add(commit_rel) {
                Ok(block) => block,
                Err(_) => return,
            };
            let mut cbuf = [0u8; BLOCK_SIZE];
            if let Err(e) = block_dev.read(&mut cbuf, commit_phys, 1) {
                debug!(
                    "[JBD2 replay] read commit failed at rel_block={commit_rel} \
                     phys_block={commit_phys} err={e:?}"
                );
                return;
            }
            let chdr = JournalHeaderS::from_disk_bytes(&cbuf[0..12]);
            debug!(
                "[JBD2 replay] commit: rel_block={} phys_block={} h_magic=0x{:x} h_blocktype={} \
                 h_sequence={} expect_seq={}",
                commit_rel,
                commit_phys,
                chdr.h_magic,
                chdr.h_blocktype,
                chdr.h_sequence,
                expect_seq
            );
            if chdr.h_magic != JBD2_MAGIC || chdr.h_blocktype != 2 || chdr.h_sequence != expect_seq
            {
                break;
            }

            // 5. Replay the metadata blocks onto their home locations.
            for (i, tag) in tags.iter().enumerate() {
                let phys = AbsoluteBN::from(tag.t_blocknr);
                let data = &mut meta_blocks[i];

                // Restore the leading journal magic for escaped blocks before
                // copying them back to their home location.
                if (tag.t_flags & 1) != 0 {
                    let magic_bytes = JBD2_MAGIC.to_be_bytes();
                    data[0] = magic_bytes[0];
                    data[1] = magic_bytes[1];
                    data[2] = magic_bytes[2];
                    data[3] = magic_bytes[3];
                    debug!("Restored JBD2 Magic for block {phys}");
                }
                debug!(
                    "[JBD2 replay] tid={expect_seq} apply meta_idx={i} to phys_block={phys} \
                     (journal data from idx={i})"
                );

                let _ = block_dev.write(data, phys, 1);
            }
            let _ = block_dev.flush();

            // 6. Advance the in-memory journal-superblock state.
            expect_seq = expect_seq.wrapping_add(1);
            self.jbd2_super_block.s_sequence = expect_seq;
            self.sequence = expect_seq;

            // `s_start` always points at the next descriptor block.
            let mut next_desc_rel = commit_rel;
            advance_rel(&mut next_desc_rel);
            self.jbd2_super_block.s_start = next_desc_rel;

            debug!(
                "[JBD2 replay] transaction applied: new_sequence={} new_s_start(rel)={}",
                self.jbd2_super_block.s_sequence, self.jbd2_super_block.s_start
            );

            journal_rel = next_desc_rel;
        }

        // No more complete transactions remain; mark the log clean.
        self.jbd2_super_block.s_start = 0;

        self.head = 0;

        // Write back the updated journal superblock without disturbing the rest
        // of the containing block.
        let sb_block = self.start_block;
        if sb_block.raw() != 0 {
            let mut blk = [0u8; BLOCK_SIZE];
            if block_dev.read(&mut blk, sb_block, 1).is_ok() {
                self.jbd2_super_block.to_disk_bytes(&mut blk[0..1024]);
                debug!(
                    "[JBD2 replay] write journal superblock to block={} (sequence={} s_start={})",
                    sb_block, self.jbd2_super_block.s_sequence, self.jbd2_super_block.s_start
                );
                let _ = block_dev.write(&blk, sb_block, 1);
                let _ = block_dev.flush();
            }
        }
        debug!(
            "[JBD2 replay] end: final_sequence={} final_s_start={} ",
            self.jbd2_super_block.s_sequence, self.jbd2_super_block.s_start
        );
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
    debug!("Jouranl Inode:{indo:?}");
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
    debug!("When create jouranl inode: iblock:{:?}", jour_inode.i_block);
    let inode_size: usize = BLOCK_SIZE * free_block.len();
    jour_inode.i_size_lo = inode_size as u32;
    jour_inode.i_size_high = 0;
    jour_inode.i_blocks_lo = (inode_size / 512) as u32;
    jour_inode.l_i_blocks_high = 0;
    jour_inode.write_extend_header();
    build_file_block_mapping(fs, &mut jour_inode, &free_block, block_dev);
    debug!("When create jouranl inode: iblock:{:?}", jour_inode.i_block);
    fs.finalize_inode_update(
        block_dev,
        InodeNumber::new(journal_inode_num as u32)?,
        &mut jour_inode,
        Ext4InodeMetadataUpdate::create(Ext4Inode::S_IFREG | 0o600),
    )
    .expect("Jouranl inode create faild!");

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

    fs.datablock_cache
        .modify_new(block_dev, free_block[0], |data| {
            jbd2_sb.to_disk_bytes(data);
        })?;
    info!("Journal inode created!");
    Ok(())
}
