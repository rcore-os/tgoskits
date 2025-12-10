use log::{debug, error};

use alloc::vec::*;
use alloc::vec;
use log::*;
use crate::ext4_backend::jbd2::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::jbd2::jbdstruct::*;
use crate::ext4_backend::endian::*;
use crate::ext4_backend::superblock::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::loopfile::*;
use crate::ext4_backend::entries::*;
use crate::ext4_backend::mkfile::*;
use crate::ext4_backend::*;
use crate::ext4_backend::bmalloc::*;
use crate::ext4_backend::bitmap_cache::*;
use crate::ext4_backend::datablock_cache::*;
use crate::ext4_backend::inodetable_cache::*;
use crate::ext4_backend::blockgroup_description::*;

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


    ///测试用
    /// 构造一个最小可用的 Ext4FileSystem，用于不触发块分配的 insert_extent 测试
    fn make_dummy_fs() -> Ext4FileSystem {
        let superblock = Ext4Superblock::default();
        let block_allocator = BlockAllocator::new(&superblock);
        let inode_allocator = InodeAllocator::new(&superblock);
        let bitmap_cache = BitmapCache::default();
        let inodetable_cahce = InodeCache::new(INODE_CACHE_MAX, INODE_SIZE as usize);
        let datablock_cache = DataBlockCache::new(DATABLOCK_CACHE_MAX, BLOCK_SIZE);

        Ext4FileSystem {
            superblock,
            group_descs: Vec::new(),
            block_allocator,
            inode_allocator,
            bitmap_cache,
            inodetable_cahce,
            datablock_cache,
            root_inode: 2,
            group_count: 0,
            mounted: true,
            journal_sb_block_start:None,
        }
    }
 ///测试用
    /// 为触发分裂场景构造一个拥有 1 个块组的最小文件系统
    fn make_fs_for_split()  {
        let mut superblock = Ext4Superblock::default();

        // 配置块大小 / 每组块数，使得 BlockAllocator 可以工作
        // BLOCK_SIZE = 1024 << s_log_block_size => s_log_block_size = log2(BLOCK_SIZE/1024)
        superblock.s_log_block_size = (BLOCK_SIZE / 1024).trailing_zeros();
        superblock.s_blocks_per_group = 1024;
        superblock.s_inodes_per_group = 1024;
        superblock.s_desc_size = Ext4GroupDesc::disk_size() as u16;

        // 单块组，block bitmap 在块 1
        let mut desc = Ext4GroupDesc::default();
        desc.bg_block_bitmap_lo = 1; // 全局 block 1 作为位图
        desc.bg_free_blocks_count_lo = 100; // 有足够空闲块

        let block_allocator = BlockAllocator::new(&superblock);
        let inode_allocator = InodeAllocator::new(&superblock);
        let bitmap_cache = BitmapCache::default();
        let inodetable_cahce = InodeCache::new(INODE_CACHE_MAX, INODE_SIZE as usize);
        let datablock_cache = DataBlockCache::new(DATABLOCK_CACHE_MAX, BLOCK_SIZE);
        
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

    /// 从原始字节缓冲区解析一个 extent 节点（根或子节点）
    fn parse_node_from_bytes(bytes: &[u8]) -> Option<ExtentNode> {
        let hdr_size = Ext4ExtentHeader::disk_size();
        if bytes.len() < hdr_size {
            error!("Extent node buffer too small: {} < {}", bytes.len(), hdr_size);
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
                "Extent header entries overflow: entries={}, max={}",
                entries, max
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
            vec.sort_unstable_by_key(|entries| { entries.ee_block });
            Some(ExtentNode::Leaf { header, entries: vec })
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
            vec.sort_unstable_by_key(|entries| { entries.ei_block });
            Some(ExtentNode::Index { header, entries: vec })
        }
    }

    /// 从 inode.i_block 解析根节点
    pub fn load_root_from_inode(&self) -> Option<ExtentNode> {
        // inode.i_block 是 15 * u32 = 60 字节，正好容纳一个 extent 节点
        let iblocks = &self.inode.i_block;
        let bytes = unsafe {
            core::slice::from_raw_parts(iblocks.as_ptr() as *const u8, iblocks.len() * 4)
        };
        Self::parse_node_from_bytes(bytes)
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
                    let v = u32::from_le_bytes([
                        buf[off],
                        buf[off + 1],
                        buf[off + 2],
                        buf[off + 3],
                    ]);
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
                    let v = u32::from_le_bytes([
                        buf[off],
                        buf[off + 1],
                        buf[off + 2],
                        buf[off + 3],
                    ]);
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

                let child_block =
                    (chosen.ei_leaf_hi as u64) << 32 | (chosen.ei_leaf_lo as u64);

                debug!(
                    "Descending into extent child block {} for lblock {}",
                    child_block,
                    lblock
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
                        .map(|ix| (ix.ei_block, ((ix.ei_leaf_hi as u64) << 32) | ix.ei_leaf_lo as u64))
                        .collect::<Vec<_>>()
                );
            }
        }

        // 尝试递归插入
        let split_result = self.insert_recursive(fs, block_dev, &mut root, new_ext, None)?;

        match split_result {
            None => {
                // 没有发生根节点分裂，只需将更新后的根节点写回 Inode
                debug!("ExtentTree::insert_extent: no root split, writing updated root back to inode");
                self.store_root_to_inode(&root);
                Ok(())
            }
            Some(split_info) => {
                // 根节点分裂了，需要增加树的深度

                // 分配一个新的块，将“左半部分”（即原本在 Root 里的数据）移到这个新块中
                let new_left_block = fs.alloc_block(block_dev)?;
                debug!(
                    "ExtentTree::insert_extent: root split occurred, new_left_block={} split_info={{start_block={}, phy_block={}}}",
                    new_left_block,
                    split_info.start_block,
                    split_info.phy_block
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

                    let prev_logical = prev.ee_block as u32;
                    let mut prev_len = prev.ee_len as u32 & 0x7FFF;
                    let new_logical = new_ext.ee_block as u32;
                    let mut new_len = new_ext.ee_len as u32 & 0x7FFF;

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
                                        "insert_recursive: merged with previous extent -> new_len={} (no split yet)",
                                        total
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
                                            ee_block: tail_logical as u32,
                                            ee_len: (remain as u16 & 0x7FFF) | (new_ext.ee_len & 0x8000),
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
                        Self::write_node_to_block(
                            block_dev,
                            block_id,
                            &disk_node,
                            header.eh_max,
                        )?;
                    }
                    // Root 节点由调用方负责写回 Inode，这里返回 None
                    return Ok(None);
                }

                // 叶子节点分裂逻辑
                debug!("Leaf node overflow ({} > {}), splitting...", entries.len(), header.eh_max);
                // 分裂点：中间
                let split_idx = entries.len() / 2;
                let right_entries = entries.split_off(split_idx);
                // 当前 node 保留左半部分，header entries 数量更新
                header.eh_entries = entries.len() as u16;

                // 分配新块用于存储右半部分
                let new_phy_block = fs.alloc_block(block_dev)?;
                error!(
                    "insert_recursive: allocated new block for right leaf node: {}",
                    new_phy_block
                );

                // 构造右节点
                let right_header = Ext4ExtentHeader {
                    eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                    eh_entries: right_entries.len() as u16,
                    eh_max: Self::calc_block_eh_max(), // 新块一定是在磁盘上的，使用标准容量
                    eh_depth: 0, // 依然是 Leaf
                    eh_generation: 0,
                };
                let right_node = ExtentNode::Leaf {
                    header: right_header,
                    entries: right_entries,
                };

                //写回数据
                // 写右节点（新块）
                Self::write_node_to_block(block_dev, new_phy_block as u32, &right_node, right_header.eh_max)?;
                // 写左节点（当前节点）
                // 如果当前节点是普通块，写回磁盘；如果是 Root，调用方会处理，但这里我们要在内存中保持正确状态
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
                let mut child_node = Self::parse_node_from_bytes(child_bytes).expect("Can't parse node from bytes!");

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
                    debug!(
                        "insert_recursive: allocated new block for right index node: {}",
                        new_phy_block
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
                        Self::write_node_to_block(
                            block_dev,
                            block_id,
                            &disk_node,
                            header.eh_max,
                        )?;
                    }

                    // 返回分裂信息
                    // 索引节点的 Key 也是它覆盖范围的起始逻辑块号
                    let split_key = match &right_node {
                        ExtentNode::Index { entries, .. } => entries[0].ei_block,
                        _ => unreachable!(),
                    };

                    return Ok(Some(SplitInfo {
                        start_block: split_key,
                        phy_block: new_phy_block,
                    }));

                } else {
                    // 子节点没分裂，那就没事了
                    return Ok(None);
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
                    if off + et_size > buf.len() { break; }
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
                    if off + idx_size > buf.len() { break; }
                    idx.to_disk_bytes(&mut buf[off..off + idx_size]);
                }
            }
        }
        // 标记脏并写回
        dev.write_block(block_id,true)?;
        Ok(())
    }

    /// 计算标准数据块能容纳的条目数
    fn calc_block_eh_max() -> u16 {
        let hdr_size = Ext4ExtentHeader::disk_size();
        let entry_size = Ext4Extent::disk_size(); // Index 和 Extent 大小一样，都是 12
        ((BLOCK_SIZE as usize).saturating_sub(hdr_size) / entry_size) as u16
    }

    /// 辅助：获取节点的起始逻辑块号
    fn get_node_start_block(node: &ExtentNode) -> u32 {
        match node {
            ExtentNode::Leaf { entries, .. } => {
                if entries.is_empty() { 0 } else { entries[0].ee_block }
            }
            ExtentNode::Index { entries, .. } => {
                if entries.is_empty() { 0 } else { entries[0].ei_block }
            }
        }
    }
}
