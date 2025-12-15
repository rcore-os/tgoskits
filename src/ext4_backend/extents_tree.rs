use log::{debug, error};

use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::endian::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::error::*;
use alloc::vec;
use alloc::vec::*;

/// 内存中的 extent 树节点表示
#[derive(Clone)]
pub enum ExtentNode {
    /// 叶子节点：header.eh_depth == 0，后面跟 Ext4Extent
    Leaf {
        header: Ext4ExtentHeader,
        entries: Vec<Ext4Extent>,
    },
    /// 内部节点：header.eh_depth > 0，后面跟 Ext4ExtentIdx
    Index {
        header: Ext4ExtentHeader,
        entries: Vec<Ext4ExtentIdx>,
    },
}

impl ExtentNode {
    pub fn header(&self) -> &Ext4ExtentHeader {
        match self {
            ExtentNode::Leaf { header, .. } => header,
            ExtentNode::Index { header, .. } => header,
        }
    }
   
    pub fn header_mut(&mut self) -> &mut Ext4ExtentHeader {
        match self {
            ExtentNode::Leaf { header, .. } => header,
            ExtentNode::Index { header, .. } => header,
        }
    }

    pub fn is_leaf(&self) -> bool {
        matches!(self, ExtentNode::Leaf { .. })
    }
}

/// 绑定到单个 inode 的 extent 树视图（不持有 BlockDev，按需传入）
pub struct ExtentTree<'a> {
    pub inode: &'a mut Ext4Inode,
}

/// 用于在递归插入时向上冒泡分裂信息
struct SplitInfo {
    ///分裂出去的右节点的起始逻辑块号 (Key)
    start_block: u32,
    ///分裂出去的右节点的物理块号 (Value)
    phy_block: u64,
}

impl<'a> ExtentTree<'a> {
    /// 构造：从给定 inode 开始操作其 extent 树
    pub fn new(inode: &'a mut Ext4Inode) -> Self {
        Self { inode }
    }

    fn add_inode_sectors_for_block(&mut self) {
        let add_sectors = (BLOCK_SIZE / 512) as u64;
        let cur = ((self.inode.l_i_blocks_high as u64) << 32) | (self.inode.i_blocks_lo as u64);
        let newv = cur.saturating_add(add_sectors);
        self.inode.i_blocks_lo = (newv & 0xFFFF_FFFF) as u32;
        self.inode.l_i_blocks_high = ((newv >> 32) & 0xFFFF) as u16;
    }

    fn sub_inode_sectors_for_block(&mut self) {
        let sub_sectors = (BLOCK_SIZE / 512) as u64;
        let cur = ((self.inode.l_i_blocks_high as u64) << 32) | (self.inode.i_blocks_lo as u64);
        let newv = cur.saturating_sub(sub_sectors);
        self.inode.i_blocks_lo = (newv & 0xFFFF_FFFF) as u32;
        self.inode.l_i_blocks_high = ((newv >> 32) & 0xFFFF) as u16;
    }

    pub fn parse_node(bytes: &[u8]) -> Option<ExtentNode> {
        Self::parse_node_from_bytes(bytes)
    }

    /// 从原始字节缓冲区解析一个 extent 节点（根或子节点）
    fn parse_node_from_bytes(bytes: &[u8]) -> Option<ExtentNode> {
        let hdr_size = Ext4ExtentHeader::disk_size();
        if bytes.len() < hdr_size {
            error!(
                "Extent node buffer too small: {} < {}",
                bytes.len(),
                hdr_size
            );
            return None;
        }

        let header = Ext4ExtentHeader::from_disk_bytes(&bytes[..hdr_size]);
        if header.eh_magic != Ext4ExtentHeader::EXT4_EXT_MAGIC {
            error!(
                "Invalid extent header magic: {:x} (expect {:x})",
                header.eh_magic,
                Ext4ExtentHeader::EXT4_EXT_MAGIC
            );
            return None;
        }

        let entries = header.eh_entries as usize;
        let max = header.eh_max as usize;
        if entries > max {
            error!(
                "Extent header entries overflow: entries={entries}, max={max}"
            );
            return None;
        }

        let mut offset = hdr_size;

        if header.eh_depth == 0 {
            // 叶子节点：解析 Ext4Extent
            let mut vec = Vec::with_capacity(entries);
            let et_size = Ext4Extent::disk_size();
            for _ in 0..entries {
                if offset + et_size > bytes.len() {
                    error!(
                        "Extent leaf truncated: need {} bytes, have {}",
                        offset + et_size,
                        bytes.len()
                    );
                    return None;
                }
                let et = Ext4Extent::from_disk_bytes(&bytes[offset..offset + et_size]);
                vec.push(et);
                offset += et_size;
            }
            vec.sort_unstable_by_key(|entries| entries.ee_block);
            Some(ExtentNode::Leaf {
                header,
                entries: vec,
            })
        } else {
            // 内部节点：解析 Ext4ExtentIdx
            let mut vec = Vec::with_capacity(entries);
            let idx_size = Ext4ExtentIdx::disk_size();
            for _ in 0..entries {
                if offset + idx_size > bytes.len() {
                    error!(
                        "Extent index truncated: need {} bytes, have {}",
                        offset + idx_size,
                        bytes.len()
                    );
                    return None;
                }
                let idx = Ext4ExtentIdx::from_disk_bytes(&bytes[offset..offset + idx_size]);
                vec.push(idx);
                offset += idx_size;
            }
            vec.sort_unstable_by_key(|entries| entries.ei_block);
            Some(ExtentNode::Index {
                header,
                entries: vec,
            })
        }
    }

    /// 从 inode.i_block 解析根节点
    pub fn load_root_from_inode(&self) -> Option<ExtentNode> {
        // inode.i_block 是 15 * u32 = 60 字节，正好容纳一个 extent 节点
        let iblocks = &self.inode.i_block; //不同端序解析为错误端序
        let mut bytes: [u8; 60] = [0; 60];
        for idx in 0..15 {
            //正确处理字节序
            let trans_b1 = iblocks[idx].to_le_bytes();
            bytes[idx * 4] = trans_b1[0];
            bytes[idx * 4 + 1] = trans_b1[1];
            bytes[idx * 4 + 2] = trans_b1[2];
            bytes[idx * 4 + 3] = trans_b1[3];
        }
        Self::parse_node_from_bytes(&bytes)
    }

    /// 将根节点写回 inode.i_block
    pub fn store_root_to_inode(&mut self, node: &ExtentNode) {
        let hdr_size = Ext4ExtentHeader::disk_size();

        match node {
            ExtentNode::Leaf { header, entries } => {
                // 仅支持 depth=0：header + 若干 Ext4Extent 写入到 i_block（60 字节）
                let mut buf = [0u8; 60];

                // 写 header
                header.to_disk_bytes(&mut buf[0..hdr_size]);

                // 写 extents
                let et_size = Ext4Extent::disk_size();
                for (i, e) in entries.iter().enumerate() {
                    let off = hdr_size + i * et_size;
                    if off + et_size > buf.len() {
                        break;
                    }
                    e.to_disk_bytes(&mut buf[off..off + et_size]);
                }

                // 将 60 字节解释为 15 个 u32 写回 i_block
                for i in 0..15 {
                    let off = i * 4;
                    let v =
                        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
                    self.inode.i_block[i] = v;
                }
            }
            ExtentNode::Index { header, entries } => {
                // depth>0：header + 若干 Ext4ExtentIdx 写入到 inode.i_block
                let mut buf = [0u8; 60];

                header.to_disk_bytes(&mut buf[0..hdr_size]);

                let idx_size = Ext4ExtentIdx::disk_size();
                for (i, idx) in entries.iter().enumerate() {
                    let off = hdr_size + i * idx_size;
                    if off + idx_size > buf.len() {
                        break;
                    }
                    idx.to_disk_bytes(&mut buf[off..off + idx_size]);
                }

                for i in 0..15 {
                    let off = i * 4;
                    let v =
                        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
                    self.inode.i_block[i] = v;
                }
            }
        }
    }

    /// 查找包含给定逻辑块的 extent（如果有）
    pub fn find_extent<B: BlockDevice>(
        &mut self,
        dev: &mut Jbd2Dev<B>,
        lblock: u32,
    ) -> BlockDevResult<Option<Ext4Extent>> {
        let root = match self.load_root_from_inode() {
            Some(node) => node,
            None => return Ok(None),
        };
        self.find_in_node(dev, &root, lblock)
    }

    /// 在给定节点下查找逻辑块对应的 extent
    fn find_in_node<B: BlockDevice>(
        &mut self,
        dev: &mut Jbd2Dev<B>,
        node: &ExtentNode,
        lblock: u32,
    ) -> BlockDevResult<Option<Ext4Extent>> {
        match node {
            ExtentNode::Leaf { entries, .. } => {
                for et in entries {
                    let start = et.ee_block; // 逻辑起始块
                    let len = et.ee_len as u32; // 覆盖长度
                    let end = start.saturating_add(len); // 半开区间 [start, end)
                    if lblock >= start && lblock < end {
                        return Ok(Some(*et));
                    }
                }
                Ok(None)
            }
            ExtentNode::Index { entries, .. } => {
                if entries.is_empty() {
                    return Ok(None);
                }

                // 在索引条目中找到最后一个 ei_block <= lblock 的条目
                let mut chosen = &entries[0];
                for idx in entries {
                    if idx.ei_block <= lblock {
                        chosen = idx;
                    } else {
                        break;
                    }
                }

                let child_block = (chosen.ei_leaf_hi as u64) << 32 | (chosen.ei_leaf_lo as u64);

                debug!(
                    "Descending into extent child block {child_block} for lblock {lblock}"
                );

                // 读取子节点所在的物理块，并从块开头解析 extent 节点
                dev.read_block(child_block as u32)?;
                let buf = dev.buffer();
                let child = match Self::parse_node_from_bytes(buf) {
                    Some(n) => n,
                    None => return Ok(None),
                };

                self.find_in_node(dev, &child, lblock)
            }
        }
    }

    pub fn remove_extend<B: BlockDevice>(
        &mut self,
        fs: &mut Ext4FileSystem,
        deleted_ext: Ext4Extent,
        block_dev: &mut Jbd2Dev<B>,
    ) -> BlockDevResult<()> {
        let del_start = deleted_ext.ee_block;
        let del_len = (deleted_ext.ee_len as u32) & 0x7FFF;
        if del_len == 0 {
            return Ok(());
        }

        // Preflight: ensure we can delete exactly del_len allocated blocks starting at del_start
        // (holes do not count toward del_len). If insufficient, return Err without side effects.
        {
            #[derive(Clone, Copy)]
            enum PreKind {
                Have,
                HoleSkip,
                NoMore,
            }

            #[derive(Clone, Copy)]
            struct PreRes {
                kind: PreKind,
                can_take: u32,
                next_lbn: u32,
            }

            fn extent_len15(e: &Ext4Extent) -> u32 {
                (e.ee_len as u32) & 0x7FFF
            }

            fn pre_leaf_step(entries: &[Ext4Extent], cur_lbn: u32) -> PreRes {
                let mut best: Option<&Ext4Extent> = None;
                for e in entries {
                    let len = extent_len15(e);
                    if len == 0 {
                        continue;
                    }
                    let start = e.ee_block;
                    let end = start.saturating_add(len);
                    if start <= cur_lbn && cur_lbn < end {
                        best = Some(e);
                        break;
                    }
                    if cur_lbn < start {
                        best = Some(e);
                        break;
                    }
                }

                let Some(e) = best else {
                    return PreRes {
                        kind: PreKind::NoMore,
                        can_take: 0,
                        next_lbn: cur_lbn,
                    };
                };

                let len15 = extent_len15(e);
                if len15 == 0 {
                    return PreRes {
                        kind: PreKind::NoMore,
                        can_take: 0,
                        next_lbn: cur_lbn,
                    };
                }
                let e_start = e.ee_block;
                let e_end = e_start.saturating_add(len15);

                if cur_lbn < e_start {
                    return PreRes {
                        kind: PreKind::HoleSkip,
                        can_take: 0,
                        next_lbn: e_start,
                    };
                }

                let within_off = cur_lbn.saturating_sub(e_start);
                let can_take = len15.saturating_sub(within_off);
                if can_take == 0 {
                    return PreRes {
                        kind: PreKind::HoleSkip,
                        can_take: 0,
                        next_lbn: e_end,
                    };
                }

                PreRes {
                    kind: PreKind::Have,
                    can_take,
                    next_lbn: cur_lbn,
                }
            }

            fn pre_step<B: BlockDevice>(
                dev: &mut Jbd2Dev<B>,
                node: &ExtentNode,
                cur_lbn: u32,
            ) -> BlockDevResult<PreRes> {
                match node {
                    ExtentNode::Leaf { entries, .. } => Ok(pre_leaf_step(entries, cur_lbn)),
                    ExtentNode::Index { entries, .. } => {
                        if entries.is_empty() {
                            return Ok(PreRes {
                                kind: PreKind::NoMore,
                                can_take: 0,
                                next_lbn: cur_lbn,
                            });
                        }

                        let mut search_lbn = cur_lbn;
                        let mut idx_pos = {
                            let pp = entries.partition_point(|idx| idx.ei_block <= search_lbn);
                            if pp == 0 { 0 } else { pp - 1 }
                        };

                        while idx_pos < entries.len() {
                            let child_phy = ((entries[idx_pos].ei_leaf_hi as u64) << 32)
                                | (entries[idx_pos].ei_leaf_lo as u64);
                            dev.read_block(child_phy as u32)?;
                            let child = ExtentTree::parse_node_from_bytes(dev.buffer())
                                .ok_or(BlockDevError::Corrupted)?;

                            let r = pre_step(dev, &child, search_lbn)?;
                            match r.kind {
                                PreKind::Have | PreKind::HoleSkip => return Ok(r),
                                PreKind::NoMore => {
                                    idx_pos += 1;
                                    if idx_pos < entries.len() {
                                        search_lbn = entries[idx_pos].ei_block;
                                        continue;
                                    }
                                    break;
                                }
                            }
                        }

                        Ok(PreRes {
                            kind: PreKind::NoMore,
                            can_take: 0,
                            next_lbn: cur_lbn,
                        })
                    }
                }
            }

            let pre_root = match self.load_root_from_inode() {
                Some(node) => node,
                None => return Err(BlockDevError::Corrupted),
            };

            let mut need = del_len;
            let mut cur = del_start;
            while need > 0 {
                let r = pre_step(block_dev, &pre_root, cur)?;
                match r.kind {
                    PreKind::Have => {
                        let take = core::cmp::min(need, r.can_take);
                        need = need.saturating_sub(take);
                        cur = cur.saturating_add(take);
                    }
                    PreKind::HoleSkip => {
                        if r.next_lbn <= cur {
                            return Err(BlockDevError::Corrupted);
                        }
                        cur = r.next_lbn;
                    }
                    PreKind::NoMore => return Err(BlockDevError::InvalidInput),
                }
            }
        }

        let mut root = match self.load_root_from_inode() {
            Some(node) => node,
            None => return Err(BlockDevError::Corrupted),
        };

        fn inline_eh_max_for_node(node: &ExtentNode) -> u16 {
            let inline_bytes = 15usize * 4;
            let hdr_size = Ext4ExtentHeader::disk_size();
            let entry_size = match node {
                ExtentNode::Leaf { .. } => Ext4Extent::disk_size(),
                ExtentNode::Index { .. } => Ext4ExtentIdx::disk_size(),
            };
            (inline_bytes.saturating_sub(hdr_size) / entry_size) as u16
        }

        fn extent_len15(e: &Ext4Extent) -> u32 {
            (e.ee_len as u32) & 0x7FFF
        }

        fn extent_start_phys(e: &Ext4Extent) -> u64 {
            ((e.ee_start_hi as u64) << 32) | (e.ee_start_lo as u64)
        }

        fn build_extent_len(orig_ee_len: u16, new_len15: u32) -> BlockDevResult<u16> {
            if new_len15 > 0x7FFF {
                return Err(BlockDevError::Corrupted);
            }
            Ok((orig_ee_len & 0x8000) | (new_len15 as u16))
        }

        #[derive(Clone, Copy)]
        enum StepKind {
            Deleted,
            HoleSkip,
            NoMoreExtent,
        }

        #[derive(Clone, Copy)]
        struct StepRes {
            kind: StepKind,
            deleted: u32,
            next_lbn: u32,
            empty: bool,
            first_key: u32,
        }

        fn first_key_of_node(node: &ExtentNode) -> u32 {
            match node {
                ExtentNode::Leaf { entries, .. } => entries.first().map(|e| e.ee_block).unwrap_or(0),
                ExtentNode::Index { entries, .. } => entries.first().map(|e| e.ei_block).unwrap_or(0),
            }
        }

        fn leaf_step<'t, B: BlockDevice>(
            tree: &mut ExtentTree<'t>,
            fs: &mut Ext4FileSystem,
            dev: &mut Jbd2Dev<B>,
            header: &mut Ext4ExtentHeader,
            entries: &mut Vec<Ext4Extent>,
            cur_lbn: u32,
            remaining: u32,
            phy_block: Option<u32>,
        ) -> BlockDevResult<StepRes> {
            if entries.is_empty() {
                return Ok(StepRes {
                    kind: StepKind::NoMoreExtent,
                    deleted: 0,
                    next_lbn: cur_lbn,
                    empty: true,
                    first_key: 0,
                });
            }

            let mut best: Option<usize> = None;
            for (i, e) in entries.iter().enumerate() {
                let len = extent_len15(e);
                if len == 0 {
                    continue;
                }
                let start = e.ee_block;
                let end = start.saturating_add(len);
                if start <= cur_lbn && cur_lbn < end {
                    best = Some(i);
                    break;
                }
                if cur_lbn < start {
                    best = Some(i);
                    break;
                }
            }

            let Some(i) = best else {
                return Ok(StepRes {
                    kind: StepKind::NoMoreExtent,
                    deleted: 0,
                    next_lbn: cur_lbn,
                    empty: entries.is_empty(),
                    first_key: entries.first().map(|e| e.ee_block).unwrap_or(0),
                });
            };

            let e = entries[i];
            let len15 = extent_len15(&e);
            if len15 == 0 {
                return Ok(StepRes {
                    kind: StepKind::NoMoreExtent,
                    deleted: 0,
                    next_lbn: cur_lbn,
                    empty: entries.is_empty(),
                    first_key: entries.first().map(|e| e.ee_block).unwrap_or(0),
                });
            }
            let e_start = e.ee_block;
            let e_end = e_start.saturating_add(len15);

            if cur_lbn < e_start {
                return Ok(StepRes {
                    kind: StepKind::HoleSkip,
                    deleted: 0,
                    next_lbn: e_start,
                    empty: entries.is_empty(),
                    first_key: entries.first().map(|e| e.ee_block).unwrap_or(0),
                });
            }

            let seg_start = cur_lbn;
            let within_off = seg_start.saturating_sub(e_start);
            let can_take = len15.saturating_sub(within_off);
            if can_take == 0 {
                return Ok(StepRes {
                    kind: StepKind::HoleSkip,
                    deleted: 0,
                    next_lbn: e_end,
                    empty: entries.is_empty(),
                    first_key: entries.first().map(|e| e.ee_block).unwrap_or(0),
                });
            }
            let cut_len = core::cmp::min(remaining, can_take);
            let seg_end = seg_start.saturating_add(cut_len);

            {
                let base = extent_start_phys(&e);
                let off = within_off as u64;
                for j in 0..(cut_len as u64) {
                    fs.free_block(dev, base + off + j)?;
                    tree.sub_inode_sectors_for_block();
                }
            }

            if seg_start == e_start && seg_end == e_end {
                entries.remove(i);
            } else if seg_start == e_start {
                let delta = seg_end.saturating_sub(e_start);
                let new_len15 = len15.saturating_sub(delta);
                let new_start_phys = extent_start_phys(&e) + delta as u64;
                let mut new_e = e;
                new_e.ee_block = seg_end;
                new_e.ee_len = build_extent_len(e.ee_len, new_len15)?;
                new_e.ee_start_lo = (new_start_phys & 0xFFFF_FFFF) as u32;
                new_e.ee_start_hi = (new_start_phys >> 32) as u16;
                entries[i] = new_e;
            } else if seg_end == e_end {
                let new_len15 = seg_start.saturating_sub(e_start);
                let mut new_e = e;
                new_e.ee_len = build_extent_len(e.ee_len, new_len15)?;
                entries[i] = new_e;
            } else {
                let left_len15 = seg_start.saturating_sub(e_start);
                let right_len15 = e_end.saturating_sub(seg_end);

                let mut left_e = e;
                left_e.ee_len = build_extent_len(e.ee_len, left_len15)?;

                let right_start_phys = extent_start_phys(&e) + seg_end.saturating_sub(e_start) as u64;
                let mut right_e = e;
                right_e.ee_block = seg_end;
                right_e.ee_len = build_extent_len(e.ee_len, right_len15)?;
                right_e.ee_start_lo = (right_start_phys & 0xFFFF_FFFF) as u32;
                right_e.ee_start_hi = (right_start_phys >> 32) as u16;

                entries[i] = left_e;
                entries.insert(i + 1, right_e);
            }

            entries.sort_unstable_by_key(|e| e.ee_block);
            header.eh_entries = entries.len() as u16;

            if let Some(block_id) = phy_block {
                let disk_node = ExtentNode::Leaf {
                    header: *header,
                    entries: entries.clone(),
                };
                ExtentTree::write_node_to_block(dev, block_id, &disk_node, header.eh_max)?;
            }

            Ok(StepRes {
                kind: StepKind::Deleted,
                deleted: cut_len,
                next_lbn: seg_end,
                empty: entries.is_empty(),
                first_key: entries.first().map(|e| e.ee_block).unwrap_or(0),
            })
        }

        fn step_recursive<'t, B: BlockDevice>(
            tree: &mut ExtentTree<'t>,
            fs: &mut Ext4FileSystem,
            dev: &mut Jbd2Dev<B>,
            node: &mut ExtentNode,
            cur_lbn: u32,
            remaining: u32,
            phy_block: Option<u32>,
        ) -> BlockDevResult<StepRes> {
            match node {
                ExtentNode::Leaf { header, entries } =>
                    leaf_step(tree, fs, dev, header, entries, cur_lbn, remaining, phy_block),
                ExtentNode::Index { header, entries } => {
                    if entries.is_empty() {
                        return Ok(StepRes {
                            kind: StepKind::NoMoreExtent,
                            deleted: 0,
                            next_lbn: cur_lbn,
                            empty: true,
                            first_key: 0,
                        });
                    }

                    let mut search_lbn = cur_lbn;
                    let mut idx_pos = {
                        let pp = entries.partition_point(|idx| idx.ei_block <= search_lbn);
                        if pp == 0 { 0 } else { pp - 1 }
                    };

                    while idx_pos < entries.len() {
                        let child_phy = ((entries[idx_pos].ei_leaf_hi as u64) << 32)
                            | (entries[idx_pos].ei_leaf_lo as u64);
                        dev.read_block(child_phy as u32)?;
                        let child_bytes = dev.buffer();
                        let mut child_node =
                            ExtentTree::parse_node_from_bytes(child_bytes).ok_or(BlockDevError::Corrupted)?;

                        let child_res = step_recursive(
                            tree,
                            fs,
                            dev,
                            &mut child_node,
                            search_lbn,
                            remaining,
                            Some(child_phy as u32),
                        )?;

                        match child_res.kind {
                            StepKind::Deleted => {
                                if child_res.empty {
                                    entries.remove(idx_pos);
                                    header.eh_entries = entries.len() as u16;
                                    fs.free_block(dev, child_phy)?;
                                    tree.sub_inode_sectors_for_block();
                                } else {
                                    entries[idx_pos].ei_block = child_res.first_key;
                                }

                                entries.sort_unstable_by_key(|e| e.ei_block);
                                header.eh_entries = entries.len() as u16;

                                if let Some(block_id) = phy_block {
                                    let disk_node = ExtentNode::Index {
                                        header: *header,
                                        entries: entries.clone(),
                                    };
                                    ExtentTree::write_node_to_block(dev, block_id, &disk_node, header.eh_max)?;
                                }

                                return Ok(StepRes {
                                    kind: StepKind::Deleted,
                                    deleted: child_res.deleted,
                                    next_lbn: child_res.next_lbn,
                                    empty: entries.is_empty(),
                                    first_key: entries.first().map(|e| e.ei_block).unwrap_or(0),
                                });
                            }
                            StepKind::HoleSkip => {
                                return Ok(StepRes {
                                    kind: StepKind::HoleSkip,
                                    deleted: 0,
                                    next_lbn: child_res.next_lbn,
                                    empty: false,
                                    first_key: first_key_of_node(node),
                                });
                            }
                            StepKind::NoMoreExtent => {
                                idx_pos += 1;
                                if idx_pos < entries.len() {
                                    search_lbn = entries[idx_pos].ei_block;
                                    continue;
                                }
                                break;
                            }
                        }
                    }

                    Ok(StepRes {
                        kind: StepKind::NoMoreExtent,
                        deleted: 0,
                        next_lbn: search_lbn,
                        empty: false,
                        first_key: first_key_of_node(node),
                    })
                }
            }
        }

        let mut remaining = del_len;
        let mut cur_lbn = del_start;
        let mut changed = false;
        while remaining > 0 {
            let res = step_recursive(self, fs, block_dev, &mut root, cur_lbn, remaining, None)?;
            match res.kind {
                StepKind::Deleted => {
                    if res.deleted == 0 {
                        return Err(BlockDevError::Corrupted);
                    }
                    remaining = remaining.saturating_sub(res.deleted);
                    cur_lbn = res.next_lbn;
                    changed = true;
                }
                StepKind::HoleSkip => {
                    if res.next_lbn <= cur_lbn {
                        return Err(BlockDevError::Corrupted);
                    }
                    cur_lbn = res.next_lbn;
                }
                StepKind::NoMoreExtent => {
                    return Err(BlockDevError::InvalidInput);
                }
            }
        }

        if !changed {
            return Err(BlockDevError::InvalidInput);
        }

        let en_max = inline_eh_max_for_node(&root);
        match &mut root {
            ExtentNode::Leaf { header, entries } => {
                header.eh_entries = entries.len() as u16;
                header.eh_max = en_max;
                self.store_root_to_inode(&root);
                Ok(())
            }
            ExtentNode::Index { header, entries } => {
                if entries.is_empty() {
                    let mut hdr = Ext4ExtentHeader::new();
                    hdr.eh_magic = Ext4ExtentHeader::EXT4_EXT_MAGIC;
                    hdr.eh_depth = 0;
                    hdr.eh_entries = 0;
                    hdr.eh_max = (15usize * 4usize
                        .saturating_sub(Ext4ExtentHeader::disk_size())
                        / Ext4Extent::disk_size()) as u16;
                    let empty_root = ExtentNode::Leaf {
                        header: hdr,
                        entries: Vec::new(),
                    };
                    self.store_root_to_inode(&empty_root);
                    return Ok(());
                }

                if entries.len() == 1 {
                    let child_phy = ((entries[0].ei_leaf_hi as u64) << 32) | (entries[0].ei_leaf_lo as u64);
                    block_dev.read_block(child_phy as u32)?;
                    let child_bytes = block_dev.buffer();
                    let mut child_node =
                        ExtentTree::parse_node_from_bytes(child_bytes).ok_or(BlockDevError::Corrupted)?;

                    let inline_max = inline_eh_max_for_node(&child_node) as usize;
                    let child_entries_len = match &child_node {
                        ExtentNode::Leaf { entries, .. } => entries.len(),
                        ExtentNode::Index { entries, .. } => entries.len(),
                    };

                    if child_entries_len <= inline_max {
                        *child_node.header_mut() = {
                            let mut h = *child_node.header();
                            h.eh_max = inline_eh_max_for_node(&child_node);
                            h
                        };

                        self.store_root_to_inode(&child_node);

                        fs.free_block(block_dev, child_phy)?;
                        self.sub_inode_sectors_for_block();
                        return Ok(());
                    }
                }

                header.eh_entries = entries.len() as u16;
                header.eh_max = en_max;
                self.store_root_to_inode(&root);
                Ok(())
            }
        }
    }

    

    /// 插入新的 Extent 入口函数
    pub fn insert_extent<B: BlockDevice>(
        &mut self,
        fs: &mut Ext4FileSystem,
        new_ext: Ext4Extent,
        block_dev: &mut Jbd2Dev<B>,
    ) -> BlockDevResult<()> {
        debug!(
            "ExtentTree::insert_extent: new_ext lbn={} len={} phys_start={}",
            new_ext.ee_block,
            new_ext.ee_len & 0x7FFF,
            new_ext.start_block()
        );

        let mut root = match self.load_root_from_inode() {
            Some(node) => node,
            None => return Err(BlockDevError::Unsupported),
        };

        match &root {
            ExtentNode::Leaf { header, entries } => {
                debug!(
                    "ExtentTree::insert_extent: current root=LEAF depth={} entries={} max={} first_extents={:?}",
                    header.eh_depth,
                    header.eh_entries,
                    header.eh_max,
                    entries
                        .iter()
                        .take(4)
                        .map(|e| (e.ee_block, e.ee_len & 0x7FFF, e.start_block()))
                        .collect::<Vec<_>>()
                );
            }
            ExtentNode::Index { header, entries } => {
                debug!(
                    "ExtentTree::insert_extent: current root=INDEX depth={} entries={} max={} first_indexes={:?}",
                    header.eh_depth,
                    header.eh_entries,
                    header.eh_max,
                    entries
                        .iter()
                        .take(4)
                        .map(|ix| (
                            ix.ei_block,
                            ((ix.ei_leaf_hi as u64) << 32) | ix.ei_leaf_lo as u64
                        ))
                        .collect::<Vec<_>>()
                );
            }
        }

        // 尝试递归插入
        let split_result = self.insert_recursive(fs, block_dev, &mut root, new_ext, None)?;

        match split_result {
            None => {
                // 没有发生根节点分裂，只需将更新后的根节点写回 Inode
                debug!(
                    "ExtentTree::insert_extent: no root split, writing updated root back to inode"
                );
                self.store_root_to_inode(&root);
                Ok(())
            }
            Some(split_info) => {
                // 根节点分裂了，需要增加树的深度

                // 分配一个新的块，将“左半部分”（即原本在 Root 里的数据）移到这个新块中
                let new_left_block = fs.alloc_block(block_dev)?;
                self.add_inode_sectors_for_block();
                debug!(
                    "ExtentTree::insert_extent: root split occurred, new_left_block={} split_info={{start_block={}, phy_block={}}}",
                    new_left_block, split_info.start_block, split_info.phy_block
                );

                // 计算普通块的 eh_max (通常 340)
                let block_eh_max = Self::calc_block_eh_max();

                // 将当前的 root (左半部分) 写入新分配的物理块
                // 注意：写入磁盘时要更新 eh_max，因为从 inode (max~4) 移到了 block (max~340)
                Self::write_node_to_block(block_dev, new_left_block as u32, &root, block_eh_max)?;

                // 在 Inode 中构建新的 Root Index
                let inline_bytes = self.inode.i_block.len() * 4;
                let hdr_size = Ext4ExtentHeader::disk_size();
                let idx_size = Ext4ExtentIdx::disk_size();
                let root_eh_max = (inline_bytes.saturating_sub(hdr_size) / idx_size) as u16;

                let mut new_root_header = Ext4ExtentHeader::new();
                new_root_header.eh_magic = Ext4ExtentHeader::EXT4_EXT_MAGIC;
                // 新的深度 = 旧深度 + 1
                new_root_header.eh_depth = root.header().eh_depth + 1;
                new_root_header.eh_entries = 2;
                new_root_header.eh_max = root_eh_max;

                // 左子节点索引
                let left_idx = Ext4ExtentIdx {
                    ei_block: Self::get_node_start_block(&root), // 获取左节点的起始逻辑块
                    ei_leaf_lo: (new_left_block & 0xFFFF_FFFF) as u32,
                    ei_leaf_hi: ((new_left_block >> 32) & 0xFFFF) as u16,
                    ei_unused: 0,
                };

                // 右子节点索引 (来自 SplitInfo)
                let right_idx = Ext4ExtentIdx {
                    ei_block: split_info.start_block,
                    ei_leaf_lo: (split_info.phy_block & 0xFFFF_FFFF) as u32,
                    ei_leaf_hi: ((split_info.phy_block >> 32) & 0xFFFF) as u16,
                    ei_unused: 0,
                };

                let new_root_node = ExtentNode::Index {
                    header: new_root_header,
                    entries: vec![left_idx, right_idx],
                };

                // 写回 Inode
                self.store_root_to_inode(&new_root_node);
                Ok(())
            }
        }
    }

    /// 递归插入函数
    /// - `node`: 当前内存中的节点数据（按引用传入，以便原地修改 Root）
    /// - `new_ext`: 要插入的 extent
    /// - `phy_block`: 当前节点所在的物理块号。如果是 Root 则为 None。
    fn insert_recursive<B: BlockDevice>(
        &mut self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        node: &mut ExtentNode,
        new_ext: Ext4Extent,
        phy_block: Option<u32>,
    ) -> BlockDevResult<Option<SplitInfo>> {
        match node {
            ExtentNode::Leaf { header, entries } => {
                debug!(
                    "insert_recursive: LEAF depth={} entries_before={} max={} new_ext=(lbn={}, len={}, phys_start={}) phy_block={:?}",
                    header.eh_depth,
                    header.eh_entries,
                    header.eh_max,
                    new_ext.ee_block,
                    new_ext.ee_len & 0x7FFF,
                    new_ext.start_block(),
                    phy_block
                );
                let pos = entries
                    .binary_search_by_key(&new_ext.ee_block, |e| e.ee_block)
                    .unwrap_or_else(|i| i);

                const MAX_LEN: u32 = 32768;

                if pos > 0 {
                    let prev = &mut entries[pos - 1];

                    let prev_logical = prev.ee_block;
                    let prev_len = prev.ee_len as u32 & 0x7FFF;
                    let new_logical = new_ext.ee_block;
                    let new_len = new_ext.ee_len as u32 & 0x7FFF;

                    if prev_len != 0 && new_len != 0 {
                        let prev_end = prev_logical.saturating_add(prev_len);

                        if new_logical == prev_end {
                            let prev_phys_start =
                                ((prev.ee_start_hi as u64) << 32) | prev.ee_start_lo as u64;
                            let new_phys_start =
                                ((new_ext.ee_start_hi as u64) << 32) | new_ext.ee_start_lo as u64;

                            if new_phys_start == prev_phys_start + prev_len as u64 {
                                let total = prev_len + new_len;
                                let hi_flag = prev.ee_len & 0x8000; // 保留原高位标志

                                if total <= MAX_LEN {
                                    prev.ee_len = (total as u16 & 0x7FFF) | hi_flag;
                                    debug!(
                                        "insert_recursive: merged with previous extent -> new_len={total} (no split yet)"
                                    );

                                    if entries.len() <= header.eh_max as usize {
                                        if let Some(block_id) = phy_block {
                                            // 为当前叶子节点构造一个临时 ExtentNode 写回磁盘
                                            let disk_node = ExtentNode::Leaf {
                                                header: *header,
                                                entries: entries.clone(),
                                            };
                                            Self::write_node_to_block(
                                                block_dev,
                                                block_id,
                                                &disk_node,
                                                header.eh_max,
                                            )?;
                                        }
                                        return Ok(None);
                                    }
                                } else {
                                    prev.ee_len = (MAX_LEN as u16 & 0x7FFF) | hi_flag;

                                    let remain = total - MAX_LEN;
                                    if remain > 0 {
                                        let tail_logical = prev_logical + MAX_LEN;
                                        let tail_phys = prev_phys_start + MAX_LEN as u64;

                                        let tail = Ext4Extent {
                                            ee_block: tail_logical,
                                            ee_len: (remain as u16 & 0x7FFF)
                                                | (new_ext.ee_len & 0x8000),
                                            ee_start_hi: (tail_phys >> 32) as u16,
                                            ee_start_lo: (tail_phys & 0xFFFF_FFFF) as u32,
                                        };

                                        let insert_pos = pos; // 在 pos 处插入新 extent
                                        entries.insert(insert_pos, tail);
                                        header.eh_entries = entries.len() as u16;
                                        debug!(
                                            "insert_recursive: previous extent saturated MAX_LEN, inserted tail extent (lbn={}, len={}, phys_start={}) now entries_len={}",
                                            tail.ee_block,
                                            tail.ee_len & 0x7FFF,
                                            tail.start_block(),
                                            header.eh_entries
                                        );

                                        if entries.len() <= header.eh_max as usize {
                                            if let Some(block_id) = phy_block {
                                                let disk_node = ExtentNode::Leaf {
                                                    header: *header,
                                                    entries: entries.clone(),
                                                };
                                                Self::write_node_to_block(
                                                    block_dev,
                                                    block_id,
                                                    &disk_node,
                                                    header.eh_max,
                                                )?;
                                            }
                                            return Ok(None);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                entries.insert(pos, new_ext);
                header.eh_entries = entries.len() as u16;
                debug!(
                    "insert_recursive: after insert (no split yet) leaf entries_len={} (max={}) first_extents={:?}",
                    header.eh_entries,
                    header.eh_max,
                    entries
                        .iter()
                        .take(4)
                        .map(|e| (e.ee_block, e.ee_len & 0x7FFF, e.start_block()))
                        .collect::<Vec<_>>()
                );

                //检查是否需要分裂
                if entries.len() <= header.eh_max as usize {
                    // 不需要分裂，如果不是 Root (phy_block有值)，则写回磁盘
                    if let Some(block_id) = phy_block {
                        let disk_node = ExtentNode::Leaf {
                            header: *header,
                            entries: entries.clone(),
                        };
                        Self::write_node_to_block(block_dev, block_id, &disk_node, header.eh_max)?;
                    }
                    // Root 节点由调用方负责写回 Inode，这里返回 None
                    return Ok(None);
                }

                // 叶子节点分裂逻辑
                debug!(
                    "Leaf node overflow ({} > {}), splitting...",
                    entries.len(),
                    header.eh_max
                );
                // 分裂点：中间
                let split_idx = entries.len() / 2;
                let right_entries = entries.split_off(split_idx);
                // 当前 node 保留左半部分，header entries 数量更新
                header.eh_entries = entries.len() as u16;

                // 分配新块用于存储右半部分
                let new_phy_block = fs.alloc_block(block_dev)?;
                self.add_inode_sectors_for_block();
                debug!(
                    "insert_recursive: allocated new block for right leaf node: {new_phy_block}"
                );

                // 构造右节点
                let right_header = Ext4ExtentHeader {
                    eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                    eh_entries: right_entries.len() as u16,
                    eh_max: Self::calc_block_eh_max(), // 新块一定是在磁盘上的，使用标准容量
                    eh_depth: 0,                       // 依然是 Leaf
                    eh_generation: 0,
                };
                let right_node = ExtentNode::Leaf {
                    header: right_header,
                    entries: right_entries,
                };

                //写回数据
                // 写右节点（新块）
                Self::write_node_to_block(
                    block_dev,
                    new_phy_block as u32,
                    &right_node,
                    right_header.eh_max,
                )?;
                // 写左节点（当前节点）
                // 如果当前节点是普通块，写回磁盘；如果是 Root，调用方会处理，但这里我们要在内存中保持正确状态
                if let Some(block_id) = phy_block {
                    let disk_node = ExtentNode::Leaf {
                        header: *header,
                        entries: entries.clone(),
                    };
                    Self::write_node_to_block(block_dev, block_id, &disk_node, header.eh_max)?;
                }

                //返回分裂信息
                // Key 是右节点的第一个 extent 的逻辑块号
                let split_key = match &right_node {
                    ExtentNode::Leaf { entries, .. } => entries[0].ee_block,
                    _ => unreachable!(),
                };

                Ok(Some(SplitInfo {
                    start_block: split_key,
                    phy_block: new_phy_block,
                }))
            }

            ExtentNode::Index { header, entries } => {
                debug!(
                    "insert_recursive: INDEX depth={} entries_before={} max={} new_ext=(lbn={}, len={}, phys_start={}) phy_block={:?}",
                    header.eh_depth,
                    header.eh_entries,
                    header.eh_max,
                    new_ext.ee_block,
                    new_ext.ee_len & 0x7FFF,
                    new_ext.start_block(),
                    phy_block
                );
                // 查找子节点
                // 找到最后一个 ei_block <= new_ext.ee_block 的索引
                // 如果 entries 为空（理论不应发生），则直接插入
                let idx_pos = if entries.is_empty() {
                    0 // 如果为空，则直接插入
                } else {
                    // 使用 partition_point 找到第一个 > target 的位置，再减 1
                    let pp = entries.partition_point(|idx| idx.ei_block <= new_ext.ee_block);
                    if pp == 0 { 0 } else { pp - 1 }
                };

                // 读取子节点
                let child_phy_block = ((entries[idx_pos].ei_leaf_hi as u64) << 32)
                    | (entries[idx_pos].ei_leaf_lo as u64);
                // 读取子节点
                block_dev.read_block(child_phy_block as u32)?;
                let child_bytes = block_dev.buffer();
                let mut child_node =
                    Self::parse_node_from_bytes(child_bytes).expect("Can't parse node from bytes!");

                //  递归调用
                let child_split_res = self.insert_recursive(
                    fs,
                    block_dev,
                    &mut child_node,
                    new_ext,
                    Some(child_phy_block as u32),
                )?;

                //  处理子节点返回的结果
                if let Some(split_info) = child_split_res {
                    // 子节点分裂了，需要将 split_info 插入到当前的 Index 节点
                    debug!("Child split bubbled up, inserting index to current node.");
                    // 插入索引并保持有序
                    let new_idx = Ext4ExtentIdx {
                        ei_block: split_info.start_block,
                        ei_leaf_lo: (split_info.phy_block & 0xFFFF_FFFF) as u32,
                        ei_leaf_hi: ((split_info.phy_block >> 32) & 0xFFFF) as u16,
                        ei_unused: 0,
                    };

                    let insert_pos = entries
                        .binary_search_by_key(&new_idx.ei_block, |e| e.ei_block)
                        .unwrap_or_else(|i| i);
                    entries.insert(insert_pos, new_idx);
                    header.eh_entries = entries.len() as u16;

                    // 检查当前 Index 节点是否需要分裂
                    if entries.len() <= header.eh_max as usize {
                        // 不需要分裂，写回
                        if let Some(block_id) = phy_block {
                            let disk_node = ExtentNode::Index {
                                header: *header,
                                entries: entries.clone(),
                            };
                            Self::write_node_to_block(
                                block_dev,
                                block_id,
                                &disk_node,
                                header.eh_max,
                            )?;
                        }
                        return Ok(None);
                    }

                    //Index 节点分裂逻辑
                    debug!("Index node overflow, splitting...");
                    // 分裂点：中间
                    let split_idx = entries.len() / 2;
                    let right_entries = entries.split_off(split_idx);
                    header.eh_entries = entries.len() as u16;
                    debug!(
                        "insert_recursive: index split at idx={} -> left_entries={} right_entries={}",
                        split_idx,
                        header.eh_entries,
                        right_entries.len()
                    );

                    // 分配新块
                    let new_phy_block = fs.alloc_block(block_dev)?;
                    self.add_inode_sectors_for_block();
                    debug!(
                        "insert_recursive: allocated new block for right index node: {new_phy_block}"
                    );

                    let right_header = Ext4ExtentHeader {
                        eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                        eh_entries: right_entries.len() as u16,
                        eh_max: Self::calc_block_eh_max(),
                        eh_depth: header.eh_depth, // 保持相同的 depth
                        eh_generation: 0,
                    };

                    let right_node = ExtentNode::Index {
                        header: right_header,
                        entries: right_entries,
                    };

                    // 写回
                    Self::write_node_to_block(
                        block_dev,
                        new_phy_block as u32,
                        &right_node,
                        right_header.eh_max,
                    )?;
                    if let Some(block_id) = phy_block {
                        let disk_node = ExtentNode::Index {
                            header: *header,
                            entries: entries.clone(),
                        };
                        Self::write_node_to_block(block_dev, block_id, &disk_node, header.eh_max)?;
                    }

                    // 返回分裂信息
                    // 索引节点的 Key 也是它覆盖范围的起始逻辑块号
                    let split_key = match &right_node {
                        ExtentNode::Index { entries, .. } => entries[0].ei_block,
                        _ => unreachable!(),
                    };

                    Ok(Some(SplitInfo {
                        start_block: split_key,
                        phy_block: new_phy_block,
                    }))
                } else {
                    // 子节点没分裂，那就没事了
                    Ok(None)
                }
            }
        }
    }

    /// 通用的写节点到物理块函数
    fn write_node_to_block<B: BlockDevice>(
        dev: &mut Jbd2Dev<B>,
        block_id: u32,
        node: &ExtentNode,
        eh_max: u16,
    ) -> BlockDevResult<()> {
        let hdr_size = Ext4ExtentHeader::disk_size();
        // 读取块
        dev.read_block(block_id)?;
        let buf = dev.buffer_mut();

        match node {
            ExtentNode::Leaf { header, entries } => {
                let et_size = Ext4Extent::disk_size();
                // 确保 header 中的 max 正确（因为内存中的 node 可能来自 root，max 很小）
                let mut disk_header = *header;
                disk_header.eh_max = eh_max;
                // 写 header
                disk_header.to_disk_bytes(&mut buf[0..hdr_size]);
                // 写 extents
                for (i, e) in entries.iter().enumerate() {
                    let off = hdr_size + i * et_size;
                    if off + et_size > buf.len() {
                        break;
                    }
                    e.to_disk_bytes(&mut buf[off..off + et_size]);
                }
            }
            ExtentNode::Index { header, entries } => {
                let idx_size = Ext4ExtentIdx::disk_size();
                let mut disk_header = *header;
                disk_header.eh_max = eh_max;

                // 写 header
                disk_header.to_disk_bytes(&mut buf[0..hdr_size]);
                // 写索引
                for (i, idx) in entries.iter().enumerate() {
                    let off = hdr_size + i * idx_size;
                    if off + idx_size > buf.len() {
                        break;
                    }
                    idx.to_disk_bytes(&mut buf[off..off + idx_size]);
                }
            }
        }
        // 标记脏并写回
        dev.write_block(block_id, true)?;
        Ok(())
    }

    /// 计算标准数据块能容纳的条目数
    fn calc_block_eh_max() -> u16 {
        let hdr_size = Ext4ExtentHeader::disk_size();
        let entry_size = Ext4Extent::disk_size(); // Index 和 Extent 大小一样，都是 12
        (BLOCK_SIZE.saturating_sub(hdr_size) / entry_size) as u16
    }

    /// 辅助：获取节点的起始逻辑块号
    fn get_node_start_block(node: &ExtentNode) -> u32 {
        match node {
            ExtentNode::Leaf { entries, .. } => {
                if entries.is_empty() {
                    0
                } else {
                    entries[0].ee_block
                }
            }
            ExtentNode::Index { entries, .. } => {
                if entries.is_empty() {
                    0
                } else {
                    entries[0].ei_block
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::ext4_backend::blockdev::{BlockDevice, Jbd2Dev};
    use crate::ext4_backend::bitmap_cache::CacheKey;
    use crate::ext4_backend::ext4::{mkfs, mount};
    use crate::ext4_backend::error::{BlockDevError, BlockDevResult};
    use alloc::vec;
    use alloc::vec::Vec;

    struct MemBlockDev {
        data: Vec<u8>,
        total_blocks: u64,
    }

    impl MemBlockDev {
        fn new(total_blocks: u64) -> Self {
            let size = total_blocks as usize * BLOCK_SIZE;
            Self {
                data: vec![0u8; size],
                total_blocks,
            }
        }
    }

    impl BlockDevice for MemBlockDev {
        fn write(&mut self, buffer: &[u8], block_id: u32, count: u32) -> BlockDevResult<()> {
            let block_size = BLOCK_SIZE;
            let required = block_size * count as usize;
            if buffer.len() < required {
                return Err(BlockDevError::BufferTooSmall {
                    provided: buffer.len(),
                    required,
                });
            }
            let start = block_id as usize * block_size;
            let end = start + required;
            if end > self.data.len() {
                return Err(BlockDevError::BlockOutOfRange {
                    block_id,
                    max_blocks: self.total_blocks,
                });
            }
            self.data[start..end].copy_from_slice(&buffer[..required]);
            Ok(())
        }

        fn read(&mut self, buffer: &mut [u8], block_id: u32, count: u32) -> BlockDevResult<()> {
            let block_size = BLOCK_SIZE;
            let required = block_size * count as usize;
            if buffer.len() < required {
                return Err(BlockDevError::BufferTooSmall {
                    provided: buffer.len(),
                    required,
                });
            }
            let start = block_id as usize * block_size;
            let end = start + required;
            if end > self.data.len() {
                return Err(BlockDevError::BlockOutOfRange {
                    block_id,
                    max_blocks: self.total_blocks,
                });
            }
            buffer[..required].copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn open(&mut self) -> BlockDevResult<()> {
            Ok(())
        }

        fn close(&mut self) -> BlockDevResult<()> {
            Ok(())
        }

        fn total_blocks(&self) -> u64 {
            self.total_blocks
        }

        fn block_size(&self) -> u32 {
            BLOCK_SIZE as u32
        }
    }

    fn setup_fs(total_blocks: u64) -> (Jbd2Dev<MemBlockDev>, Ext4FileSystem) {
        let dev = MemBlockDev::new(total_blocks);
        let mut jbd = Jbd2Dev::initial_jbd2dev(0, dev, false);
        mkfs(&mut jbd).unwrap();
        let fs = mount(&mut jbd).unwrap();
        (jbd, fs)
    }

    fn new_extent_inode() -> Ext4Inode {
        let mut inode = Ext4Inode::default();
        inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
        inode.write_extend_header();
        inode
    }

    fn alloc_data_block<B: BlockDevice>(fs: &mut Ext4FileSystem, dev: &mut Jbd2Dev<B>) -> u64 {
        fs.alloc_block(dev).unwrap()
    }

    fn bitmap_block_is_allocated<B: BlockDevice>(
        fs: &mut Ext4FileSystem,
        dev: &mut Jbd2Dev<B>,
        global_block: u64,
    ) -> bool {
        let (group_idx, block_in_group) = fs.block_allocator.global_to_group(global_block);
        let desc = fs
            .group_descs
            .get(group_idx as usize)
            .expect("invalid group_idx");
        let bitmap_block = desc.block_bitmap();
        let key = CacheKey::new_block(group_idx);

        let bm = fs
            .bitmap_cache
            .get_or_load(dev, key, bitmap_block as u64)
            .expect("load block bitmap failed");

        let idx = block_in_group as usize;
        let byte = bm.data[idx / 8];
        ((byte >> (idx % 8)) & 1) == 1
    }

    fn insert_n_extents_with_phys_gaps<B: BlockDevice>(
        fs: &mut Ext4FileSystem,
        dev: &mut Jbd2Dev<B>,
        inode: &mut Ext4Inode,
        n: u32,
    ) -> std::vec::Vec<Ext4Extent> {
        let mut tree = ExtentTree::new(inode);
        let mut out = std::vec::Vec::new();
        for lbn in 0..n {
            let phys = alloc_data_block(fs, dev);
            let _gap = alloc_data_block(fs, dev);
            let ext = Ext4Extent::new(lbn, phys, 1);
            tree.insert_extent(fs, ext, dev).unwrap();
            out.push(ext);
        }
        out
    }

    fn alloc_contiguous<B: BlockDevice>(
        fs: &mut Ext4FileSystem,
        dev: &mut Jbd2Dev<B>,
        count: u32,
    ) -> u64 {
        assert!(count > 0);
        let first = alloc_data_block(fs, dev);
        let mut prev = first;
        for _ in 1..count {
            let b = alloc_data_block(fs, dev);
            assert_eq!(b, prev + 1);
            prev = b;
        }
        first
    }

    fn collect_extents_from_inode<B: BlockDevice>(
        inode: &mut Ext4Inode,
        dev: &mut Jbd2Dev<B>,
    ) -> std::vec::Vec<Ext4Extent> {
        fn walk<B: BlockDevice>(
            dev: &mut Jbd2Dev<B>,
            node: &ExtentNode,
            out: &mut std::vec::Vec<Ext4Extent>,
        ) {
            match node {
                ExtentNode::Leaf { entries, .. } => out.extend_from_slice(entries),
                ExtentNode::Index { entries, .. } => {
                    for idx in entries {
                        let child_phy = ((idx.ei_leaf_hi as u64) << 32) | (idx.ei_leaf_lo as u64);
                        dev.read_block(child_phy as u32).unwrap();
                        let child =
                            ExtentTree::parse_node_from_bytes(dev.buffer()).expect("parse child");
                        walk(dev, &child, out);
                    }
                }
            }
        }

        let tree = ExtentTree::new(inode);
        let root = tree.load_root_from_inode().unwrap();
        let mut out = std::vec::Vec::new();
        walk(dev, &root, &mut out);
        out.sort_unstable_by_key(|e| e.ee_block);
        out
    }

    #[test]
    fn remove_extend_root_leaf_no_degeneration() {
        let (mut dev, mut fs) = setup_fs(16 * 1024);
        let mut inode = new_extent_inode();

        let exts = insert_n_extents_with_phys_gaps(&mut fs, &mut dev, &mut inode, 2);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, exts[0], &mut dev).unwrap();
        }

        let tree = ExtentTree::new(&mut inode);
        let root = tree.load_root_from_inode().unwrap();
        match root {
            ExtentNode::Leaf { header, entries } => {
                assert_eq!(header.eh_depth, 0);
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].ee_block, 1);
            }
            _ => panic!("expected leaf root"),
        }
    }

    #[test]
    fn remove_extend_root_leaf_degeneration_to_empty() {
        let (mut dev, mut fs) = setup_fs(16 * 1024);
        let mut inode = new_extent_inode();

        let exts = insert_n_extents_with_phys_gaps(&mut fs, &mut dev, &mut inode, 1);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, exts[0], &mut dev).unwrap();
        }

        let tree = ExtentTree::new(&mut inode);
        let root = tree.load_root_from_inode().unwrap();
        match root {
            ExtentNode::Leaf { header, entries } => {
                assert_eq!(header.eh_depth, 0);
                assert_eq!(entries.len(), 0);
            }
            _ => panic!("expected leaf root"),
        }
    }

    #[test]
    fn remove_extend_multilevel_to_root_promotion() {
        let (mut dev, mut fs) = setup_fs(32 * 1024);
        let mut inode = new_extent_inode();

        let exts = insert_n_extents_with_phys_gaps(&mut fs, &mut dev, &mut inode, 5);

        {
            let tree = ExtentTree::new(&mut inode);
            let root = tree.load_root_from_inode().unwrap();
            match root {
                ExtentNode::Index { header, .. } => assert!(header.eh_depth > 0),
                _ => panic!("expected index root after split"),
            }
        }

        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, exts[2], &mut dev).unwrap();
            tree.remove_extend(&mut fs, exts[3], &mut dev).unwrap();
            tree.remove_extend(&mut fs, exts[4], &mut dev).unwrap();
        }

        let tree = ExtentTree::new(&mut inode);
        let root = tree.load_root_from_inode().unwrap();
        match root {
            ExtentNode::Leaf { header, entries } => {
                assert_eq!(header.eh_depth, 0);
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].ee_block, 0);
                assert_eq!(entries[1].ee_block, 1);
            }
            _ => panic!("expected leaf root after promotion"),
        }
    }

    #[test]
    fn remove_extend_repeated_deletions_consistency() {
        let (mut dev, mut fs) = setup_fs(32 * 1024);
        let mut inode = new_extent_inode();
        let exts = insert_n_extents_with_phys_gaps(&mut fs, &mut dev, &mut inode, 5);

        for ext in exts {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, ext, &mut dev).unwrap();

            let tree2 = ExtentTree::new(&mut inode);
            assert!(tree2.load_root_from_inode().is_some());
        }

        let tree = ExtentTree::new(&mut inode);
        let root = tree.load_root_from_inode().unwrap();
        match root {
            ExtentNode::Leaf { header, entries } => {
                assert_eq!(header.eh_depth, 0);
                assert_eq!(entries.len(), 0);
            }
            _ => panic!("expected empty leaf root"),
        }
    }

    #[test]
    fn remove_extend_frees_block_bitmap_bit() {
        let (mut dev, mut fs) = setup_fs(16 * 1024);
        let mut inode = new_extent_inode();

        let phys = alloc_data_block(&mut fs, &mut dev);
        assert!(bitmap_block_is_allocated(&mut fs, &mut dev, phys));

        let ext = Ext4Extent::new(0, phys, 1);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.insert_extent(&mut fs, ext, &mut dev).unwrap();
        }

        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, ext, &mut dev).unwrap();
        }

        assert!(!bitmap_block_is_allocated(&mut fs, &mut dev, phys));
    }

    #[test]
    fn remove_extend_partial_delete_splits_extent_and_updates_bitmap() {
        let (mut dev, mut fs) = setup_fs(32 * 1024);
        let mut inode = new_extent_inode();

        let base = alloc_contiguous(&mut fs, &mut dev, 4);
        let ext = Ext4Extent::new(0, base, 4);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.insert_extent(&mut fs, ext, &mut dev).unwrap();
        }

        let del = Ext4Extent::new(1, 0, 2);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, del, &mut dev).unwrap();
        }

        assert!(bitmap_block_is_allocated(&mut fs, &mut dev, base));
        assert!(!bitmap_block_is_allocated(&mut fs, &mut dev, base + 1));
        assert!(!bitmap_block_is_allocated(&mut fs, &mut dev, base + 2));
        assert!(bitmap_block_is_allocated(&mut fs, &mut dev, base + 3));

        let exts = collect_extents_from_inode(&mut inode, &mut dev);
        assert_eq!(exts.len(), 2);
        assert_eq!(exts[0].ee_block, 0);
        assert_eq!((exts[0].ee_len as u32) & 0x7FFF, 1);
        assert_eq!(((exts[0].ee_start_hi as u64) << 32) | (exts[0].ee_start_lo as u64), base);
        assert_eq!(exts[1].ee_block, 3);
        assert_eq!((exts[1].ee_len as u32) & 0x7FFF, 1);
        assert_eq!(((exts[1].ee_start_hi as u64) << 32) | (exts[1].ee_start_lo as u64), base + 3);
    }

    #[test]
    fn remove_extend_full_delete_single_extent_bitmap_and_metadata() {
        let (mut dev, mut fs) = setup_fs(32 * 1024);
        let mut inode = new_extent_inode();

        let base = alloc_contiguous(&mut fs, &mut dev, 2);
        let ext = Ext4Extent::new(0, base, 2);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.insert_extent(&mut fs, ext, &mut dev).unwrap();
        }

        let del = Ext4Extent::new(0, 0, 2);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, del, &mut dev).unwrap();
        }

        assert!(!bitmap_block_is_allocated(&mut fs, &mut dev, base));
        assert!(!bitmap_block_is_allocated(&mut fs, &mut dev, base + 1));
        let exts = collect_extents_from_inode(&mut inode, &mut dev);
        assert_eq!(exts.len(), 0);
    }

    #[test]
    fn remove_extend_multi_extent_skip_hole_and_verify() {
        let (mut dev, mut fs) = setup_fs(64 * 1024);
        let mut inode = new_extent_inode();

        let base1 = alloc_contiguous(&mut fs, &mut dev, 2);
        let _gap1 = alloc_data_block(&mut fs, &mut dev);
        let _gap2 = alloc_data_block(&mut fs, &mut dev);
        let base2 = alloc_contiguous(&mut fs, &mut dev, 2);

        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.insert_extent(&mut fs, Ext4Extent::new(0, base1, 2), &mut dev)
                .unwrap();
            tree.insert_extent(&mut fs, Ext4Extent::new(4, base2, 2), &mut dev)
                .unwrap();
        }

        // delete 3 allocated blocks starting at lbn=1: deletes lbn=1, then skips hole [2..4), then deletes lbn=4 and lbn=5
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, Ext4Extent::new(1, 0, 3), &mut dev)
                .unwrap();
        }

        assert!(bitmap_block_is_allocated(&mut fs, &mut dev, base1));
        assert!(!bitmap_block_is_allocated(&mut fs, &mut dev, base1 + 1));
        assert!(!bitmap_block_is_allocated(&mut fs, &mut dev, base2));
        assert!(!bitmap_block_is_allocated(&mut fs, &mut dev, base2 + 1));

        let exts = collect_extents_from_inode(&mut inode, &mut dev);
        assert_eq!(exts.len(), 1);
        assert_eq!(exts[0].ee_block, 0);
        assert_eq!((exts[0].ee_len as u32) & 0x7FFF, 1);
        assert_eq!(((exts[0].ee_start_hi as u64) << 32) | (exts[0].ee_start_lo as u64), base1);
    }

    #[test]
    fn remove_extend_over_length_errors_and_does_not_delete_unrelated() {
        let (mut dev, mut fs) = setup_fs(64 * 1024);
        let mut inode = new_extent_inode();

        let base1 = alloc_contiguous(&mut fs, &mut dev, 2);
        let base2 = alloc_contiguous(&mut fs, &mut dev, 1);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.insert_extent(&mut fs, Ext4Extent::new(0, base1, 2), &mut dev)
                .unwrap();
            tree.insert_extent(&mut fs, Ext4Extent::new(10, base2, 1), &mut dev)
                .unwrap();
        }

        let before_exts = collect_extents_from_inode(&mut inode, &mut dev);
        assert!(bitmap_block_is_allocated(&mut fs, &mut dev, base1));
        assert!(bitmap_block_is_allocated(&mut fs, &mut dev, base1 + 1));
        assert!(bitmap_block_is_allocated(&mut fs, &mut dev, base2));

        let res = {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, Ext4Extent::new(0, 0, 10), &mut dev)
        };
        assert!(res.is_err());

        // Unrelated extent must remain allocated and metadata should remain unchanged.
        assert!(bitmap_block_is_allocated(&mut fs, &mut dev, base2));
        let after_exts = collect_extents_from_inode(&mut inode, &mut dev);
        assert_eq!(before_exts.len(), after_exts.len());
        for (a, b) in before_exts.iter().zip(after_exts.iter()) {
            assert_eq!(a.ee_block, b.ee_block);
            assert_eq!(a.ee_len, b.ee_len);
            assert_eq!(a.ee_start_hi, b.ee_start_hi);
            assert_eq!(a.ee_start_lo, b.ee_start_lo);
        }
    }
}
