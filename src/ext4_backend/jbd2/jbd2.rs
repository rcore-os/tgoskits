use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::endian::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::file::*;
use crate::ext4_backend::jbd2::jbdstruct::*;
use crate::ext4_backend::loopfile::*;
use crate::ext4_backend::error::*;
use alloc::vec;
use log::debug;
use log::error;
use log::info;
use log::warn;

use alloc::vec::Vec;
use log::trace;

impl JBD2DEVSYSTEM {
    ///计算下一个日志块的位置(处理回绕),返回当前的（可以直接用，直接写，已经处理过偏移）!
    pub fn set_next_log_block(&mut self) -> u32 {
        let mut next = self.head + 1;
        if next >= self.max_len {
            next = 1; //跳过0
        }
        self.head = next;
        next + self.start_block
    }
    ///提交事务
    /// 允许使用原始块设备!
    /// update:Vec<JBD2_UPDATE>
    pub fn commit_transaction<B: BlockDevice>(&mut self, block_dev: &mut B) -> Result<bool, ()> {
        let tid = self.sequence; //事务id
        error!(
            "[JBD2 commit] begin: tid={} updates_len={} head={} start_block={} max_len={} seq_in_superblock={} s_start={}",
            tid,
            self.commit_queue.len(),
            self.head,
            self.start_block,
            self.max_len,
            self.jbd2_super_block.s_sequence,
            self.jbd2_super_block.s_start,
        );

        if self.commit_queue.len() <= 0 {
            warn!("No thing need to commit");
            return Ok(false);
        }

        let mut desc_buffer = vec![0; BLOCK_SIZE];

        //写header->内存缓存
        let mut new_jbd_header = JournalHeaderS::default();
        new_jbd_header.h_blocktype = 1; //Descriptor
        new_jbd_header.h_sequence = tid; //设置事务id
        new_jbd_header.to_disk_bytes(&mut desc_buffer[0..JournalHeaderS::disk_size()]);

        let mut current_offset = 12; //跳过头
        //写many tag，目前开发测试简化为一个descriptor块能塞下:)
        for (idx, update) in self.commit_queue.iter().enumerate() {
            //检查逃逸escape 如果数据块开头也是jbd2_magic 要标志逃逸
            let mut tag = JournalBlockTagS {
                t_blocknr: update.0 as u32,
                t_checksum: 0,
                t_flags: 0, //后面记得处理逃逸
            };
            let magic: u32 = u32::from_le_bytes(update.1[0..4].try_into().unwrap());
            if magic == JBD2_MAGIC {
                tag.t_flags |= JOURANL_ESCAPE;
                error!("JOURNAL ERROR ,Updates data escape!!!");
            }

            //最后一个
            if idx == self.commit_queue.len() - 1 {
                tag.t_flags |= JBD2_FLAG_LAST_TAG;
            }
            trace!(
                "[JBD2 commit] tid={} tag_idx={} t_blocknr={} t_flags=0x{:x}",
                tid, idx, tag.t_blocknr, tag.t_flags,
            );
            tag.to_disk_bytes(&mut desc_buffer[current_offset..current_offset + 8]);
            current_offset += 8;
        }

        //实际写入盘 这里可以直接写
        let block_id = self.set_next_log_block();
        trace!(
            "[JBD2 commit] tid={tid} descriptor_block_id={block_id} (absolute)"
        );
        block_dev.write(&desc_buffer, block_id, 1).expect("Jouranl block write failed!");

        let mut no_escape: Vec<(u64, [u8; BLOCK_SIZE])> = Vec::new();
        //逃逸处理
        for update in self.commit_queue.iter() {
            //逃逸处理
            let mut check_data: [u8; BLOCK_SIZE] = [0; BLOCK_SIZE];
            check_data.copy_from_slice(&update.1);
            let magic = u32::from_le_bytes(check_data[0..4].try_into().unwrap());
            if magic == JBD2_MAGIC {
                error!("Find excape data,will fill 0");
                check_data[0..4].fill(0);
            }
            no_escape.push((update.0, check_data));
        }

        //写实际的metadata CORE!!!!!
        for (idx, up) in no_escape.iter().enumerate() {
            let metadata_journal_block_id = self.set_next_log_block();
            trace!(
                "[JBD2 commit] tid={} meta_idx={} journal_block_id={} (absolute) target_phys_block={}",
                tid, idx, metadata_journal_block_id, up.0
            );
            block_dev.write(&up.1, metadata_journal_block_id, 1).expect("Jouranl block write failed!");
        }

        block_dev.flush().expect("Jouranl block write failed!");

        //清空update缓存
        self.commit_queue.clear();
        trace!("[JBD2 BUFFER] BUFFER ALREADY CLEA");

        //写入Commit Block

        let mut commit_buffer = [0_u8; BLOCK_SIZE];

        let commit_block = CommitHeader {
            //commit block type 2
            h_header: JournalHeaderS {
                h_magic: JBD2_MAGIC,
                h_blocktype: 2,
                h_sequence: tid,
            }, //注意完成的tid
            h_chksum_type: 0,
            h_chksum_size: 0,
            h_padding: [0; 2],
            h_chksum: [0; 8],
            h_commit_sec: 0, //提交时间
            h_commit_nsec: 0,
        };

        commit_block.to_disk_bytes(&mut commit_buffer);
        let commit_block_id = self.set_next_log_block();
        trace!(
            "[JBD2 commit] tid={tid} commit_block_id={commit_block_id} (absolute)"
        );
        block_dev.write(&commit_buffer, commit_block_id, 1).expect("Jouranl block write failed!");
        //至此，commit已经完成，metadata数据已经安全:）
        block_dev.flush().expect("Jouranl block write failed!");
        self.sequence += 1;
        trace!(
            "[JBD2 commit] end: tid={} new_sequence={}",
            tid, self.sequence
        );

        //注意此时head指向下一个可用的块
        Ok(true)
    }

    ///事务重放：从当前 superblock 状态开始，尽可能重放连续的完整事务
    pub fn replay<B: BlockDevice>(&mut self, block_dev: &mut B) {
        // 注意：journal_superblock_s 里的 s_first / s_start 是“日志区内部的相对块号”，
        // 真实物理块号 = self.start_block + 相对块号。
        // 我们在内存里一直用相对块号 cur_rel/first，相对 [0..maxlen) 或 [1..maxlen)，
        // 只有真正读写设备时才加上 start_block 偏移。

        // 扫描起点（相对块号）：优先用 s_start，没有则从 s_first 开始
        let mut cur_rel = self.jbd2_super_block.s_start;
        if cur_rel == 0 {
            cur_rel = self.jbd2_super_block.s_first;
        }

        let first = self.jbd2_super_block.s_first; // 相对块号
        let maxlen = self.jbd2_super_block.s_maxlen; // 日志总块数
        let mut expect_seq = self.jbd2_super_block.s_sequence;

        // 简单防护：maxlen 为 0 直接返回
        if maxlen == 0 {
            return;
        }

        trace!(
            "[JBD2 replay] begin: start_block={} first(rel)={} maxlen={} expect_seq={} cur_rel={} s_start(rel)={} s_sequence={}",
            self.start_block,
            first,
            maxlen,
            expect_seq,
            cur_rel,
            self.jbd2_super_block.s_start,
            self.jbd2_super_block.s_sequence,
        );

        loop {
            // 1) 读取 descriptor 块并做基本校验
            let mut desc_buf = [0u8; BLOCK_SIZE];
            let desc_phys = self.start_block + cur_rel; // 物理块号
            if let Err(e) = block_dev.read(&mut desc_buf, desc_phys, 1) {
                trace!(
                    "[JBD2 replay] read descriptor failed at rel_block={cur_rel} phys_block={desc_phys} err={e:?}"
                );
                break;
            }

            let hdr = JournalHeaderS::from_disk_bytes(&desc_buf[0..12]);
            trace!(
                "[JBD2 replay] descriptor: rel_block={} phys_block={} h_magic=0x{:x} h_blocktype={} h_sequence={} expect_seq={}",
                cur_rel, desc_phys, hdr.h_magic, hdr.h_blocktype, hdr.h_sequence, expect_seq
            );
            if hdr.h_magic != JBD2_MAGIC || hdr.h_blocktype != 1 {
                // 不是合法的 descriptor，认为后面没有可重放事务
                break;
            }
            if hdr.h_sequence != expect_seq {
                // 序列号不匹配，认为没有更多可重放事务
                break;
            }

            // 2) 解析 descriptor 里的 tags
            let mut tags: Vec<JournalBlockTagS> = Vec::new();
            let mut off = 12usize; // 跳过 header
            let mut tag_idx = 0usize;
            while off + 8 <= BLOCK_SIZE {
                let tag = JournalBlockTagS::from_disk_bytes(&desc_buf[off..off + 8]);

                // 简单退出条件：全 0 视为没有更多 tag
                if tag.t_blocknr == 0 && tag.t_checksum == 0 && tag.t_flags == 0 {
                    break;
                }

                trace!(
                    "[JBD2 replay] tid={} tag_idx={} t_blocknr={} t_flags=0x{:x}",
                    expect_seq, tag_idx, tag.t_blocknr, tag.t_flags
                );

                let last = (tag.t_flags & JBD2_FLAG_LAST_TAG) != 0;
                tags.push(tag);
                off += 8;
                tag_idx += 1;

                if last {
                    break;
                }
            }

            if tags.is_empty() {
                // 没有任何 tag，无事务可重放
                break;
            }

            // 3) 读取对应数量的 metadata 日志块
            let mut meta_blocks: Vec<[u8; BLOCK_SIZE]> = Vec::new();
            for (idx, _) in tags.iter().enumerate() {
                // 下一个块（注意处理回绕），仍然用相对块号
                cur_rel += 1;
                if cur_rel - first >= maxlen {
                    // 环绕
                    cur_rel = first;
                }

                let meta_phys = self.start_block + cur_rel;
                let mut mbuf = [0u8; BLOCK_SIZE];
                if let Err(e) = block_dev.read(&mut mbuf, meta_phys, 1) {
                    trace!(
                        "[JBD2 replay] read meta block failed: idx={idx} rel_block={cur_rel} phys_block={meta_phys} err={e:?}"
                    );
                    return;
                }
                trace!(
                    "[JBD2 replay] tid={expect_seq} loaded meta_idx={idx} from journal_rel_block={cur_rel} phys_block={meta_phys}"
                );
                meta_blocks.push(mbuf);
            }

            // 4) 读取 commit 块并验证
            cur_rel += 1;
            if cur_rel - first >= maxlen {
                cur_rel = first;
            }

            let commit_phys = self.start_block + cur_rel;
            let mut cbuf = [0u8; BLOCK_SIZE];
            if let Err(e) = block_dev.read(&mut cbuf, commit_phys, 1) {
                trace!(
                    "[JBD2 replay] read commit failed at rel_block={cur_rel} phys_block={commit_phys} err={e:?}"
                );
                return;
            }
            let chdr = JournalHeaderS::from_disk_bytes(&cbuf[0..12]);
            trace!(
                "[JBD2 replay] commit: rel_block={} phys_block={} h_magic=0x{:x} h_blocktype={} h_sequence={} expect_seq={}",
                cur_rel, commit_phys, chdr.h_magic, chdr.h_blocktype, chdr.h_sequence, expect_seq
            );
            if chdr.h_magic != JBD2_MAGIC || chdr.h_blocktype != 2 || chdr.h_sequence != expect_seq
            {
                // 没有匹配的 commit，事务不完整，不再继续
                break;
            }

            // 5) 真正重放：把每个 metadata 块写回主盘对应的 t_blocknr
            for (i, tag) in tags.iter().enumerate() {
                let phys = tag.t_blocknr;
                let data = &mut meta_blocks[i];

                //检查是否逃逸
                if (tag.t_flags & 1) != 0 {
                    // JBD2_FLAG_ESCAPE = 1
                    let magic_bytes = JBD2_MAGIC.to_be_bytes();
                    data[0] = magic_bytes[0];
                    data[1] = magic_bytes[1];
                    data[2] = magic_bytes[2];
                    data[3] = magic_bytes[3];
                    trace!("Restored JBD2 Magic for block {phys}");
                }
                trace!(
                    "[JBD2 replay] tid={expect_seq} apply meta_idx={i} to phys_block={phys} (journal data from idx={i})"
                );

                let _ = block_dev.write(data, phys, 1);
            }
            let _ = block_dev.flush();

            // 6) 更新内存中的 journal superblock 状态
            expect_seq = expect_seq.wrapping_add(1);
            self.jbd2_super_block.s_sequence = expect_seq;

            // s_start 指向下一个事务起点（当前 commit 后一块），保持为“相对块号”
            cur_rel += 1;
            if cur_rel - first >= maxlen {
                cur_rel = first;
            }
            trace!(
                "[JBD2 replay] transaction applied: new_sequence={} new_s_start(rel)={} (journal rel_cur={})",
                self.jbd2_super_block.s_sequence, cur_rel, cur_rel
            );
            self.jbd2_super_block.s_start = cur_rel;

            // 7) 将更新后的 journal superblock 写回磁盘
            let mut sb_buf = [0u8; 1024];
            self.jbd2_super_block.to_disk_bytes(&mut sb_buf);

            // 约定 journal superblock 位于 start_block
            let sb_block = self.start_block;
            if sb_block != 0 {
                trace!(
                    "[JBD2 replay] write journal superblock to block={} (sequence={} s_start={})",
                    sb_block, self.jbd2_super_block.s_sequence, self.jbd2_super_block.s_start
                );
                let _ = block_dev.write(&sb_buf, sb_block, 1);
                let _ = block_dev.flush();
            }
        }
        debug!(
        "[JBD2 replay] end: final_sequence={} final_s_start={} ",
        self.jbd2_super_block.s_sequence, self.jbd2_super_block.s_start
    );
    }
    
}

///dump jouranl inode
pub fn dump_journal_inode<B: BlockDevice>(fs: &mut Ext4FileSystem, block_dev: &mut Jbd2Dev<B>) {
    let mut indo = fs.get_inode_by_num(block_dev, 8).expect("journal");
    let datablock = resolve_inode_block( block_dev, &mut indo, 0)
        .unwrap()
        .unwrap();
    let journal_data = fs
        .datablock_cache
        .get_or_load(block_dev, datablock as u64)
        .unwrap()
        .data
        .clone();
    let sb = JournalSuperBllockS::from_disk_bytes(&journal_data);
    error!("Journal Superblock:{sb:?}");
    error!("Jouranl Inode:{indo:?}");
}

///jouranl目录创建 journal超级块写入
pub fn create_journal_entry<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
) -> BlockDevResult<()> {
    //分配新数据块放superblock
    let journal_inode_num = JOURNAL_FILE_INODE;
    let free_block = fs
        .alloc_blocks(block_dev, 4096)
        .expect("No enough block can alloc out!");

    // Ensure journal area starts clean: otherwise old image contents could look like valid
    // descriptor/commit blocks and replay would corrupt filesystem metadata.
    let zero = [0u8; BLOCK_SIZE];
    for &b in free_block.iter() {
        block_dev.write_blocks(&zero, b as u32, 1, true)?;
    }
    //journal inode 额外参数
    let mut jour_inode = fs
        .get_inode_by_num(block_dev, journal_inode_num as u32)
        .unwrap();
    jour_inode.write_extend_header();
    build_file_block_mapping(fs, &mut jour_inode, &free_block, block_dev);
    debug!("When create jouranl inode: iblock:{:?}", jour_inode.i_block);
    let inode_size: usize = BLOCK_SIZE * free_block.len();
    //初始化 然后写入 journal inode
    fs.modify_inode(block_dev, journal_inode_num as u32, |inode| {
        inode.i_mode = Ext4Inode::S_IFREG | 0o600;
        inode.i_links_count = 1;
        inode.i_size_lo = inode_size as u32;
        inode.i_flags = Ext4Inode::EXT4_EXTENTS_FL;
        inode.i_blocks_lo = (inode_size / 512) as u32;
        inode.i_block = jour_inode.i_block;
    })
    .expect("Jouranl inode create faild!");

    let mut jbd2_sb = JournalSuperBllockS::default();

    jbd2_sb.s_maxlen = free_block.len() as u32; //修正块数
    jbd2_sb.s_start = 0; //相对于superblock
    jbd2_sb.s_blocksize = BLOCK_SIZE_U32;
    jbd2_sb.s_sequence = 1;

    fs.datablock_cache.modify_new(free_block[0], |data| {
        jbd2_sb.to_disk_bytes(data);
    });
    info!("Journal inode created!");
    Ok(())
}
