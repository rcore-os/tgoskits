use alloc::string::ToString;
use alloc::vec::Vec;
use log::{error, warn};

use crate::ext4_backend::jbd2::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::jbd2::jbdstruct::*;
use crate::ext4_backend::endian::*;
use crate::ext4_backend::superblock::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::extents_tree::*;
use crate::ext4_backend::loopfile::*;
use crate::ext4_backend::mkd::*;
use crate::ext4_backend::entries::*;
///mkfile lib




/// 根据数据块列表为普通文件 inode 构建块映射：
/// - 否则使用传统直接块指针（i_block[0..]）。
pub fn build_file_block_mapping<B:BlockDevice>(
    fs: &mut Ext4FileSystem,
    inode: &mut Ext4Inode,
    data_blocks: &[u64],
    block_dev:&mut Jbd2Dev<B>
) {
    if data_blocks.is_empty() {
        inode.i_blocks_lo = 0;
        inode.l_i_blocks_high = 0;
        inode.i_block = [0; 15];
        return;
    }

    

    if fs.superblock.has_extents() {
        
        // 使用 extent 映射数据块，优先合并连续块
        inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
        inode.i_block = [0; 15];

        //初始头构建
        if !inode.have_extend_header() {
            warn!("inode not have a valid extend magic,will building...");
            inode.write_extend_header();
            
        }
    
            let mut exts_vec: Vec<Ext4Extent> = Vec::new();

            let mut run_start_lbn: u32 = 0;
            let mut run_start_pblk: u64 = data_blocks[0];
            let mut run_len: u32 = 1;

            for (idx, &pblk) in data_blocks.iter().enumerate().skip(1) {
                let lbn = idx as u32;
                let prev_lbn = lbn - 1;
                let prev_pblk = data_blocks[prev_lbn as usize];

                let is_contiguous = pblk == prev_pblk.saturating_add(1);

                if is_contiguous {
                    run_len = run_len.saturating_add(1);
                } else {
                    // 结束当前 run，生成一个 extent
                    let ext = Ext4Extent::new(run_start_lbn, run_start_pblk, run_len as u16);
                    exts_vec.push(ext);

                    run_start_lbn = lbn;
                    run_start_pblk = pblk;
                    run_len = 1;
                }
            }

                let ext = Ext4Extent::new(run_start_lbn, run_start_pblk, run_len as u16);
                exts_vec.push(ext);

        // 构造一个叶子根节点，并通过 ExtentTree 将其写入 inode.i_block
        let mut tree = ExtentTree::new(inode);
        for extend in exts_vec {
            tree.insert_extent(fs, extend,block_dev )     ;       
        }
    } else {
        //传统直接最多使用前12个+3个间接指针
        inode.i_block = [0; 15];
        for (i, blk) in data_blocks.iter().take(12).enumerate() {
            inode.i_block[i] = *blk as u32;
        }
        if data_blocks.len() > 12 {
            //需要1级间接块
            error!("not support tranditional block pointer");
        }
    }
}

///创建文件类型entry通用接口
/// 传入文件名称,可选初始数据
pub fn mkfile<B: BlockDevice>(device: &mut Jbd2Dev<B>,fs:&mut Ext4FileSystem, path:&str,initial_data:Option<&[u8]>)->Option<Ext4Inode>{
    // 规范化路径
    let norm_path = split_paren_child_and_tranlatevalid(path);

    // 如果目标已存在，直接返回
    if let Ok(Some(inode)) = get_file_inode(fs, device, &norm_path) {
        return Some(inode);
    }

    // 拆 parent / child
    let mut valid_path = norm_path;
    let split_point = valid_path.rfind('/')?;
    let child = valid_path.split_off(split_point)[1..].to_string();
    let parent = valid_path;

    // 确保父目录存在
    if mkdir(device, fs, &parent).is_none() {
        return None;
    }

    // 重新获取父目录 inode 及其 inode 号
    let (parent_ino_num, parent_inode) = match get_inode_with_num(fs, device, &parent).ok().flatten() {
        Some((n, ino)) => (n, ino),
        None => return None,
    };

    //为新文件分配 inode（内部自动选择块组）
    let new_file_ino = match fs.alloc_inode(device) {
        Ok(ino) => ino,
        Err(_) => return None,
    };

    // 如有初始数据，为文件分配一个或多个数据块并写入
    let mut data_blocks: Vec<u64> = Vec::new();
    let mut total_written: usize = 0;
    if let Some(buf) = initial_data {
        let mut remaining = buf.len();
        let mut src_off = 0usize;

        while remaining > 0 {
            // 如果未启用 extents，则最多只使用 12 个直接块
            if !fs.superblock.has_extents() && data_blocks.len() >= 12 {
                break;
            }

            let blk = match fs.alloc_block(device) {
                Ok(b) => b,
                Err(_) => break,
            };

            let write_len = core::cmp::min(remaining, BLOCK_SIZE as usize);

            // 将数据写入新分配的数据块，其余部分填零
             fs.datablock_cache.modify_new( blk as u64, |data| {
                for b in data.iter_mut() {
                    *b = 0;
                }
                let end = src_off + write_len;
                data[..write_len].copy_from_slice(&buf[src_off..end]);
            });

            data_blocks.push(blk);
            total_written += write_len;
            remaining -= write_len;
            src_off += write_len;
        }
    }

    // 构造新文件 inode 的内存版本，然后通过 modify_inode 一次性写回
    let mut new_inode = Ext4Inode::default();
    new_inode.i_mode = Ext4Inode::S_IFREG | 0o644;
    new_inode.i_links_count = 1;

    if !data_blocks.is_empty() {
        // 有初始数据：多块或单块文件
        let used_blocks = data_blocks.len() as u32;
        new_inode.i_size_lo = total_written as u32;
        new_inode.i_size_high = 0;
        new_inode.i_blocks_lo = used_blocks.saturating_mul((BLOCK_SIZE / 512) as u32);
        new_inode.l_i_blocks_high = 0;

        build_file_block_mapping(fs, &mut new_inode, &data_blocks,device);
    } else {
        //无初始数据：空文件
        new_inode.i_size_lo = 0;
        new_inode.i_size_high = 0;
        new_inode.i_blocks_lo = 0;
        new_inode.l_i_blocks_high = 0;
        new_inode.i_block = [0; 15];
    }

    if fs
        .modify_inode(device, new_file_ino, |on_disk| {
            *on_disk = new_inode.clone();
        })
        .is_err()
    {
        return None;
    }

    //在父目录中插入一个普通文件类型的目录项（必要时自动扩展目录块）
    let mut parent_inode_copy = parent_inode;
    if insert_dir_entry(fs, device, parent_ino_num, &mut parent_inode_copy, new_file_ino, &child, Ext4DirEntry2::EXT4_FT_REG_FILE).is_err() {
        return None;
    }

    //返回新文件 inode
    get_file_inode(fs, device, path).ok().flatten()

}

///读取指定路径的整个文件内容
pub fn read_file<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> BlockDevResult<Option<Vec<u8>>> {
    let mut inode = match get_file_inode(fs, device, path) {
        Ok(Some(ino)) => ino,
        Ok(None) => return Ok(None),
        Err(e) => return Err(e),
    };
    if !inode.is_file() {
        error!("Entry:{} not aa file",path);
        return BlockDevResult::Err(BlockDevError::ReadError)
    }

    let size = inode.size() as usize;
    if size == 0 {
        return Ok(Some(Vec::new()));
    }

    let block_bytes = BLOCK_SIZE as usize;
    let total_blocks = (size + block_bytes - 1) / block_bytes;

    let mut buf = Vec::with_capacity(size);

    for lbn in 0..total_blocks {
        let phys = match resolve_inode_block(fs, device, &mut inode, lbn as u32)? {
            Some(b) => b,
            None => break,
        };

        let cached = fs
            .datablock_cache
            .get_or_load(device, phys as u64)?;
        let data = &cached.data[..block_bytes];
        buf.extend_from_slice(data);
    }

    buf.truncate(size);
    Ok(Some(buf))
}

pub fn write_file<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    offset: usize,
    data: &[u8],
) -> BlockDevResult<()> {
    if data.is_empty() {
        return Ok(());
    }

    // 获取 inode 及其 inode 号
    let info = match get_inode_with_num(fs, device, path).ok().flatten() {
        Some(v) => v,
        None => return Err(BlockDevError::WriteError),
    };
    let (inode_num, mut inode) = info;

    let old_size = inode.size() as usize;
    let block_bytes = BLOCK_SIZE as usize;

    if old_size == 0 {
        return Err(BlockDevError::Unsupported);
    }

    if offset > old_size {
        return Err(BlockDevError::Unsupported);
    }

    let end = offset.saturating_add(data.len());
    let old_blocks = if old_size == 0 {
        0
    } else {
        (old_size + block_bytes - 1) / block_bytes
    };
    let new_blocks = if end == 0 {
        0
    } else {
        (end + block_bytes - 1) / block_bytes
    };

    if end > old_size {
        if !fs.superblock.has_extents() || !inode.is_extent() {
            // 只在 extent 模式下支持扩展
            return Err(BlockDevError::Unsupported);
        }

        let mut new_blocks_map: Vec<(u32, u64)> = Vec::new();
        for lbn in old_blocks as u32..new_blocks as u32 {
            let phys = fs.alloc_block(device)?;
            new_blocks_map.push((lbn, phys));
        }

        let mut tree = ExtentTree::new(&mut inode);

        if !new_blocks_map.is_empty() {
            //合并extent
            let mut idx = 0usize;
            while idx < new_blocks_map.len() {
                let (start_lbn, start_phys) = new_blocks_map[idx];
                let mut run_len: u32 = 1;
                let mut last_lbn = start_lbn;
                let mut last_phys = start_phys;

                idx += 1;
                while idx < new_blocks_map.len() {
                    let (cur_lbn, cur_phys) = new_blocks_map[idx];
                    if cur_lbn == last_lbn + 1 && cur_phys == last_phys + 1 {
                        run_len = run_len.saturating_add(1);
                        last_lbn = cur_lbn;
                        last_phys = cur_phys;
                        idx += 1;
                    } else {
                        break;
                    }
                }

                let ext = Ext4Extent::new(start_lbn, start_phys, run_len as u16);
                tree.insert_extent(fs, ext, device)?;
            }
        }

        // 更新 inode 的大小和块计数
        let new_size = end;
        inode.i_size_lo = new_size as u32;
        inode.i_size_high = (new_size >> 32) as u32;
        let used_blocks = new_blocks as u32;
        inode.i_blocks_lo = used_blocks.saturating_mul((BLOCK_SIZE / 512) as u32);
        inode.l_i_blocks_high = 0;

        // 写回 inode 元数据
        let (group_idx, _idx) = fs.inode_allocator.global_to_group(inode_num);
        let inode_table_start = match fs.group_descs.get(group_idx as usize) {
            Some(desc) => desc.inode_table() as u64,
            None => return Err(BlockDevError::Corrupted),
        };
        let (block_num, off, _g) = fs.inodetable_cahce.calc_inode_location(
            inode_num,
            fs.superblock.s_inodes_per_group,
            inode_table_start,
            BLOCK_SIZE,
        );

        fs.inodetable_cahce.modify(
            device,
            inode_num as u64,
            block_num,
            off,
            |on_disk| {
                on_disk.i_size_lo = inode.i_size_lo;
                on_disk.i_size_high = inode.i_size_high;
                on_disk.i_blocks_lo = inode.i_blocks_lo;
                on_disk.l_i_blocks_high = inode.l_i_blocks_high;
                on_disk.i_flags = inode.i_flags;
                on_disk.i_block = inode.i_block;
            },
        )?;
    }

    let final_size = old_size.max(end);
    let total_blocks = if final_size == 0 {
        0
    } else {
        (final_size + block_bytes - 1) / block_bytes
    };

    let start_lbn = offset / block_bytes;
    let end_lbn = (end - 1) / block_bytes;

    for lbn in start_lbn..=end_lbn {
        let phys = match resolve_inode_block(fs, device, &mut inode, lbn as u32)? {
            Some(b) => b,
            None => return Err(BlockDevError::Corrupted),
        };

        fs.datablock_cache.modify(device, phys as u64, |blk| {
            let block_start = lbn * block_bytes;
            let block_end = block_start + block_bytes;

            let write_start = core::cmp::max(offset, block_start);
            let write_end = core::cmp::min(end, block_end);
            if write_start >= write_end {
                return;
            }

            let src_off = write_start - offset;
            let dst_off = write_start - block_start;
            let len = write_end - write_start;

            blk[dst_off..dst_off + len]
                .copy_from_slice(&data[src_off..src_off + len]);
        })?;
    }

    Ok(())
}