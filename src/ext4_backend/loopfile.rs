//文件遍历

use alloc::vec::Vec;
use log::{error, info};

use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::entries::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::extents_tree::*;
use crate::ext4_backend::hashtree::*;
use log::debug;

/// 根据 inode 的逻辑块号解析到物理块号，支持 12 个直接块和 1/2/3 级间接块
pub fn resolve_inode_block<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    inode: &mut Ext4Inode,
    logical_block: u32,
) -> BlockDevResult<Option<u32>> {
    // 优先走 extent 树（支持多层索引）；失败时再回退到传统多级指针逻辑
    if inode.is_extent() {
        let mut tree = ExtentTree::new(inode);
        if let Some(ext) = tree.find_extent(block_dev, logical_block)? {
            let mut len = ext.ee_len as u32;
            // 最高位表示 uninitialized 标志，长度使用低 15 位
            if (len & 0x8000) != 0 {
                len &= 0x7FFF;
            }
            if len == 0 {
                return Ok(None);
            }

            let start_lbn = ext.ee_block;
            if logical_block < start_lbn || logical_block >= start_lbn.saturating_add(len) {
                return Ok(None);
            }

            let base = ((ext.ee_start_hi as u64) << 32) | ext.ee_start_lo as u64;
            let phys = base + (logical_block - start_lbn) as u64;
            if phys > u32::MAX as u64 {
                return Err(BlockDevError::Corrupted);
            }
            return Ok(Some(phys as u32));
        }
        error!("Can;t find proper extend for this logical block");
    }

    let lbn = logical_block as usize;
    let per_block = BLOCK_SIZE / 4; // 每个间接块能存多少个 u32 块号

    //  直接块 [0, 12)
    if lbn < 12 {
        let blk = inode.i_block[lbn];
        return Ok(if blk == 0 { None } else { Some(blk) });
    }

    //  一级间接块
    let mut idx = lbn - 12;
    if idx < per_block {
        let ind_blk = inode.i_block[12];
        if ind_blk == 0 {
            return Ok(None);
        }
        let cached = fs.datablock_cache.get_or_load(block_dev, ind_blk as u64)?;
        let data = &cached.data[..BLOCK_SIZE];
        let off = idx * 4;
        if off + 4 > data.len() {
            return Ok(None);
        }
        let raw = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        return Ok(if raw == 0 { None } else { Some(raw) });
    }

    //  二级间接块
    idx -= per_block;
    let level1_span = per_block * per_block;
    if idx < level1_span {
        let l1_blk = inode.i_block[13];
        if l1_blk == 0 {
            return Ok(None);
        }

        let first_idx = idx / per_block;
        let second_idx = idx % per_block;

        // 读取一级间接块，取出对应的二级块号
        let l1_cached = fs.datablock_cache.get_or_load(block_dev, l1_blk as u64)?;
        let l1_data = &l1_cached.data[..BLOCK_SIZE];
        let off1 = first_idx * 4;
        if off1 + 4 > l1_data.len() {
            return Ok(None);
        }
        let l2_blk = u32::from_le_bytes([
            l1_data[off1],
            l1_data[off1 + 1],
            l1_data[off1 + 2],
            l1_data[off1 + 3],
        ]);
        if l2_blk == 0 {
            return Ok(None);
        }

        // 读取二级间接块，取出最终数据块号
        let l2_cached = fs.datablock_cache.get_or_load(block_dev, l2_blk as u64)?;
        let l2_data = &l2_cached.data[..BLOCK_SIZE];
        let off2 = second_idx * 4;
        if off2 + 4 > l2_data.len() {
            return Ok(None);
        }
        let data_blk = u32::from_le_bytes([
            l2_data[off2],
            l2_data[off2 + 1],
            l2_data[off2 + 2],
            l2_data[off2 + 3],
        ]);
        return Ok(if data_blk == 0 { None } else { Some(data_blk) });
    }

    //  三级间接块
    idx -= level1_span;
    let level2_span = per_block * per_block * per_block;
    if idx >= level2_span {
        // 超出三级间接能表示的范围
        return Ok(None);
    }

    let l0_blk = inode.i_block[14];
    if l0_blk == 0 {
        return Ok(None);
    }

    let idx0 = idx / level1_span; // 第一级索引
    let rem = idx % level1_span;
    let idx1 = rem / per_block; // 第二级索引
    let idx2 = rem % per_block; // 第三级索引

    // 第一级
    let l0_cached = fs.datablock_cache.get_or_load(block_dev, l0_blk as u64)?;
    let l0_data = &l0_cached.data[..BLOCK_SIZE];
    let off0 = idx0 * 4;
    if off0 + 4 > l0_data.len() {
        return Ok(None);
    }
    let l1_blk = u32::from_le_bytes([
        l0_data[off0],
        l0_data[off0 + 1],
        l0_data[off0 + 2],
        l0_data[off0 + 3],
    ]);
    if l1_blk == 0 {
        return Ok(None);
    }

    // 第二级
    let l1_cached = fs.datablock_cache.get_or_load(block_dev, l1_blk as u64)?;
    let l1_data = &l1_cached.data[..BLOCK_SIZE];
    let off1 = idx1 * 4;
    if off1 + 4 > l1_data.len() {
        return Ok(None);
    }
    let l2_blk = u32::from_le_bytes([
        l1_data[off1],
        l1_data[off1 + 1],
        l1_data[off1 + 2],
        l1_data[off1 + 3],
    ]);
    if l2_blk == 0 {
        return Ok(None);
    }

    // 第三级
    let l2_cached = fs.datablock_cache.get_or_load(block_dev, l2_blk as u64)?;
    let l2_data = &l2_cached.data[..BLOCK_SIZE];
    let off2 = idx2 * 4;
    if off2 + 4 > l2_data.len() {
        return Ok(None);
    }
    let data_blk = u32::from_le_bytes([
        l2_data[off2],
        l2_data[off2 + 1],
        l2_data[off2 + 2],
        l2_data[off2 + 3],
    ]);

    Ok(if data_blk == 0 { None } else { Some(data_blk) })
}

pub fn resolve_inode_block_allextend<B: BlockDevice>(
    _fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    inode: &mut Ext4Inode,
) -> BlockDevResult<Vec<u64>> {
    if !inode.is_extent() {
        return Ok(Vec::new());
    }

    fn push_extent_blocks(out: &mut Vec<u64>, ext: &Ext4Extent) {
        let mut len = ext.ee_len as u32;
        // 最高位表示 uninitialized 标志，长度使用低 15 位
        if (len & 0x8000) != 0 {
            len &= 0x7FFF;
        }
        if len == 0 {
            return;
        }
        let base = ((ext.ee_start_hi as u64) << 32) | ext.ee_start_lo as u64;
        for i in 0..len as u64 {
            out.push(base + i);
        }
    }

    fn walk_node<B: BlockDevice>(
        dev: &mut Jbd2Dev<B>,
        node: &ExtentNode,
        out: &mut Vec<u64>,
    ) -> BlockDevResult<()> {
        match node {
            ExtentNode::Leaf { entries, .. } => {
                for ext in entries {
                    push_extent_blocks(out, ext);
                }
                Ok(())
            }
            ExtentNode::Index { entries, .. } => {
                for idx in entries {
                    let child_block = ((idx.ei_leaf_hi as u64) << 32) | (idx.ei_leaf_lo as u64);
                    dev.read_block(child_block as u32)?;
                    let buf = dev.buffer();
                    let child = ExtentTree::parse_node(buf).ok_or(BlockDevError::Corrupted)?;
                    walk_node(dev, &child, out)?;
                }
                Ok(())
            }
        }
    }

    let tree = ExtentTree::new(inode);
    let root = match tree.load_root_from_inode() {
        Some(n) => n,
        None => return Ok(Vec::new()),
    };

    let mut blocks: Vec<u64> = Vec::new();
    walk_node(block_dev, &root, &mut blocks)?;
    blocks.sort_unstable();
    blocks.dedup();
    Ok(blocks)
}

///传入完整的路径信息按照特性进行扫描。
pub fn get_file_inode<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    path: &str,
) -> BlockDevResult<Option<(u32, Ext4Inode)>> {
    // 规范化路径：空串或"/" 视为根目录
    if path.is_empty() || path == "/" {
        let inode = fs.get_root(block_dev)?;
        return Ok(Some((fs.root_inode, inode)));
    }

    // 按 '/' 分割，过滤掉空段
    let components = path.split('/').filter(|s| !s.is_empty());

    // 从根目录开始逐级解析，并维护一个路径栈以支持 ".." 回溯
    let mut current_inode = fs.get_root(block_dev)?;
    let mut current_ino_num: u32 = fs.root_inode;
    let mut path_vec: Vec<Ext4Inode> = Vec::new();
    path_vec.push(current_inode);

    // 根目录所在的 inode 表起始块目前按 group0 处理
    let inode_table_start = match fs.group_descs.first() {
        Some(desc) => desc.inode_table(),
        None => return Err(BlockDevError::Corrupted),
    };
    for name in components {
        if !current_inode.is_dir() {
            // 中间层不是目录，路径非法
            return Ok(None);
        }

        // 特殊处理当前目录和父目录
        if name == "." {
            continue;
        }
        if name == ".." {
            // 回溯到父目录：栈中至少保留根目录一层
            if path_vec.len() > 1 {
                path_vec.pop();
                if let Some(parent_inode) = path_vec.last() {
                    current_inode = *parent_inode;
                }
            }
            continue;
        }

        let target = name.as_bytes();
        let mut found_inode_num: Option<u64> = None;

        // 尝试使用哈希树查找
        match lookup_directory_entry(fs, block_dev, &current_inode, target) {
            Ok(result) => {
                found_inode_num = Some(result.entry.inode as u64);
            }
            Err(_) => {
                // 哈希树查找失败，回退到线性查找
                debug!("Hash tree lookup failed, falling back to linear search");

                // 使用 resolve_inode_block_allextend 获取所有物理块，然后逐块线性查找
                let total_size = current_inode.size() as usize;
                let block_bytes = BLOCK_SIZE;
                let blocks = resolve_inode_block_allextend(fs, block_dev, &mut current_inode)?;
                info!(
                    "Directory inode size: {} bytes, blocks used: {}",
                    &total_size,
                    &blocks.len()
                );

                for (idx, phys) in blocks.iter().enumerate() {
                    info!("Scan dir block idx {} phys {}", &idx, phys);
                    let cached_block = fs.datablock_cache.get_or_load(block_dev, *phys)?;
                    let block_data = &cached_block.data[..block_bytes];

                    if let Some(entry) = classic_dir::find_entry(block_data, target) {
                        found_inode_num = Some(entry.inode as u64);
                        break;
                    }
                }
            }
        }

        let inode_num = match found_inode_num {
            Some(n) => n,
            None => return Ok(None),
        };

        let inode_num_u32 = inode_num as u32;

        let (block_num, offset, _group_idx) = fs.inodetable_cahce.calc_inode_location(
            inode_num_u32,
            fs.superblock.s_inodes_per_group,
            inode_table_start,
            BLOCK_SIZE,
        );

        let cached_inode = fs
            .inodetable_cahce
            .get_or_load(block_dev, inode_num, block_num, offset)?;
        current_inode = cached_inode.inode;
        current_ino_num = inode_num_u32;
        path_vec.push(current_inode);
    }

    Ok(Some((current_ino_num, current_inode)))
}
