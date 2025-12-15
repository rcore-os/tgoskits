use core::u32;

use alloc::string::ToString;
use alloc::vec::Vec;
use log::{error, info};
use log::{debug, warn};

use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::dir::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::entries::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::extents_tree::*;
use crate::ext4_backend::loopfile::*;
use crate::ext4_backend::error::*;
use alloc::string::String;




pub fn rename<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    old_path: &str,
    new_path: &str,
) -> BlockDevResult<()> {
    let old_norm = split_paren_child_and_tranlatevalid(old_path);
    let new_norm = split_paren_child_and_tranlatevalid(new_path);

    // 新文件是否存在：存在则先删除
    if let Some((_ino, inod)) = get_inode_with_num(fs, device, &new_norm).ok().flatten() {
        if inod.is_dir() {
            delete_dir(fs, device, new_path);
        } else {
            delete_file(fs, device, new_path);
        }
    }
    //删除了还存在？错误!
    if get_inode_with_num(fs, device, &new_norm).ok().flatten().is_some() {
        return Err(BlockDevError::WriteError);
    }

    mv(fs, device, &old_norm, &new_norm)?;

    // 校验
    if get_inode_with_num(fs, device, &old_norm).ok().flatten().is_some() {
        return Err(BlockDevError::WriteError);
    }
    if get_inode_with_num(fs, device, &new_norm).ok().flatten().is_none() {
        return Err(BlockDevError::WriteError);
    }

    Ok(())
}
pub fn truncate<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    truncate_size: u64,
) -> BlockDevResult<()> {
    let norm_path = split_paren_child_and_tranlatevalid(path);

    // 首先找到目标文件。
    let (inode_num, _inode) = match get_inode_with_num(fs, device, &norm_path).ok().flatten() {
        Some(v) => v,
        None => return Err(BlockDevError::InvalidInput),
    };

    truncate_with_ino(device, fs, inode_num, truncate_size)
}

///TODO:shrink暂时不要用不成熟   记得更新inodesize extendtree不负责更新inodesize
pub fn truncate_with_ino<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: u32,
    truncate_size: u64,
) -> BlockDevResult<()> {
    let mut inode = fs.get_inode_by_num(device, inode_num)?;
    
    if !inode.is_file() {
        warn!("trubcate abnormal file")
    }else if inode.is_symlink() {
        error!("Can't truncate symlink file!");
        return Err(BlockDevError::Unsupported);
    }

    let old_size = inode.size();
    if truncate_size == old_size {
        return Ok(());
    }

    let block_bytes = BLOCK_SIZE as u64;
    let old_blocks = if old_size == 0 {
        0u64
    } else {
        old_size.div_ceil(block_bytes)
    };
    let new_blocks = if truncate_size == 0 {
        0u64
    } else {
        truncate_size.div_ceil(block_bytes)
    };

    // extent 分支：支持 grow；shrink 仅支持 truncate 到 0（否则需要删/裁剪 extent）
    if fs.superblock.has_extents() && inode.have_extend_header_and_use_extend() {
        if truncate_size < old_size {
            // shrink：删除逻辑范围尾部，但 hole 不应导致 double free。
            // 通过 ExtentTree::remove_extend 让 extent tree 内部负责释放物理块。
            let del_start_lbn = new_blocks as u32;

            loop {
                let blocks_map = resolve_inode_block_allextend(fs, device, &mut inode)?;
                let del_len = if truncate_size == 0 {
                    blocks_map.len() as u32
                } else {
                    blocks_map.range(del_start_lbn..).count() as u32
                };

                if del_len == 0 {
                    break;
                }

                let start_lbn = if truncate_size == 0 {
                    // Plan B: start from the first mapped LBN to avoid rescanning from 0 repeatedly.
                    let Some((&first_lbn, _)) = blocks_map.iter().next() else {
                        break;
                    };
                    first_lbn
                } else {
                    del_start_lbn
                };

                let chunk = core::cmp::min(del_len, 0x7FFF);
                {
                    let mut tree = ExtentTree::new(&mut inode);
                    tree.remove_extend(fs, Ext4Extent::new(start_lbn, 0, chunk as u16), device)?;
                }
            }
        }

        if new_blocks > old_blocks {


            let mut new_blocks_map: Vec<(u32, u64)> = Vec::new();
            for lbn in old_blocks as u32..new_blocks as u32 {
                let phys = fs.alloc_block(device)?;
                fs.datablock_cache.modify_new(phys, |data| {
                    for b in data.iter_mut() {
                        *b = 0;
                    }
                });
                new_blocks_map.push((lbn, phys));
            }

            let mut tree = ExtentTree::new(&mut inode);
            if !new_blocks_map.is_empty() {
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
        }

        inode.i_size_lo = (truncate_size & 0xffff_ffff) as u32;
        inode.i_size_high = (truncate_size >> 32) as u32;
        // i_blocks reflects number of allocated blocks, not logical length. Recompute after edits.
        let alloc_blocks = resolve_inode_block_allextend(fs, device, &mut inode)?.len() as u64;
        let iblocks_used = alloc_blocks.saturating_mul(BLOCK_SIZE as u64 / 512);
        inode.i_blocks_lo = (iblocks_used & 0xffff_ffff) as u32;
        inode.l_i_blocks_high = ((iblocks_used >> 32) & 0xffff) as u16;

        fs.modify_inode(device, inode_num, |td| {
            *td = inode;
        })?;
        return Ok(());
    }

    //todo:
    // 非 extent：仅支持 12 个直接块（现有实现本来就不支持间接块）
    if new_blocks > 12 {
        return Err(BlockDevError::Unsupported);
    }

    // grow：分配新块并填 0，写入 i_block
    if new_blocks > old_blocks {
        for lbn in old_blocks as u32..new_blocks as u32 {
            let phys = fs.alloc_block(device)?;
            fs.datablock_cache.modify_new(phys, |data| {
                for b in data.iter_mut() {
                    *b = 0;
                }
            });
            inode.i_block[lbn as usize] = phys as u32;
        }
    }

    // shrink：释放尾部块，并清空 i_block
    if new_blocks < old_blocks {
        for lbn in new_blocks as u32..old_blocks as u32 {
            let phys = inode.i_block[lbn as usize] as u64;
            if phys != 0 {
                fs.free_block(device, phys)?;
            }
            inode.i_block[lbn as usize] = 0;
        }
    }

    inode.i_size_lo = (truncate_size & 0xffff_ffff) as u32;
    inode.i_size_high = (truncate_size >> 32) as u32;
    let iblocks_used = (new_blocks.saturating_mul(BLOCK_SIZE as u64 / 512)) as u64;
    inode.i_blocks_lo = (iblocks_used & 0xffff_ffff) as u32;
    inode.l_i_blocks_high = ((iblocks_used >> 32) & 0xffff) as u16;

    fs.modify_inode(device, inode_num, |td| {
        *td = inode;
    })?;

    Ok(())
}
pub fn create_symbol_link<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    src_path: &str,
    dst_path: &str,
) -> BlockDevResult<()> {
    // 首先判断两个目标文件是否存在，被链接不存在报错，链接文件存在报错。
    let src_norm = split_paren_child_and_tranlatevalid(src_path);
    let dst_norm = split_paren_child_and_tranlatevalid(dst_path);

    if get_file_inode(fs, device, &src_norm)?.is_none() {
        return Err(BlockDevError::InvalidInput);
    }
    if get_file_inode(fs, device, &dst_norm)?.is_some() {
        return Err(BlockDevError::InvalidInput);
    }

    // 拆 parent / child（父目录必须存在）
    let (parent, child) = if let Some(pos) = dst_norm.rfind('/') {
        let p = if pos == 0 {
            "/".to_string()
        } else {
            dst_norm[..pos].to_string()
        };
        let c = dst_norm[pos + 1..].to_string();
        (p, c)
    } else {
        ("/".to_string(), dst_norm)
    };

    let (parent_ino_num, parent_inode) = match get_inode_with_num(fs, device, &parent).ok().flatten()
    {
        Some(v) => v,
        None => return Err(BlockDevError::InvalidInput),
    };
    if !parent_inode.is_dir() {
        return Err(BlockDevError::InvalidInput);
    }

    // 为新链接分配 inode
    let new_ino = fs.alloc_inode(device)?;

    let target_bytes = src_path.as_bytes();
    let target_len = target_bytes.len();
    let size_lo = (target_len as u64 & 0xffffffff) as u32;
    let size_hi = ((target_len as u64) >> 32) as u32;

    let mut new_inode = Ext4Inode::default();
    new_inode.i_mode = Ext4Inode::S_IFLNK | 0o777;
    new_inode.i_links_count = 1;
    new_inode.i_size_lo = size_lo;
    new_inode.i_size_high = size_hi;

    if target_len == 0 {
        new_inode.i_blocks_lo = 0;
        new_inode.l_i_blocks_high = 0;
        new_inode.i_block = [0; 15];
    } else if target_len <= 60 {
        // fast symlink：目标路径直接写入 i_block
        let mut raw = [0u8; 60];
        raw[..target_len].copy_from_slice(target_bytes);
        for i in 0..15 {
            new_inode.i_block[i] = u32::from_le_bytes([
                raw[i * 4],
                raw[i * 4 + 1],
                raw[i * 4 + 2],
                raw[i * 4 + 3],
            ]);
        }
        new_inode.i_blocks_lo = 0;
        new_inode.l_i_blocks_high = 0;
    } else {
        // 普通 symlink：用数据块存储目标路径
        let mut data_blocks: Vec<u64> = Vec::new();
        let mut remaining = target_len;
        let mut src_off = 0usize;

        while remaining > 0 {
            if !fs.superblock.has_extents() && data_blocks.len() >= 12 {
                return Err(BlockDevError::Unsupported);
            }

            let blk = fs.alloc_block(device)?;
            let write_len = core::cmp::min(remaining, BLOCK_SIZE);
            fs.datablock_cache.modify_new(blk, |data| {
                for b in data.iter_mut() {
                    *b = 0;
                }
                let end = src_off + write_len;
                data[..write_len].copy_from_slice(&target_bytes[src_off..end]);
            });

            data_blocks.push(blk);
            remaining -= write_len;
            src_off += write_len;
        }

        let used_datablocks = data_blocks.len() as u64;
        let iblocks_used = used_datablocks.saturating_mul(BLOCK_SIZE as u64 / 512) as u32;
        new_inode.i_blocks_lo = iblocks_used;
        new_inode.l_i_blocks_high = (iblocks_used as u64 >> 32) as u16;

        build_file_block_mapping(fs, &mut new_inode, &data_blocks, device);
    }

    fs.modify_inode(device, new_ino, |on_disk| {
        *on_disk = new_inode;
    })?;

    // 插入父目录目录项（symlink 类型）
    let mut parent_inode_copy = parent_inode;
    insert_dir_entry(
        fs,
        device,
        parent_ino_num,
        &mut parent_inode_copy,
        new_ino,
        &child,
        Ext4DirEntry2::EXT4_FT_SYMLINK,
    )?;

    Ok(())
}




fn read_symlink_target<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode: &mut Ext4Inode,
) -> BlockDevResult<Vec<u8>> {


    let size = inode.size() as usize;
    if size == 0 {
        return Ok(Vec::new());
    }

    if size <= 60 {
        let mut raw = [0u8; 60];
        for (i, word) in inode.i_block.iter().take(15).enumerate() {
            raw[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        return Ok(raw[..size].to_vec());
    }

    let block_bytes = BLOCK_SIZE;
    let total_blocks = size.div_ceil(block_bytes);
    let mut buf = Vec::with_capacity(size);

    if inode.have_extend_header_and_use_extend() {
        let blocks = resolve_inode_block_allextend(fs, device, inode)?;
        for &phys in blocks.values() {
            let cached = fs.datablock_cache.get_or_load(device, phys)?;
            let data = &cached.data[..block_bytes];
            buf.extend_from_slice(data);
            if buf.len() >= size {
                break;
            }
        }
    } else {
        for lbn in 0..total_blocks {
            let phys = match resolve_inode_block( device, inode, lbn as u32)? {
                Some(b) => b,
                None => break,
            };
            let cached = fs.datablock_cache.get_or_load(device, phys as u64)?;
            let data = &cached.data[..block_bytes];
            buf.extend_from_slice(data);
        }
    }

    buf.truncate(size);

  

    Ok(buf)
}

fn resolve_symlink_path(current_path: &str, target: &str) -> String {
    if target.starts_with('/') {
        return split_paren_child_and_tranlatevalid(target);
    }
    let parent = match current_path.rfind('/') {
        Some(0) | None => "/",
        Some(pos) => &current_path[..pos],
    };
    let mut combined = String::new();
    if parent == "/" {
        combined.push('/');
        combined.push_str(target);
    } else {
        combined.push_str(parent);
        combined.push('/');
        combined.push_str(target);
    }
    split_paren_child_and_tranlatevalid(&combined)
}

fn read_file_follow<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    depth: usize,
) -> BlockDevResult<Option<Vec<u8>>> {
  
    if depth > 8 {
        return Err(BlockDevError::InvalidInput);
    }

    let mut inode = match get_file_inode(fs, device, path) {
        Ok(Some((_ino_num, ino))) => ino,
        Ok(None) => return Ok(None),
        Err(e) => return Err(e),
    };

    if inode.is_symlink() {
        let target_bytes = read_symlink_target(device, fs, &mut inode)?;
        let target = match core::str::from_utf8(&target_bytes) {
            Ok(s) => s,
            Err(_) => return Err(BlockDevError::Corrupted),
        };
        let resolved = resolve_symlink_path(path, target);
        return read_file_follow(device, fs, &resolved, depth + 1);
    }

    if !inode.is_file() {
        error!("Entry:{path} not aa file");
        return BlockDevResult::Err(BlockDevError::ReadError);
    }

    let size = inode.size() as usize;
    if size == 0 {
        return Ok(Some(Vec::new()));
    }

    let block_bytes = BLOCK_SIZE;
    let total_blocks = size.div_ceil(block_bytes);

    let mut buf = Vec::with_capacity(size);

    if inode.have_extend_header_and_use_extend() {
        let blocks = resolve_inode_block_allextend(fs, device, &mut inode)?;
        for &phys in blocks.values() {
            let cached = fs.datablock_cache.get_or_load(device, phys)?;
            let data = &cached.data[..block_bytes];
            buf.extend_from_slice(data);
            if buf.len() >= size {
                break;
            }
        }
    } else {
        for lbn in 0..total_blocks {
            let phys = match resolve_inode_block( device, &mut inode, lbn as u32)? {
                Some(b) => b,
                None => break,
            };

            let cached = fs.datablock_cache.get_or_load(device, phys as u64)?;
            let data = &cached.data[..block_bytes];
            buf.extend_from_slice(data);
        }
    }

    buf.truncate(size);

   

    Ok(Some(buf))
}

//mv
pub fn mv<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    old_path: &str,
    new_path: &str,
) -> BlockDevResult<()> {
    //找到对应entry，找不到就返回。
    //判断new_path的父目录是否已经存在不存在就返回，存在继续判断new_path是否有对应的entry，存在就返回
    //判断被移动的entry类型，如果是目录
    //对entry的父目录的link-1.
    //将旧entry使用insertnewentry插入到新目录修改文件名称，更新长度信息，使用removeentry...删除旧entry
    //对新父目录的link+1.
    //如果是文件或者链接
    //将旧entry使用insertnewentry插入到新目录修改文件名称，更新长度信息，使用removeentry...删除旧entry

    let old_norm = split_paren_child_and_tranlatevalid(old_path);
    let new_norm = split_paren_child_and_tranlatevalid(new_path);

    let (old_parent, old_name) = match old_norm.rfind('/') {
        Some(pos) => {
            let parent = if pos == 0 {
                "/".to_string()
            } else {
                old_norm[..pos].to_string()
            };
            let name = old_norm[pos + 1..].to_string();
            (parent, name)
        }
        None => {
            error!("mv invalid old_path(no '/'): old_path={}", old_path);
            return Err(BlockDevError::InvalidInput);
        }
    };
    let (new_parent, new_name) = match new_norm.rfind('/') {
        Some(pos) => {
            let parent = if pos == 0 {
                "/".to_string()
            } else {
                new_norm[..pos].to_string()
            };
            let name = new_norm[pos + 1..].to_string();
            (parent, name)
        }
        None => {
            error!("mv invalid new_path(no '/'): new_path={}", new_path);
            return Err(BlockDevError::InvalidInput);
        }
    };

    // 找到 old entry（inode + file_type），找不到就返回
    let (_old_pino, mut old_parent_inode) = match get_inode_with_num(fs, block_dev, &old_parent)
        .ok()
        .flatten()
    {
        Some(v) => v,
        None => {
            error!("mv old parent not found: old_path={} old_parent={}", old_path, old_parent);
            return Err(BlockDevError::InvalidInput);
        }
    };

    let mut src_ino: Option<u32> = None;
    let mut src_ft: Option<u8> = None;
    if let Ok(blocks) = resolve_inode_block_allextend(fs, block_dev, &mut old_parent_inode) {
        for phys in blocks {
            let cached = match fs.datablock_cache.get_or_load(block_dev, phys.1) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let data = &cached.data[..BLOCK_SIZE];
            let iter = DirEntryIterator::new(data);
            for (entry, _) in iter {
                if entry.inode == 0 {
                    continue;
                }
                if entry.name == old_name.as_bytes() {
                    src_ino = Some(entry.inode);
                    src_ft = Some(entry.file_type);
                    break;
                }
            }
            if src_ino.is_some() {
                break;
            }
        }
    }
    if src_ino.is_none() {
        // Non-extent directory: scan blocks using resolve_inode_block
        let total_size = old_parent_inode.size() as usize;
        let total_blocks = if total_size == 0 {
            0
        } else {
            total_size.div_ceil(BLOCK_SIZE)
        };
        for lbn in 0..total_blocks {
            let phys = match resolve_inode_block( block_dev, &mut old_parent_inode, lbn as u32) {
                Ok(Some(b)) => b,
                _ => continue,
            };
            let cached = match fs.datablock_cache.get_or_load(block_dev, phys as u64) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let data = &cached.data[..BLOCK_SIZE];
            let iter = DirEntryIterator::new(data);
            for (entry, _) in iter {
                if entry.inode == 0 {
                    continue;
                }
                if entry.name == old_name.as_bytes() {
                    src_ino = Some(entry.inode);
                    src_ft = Some(entry.file_type);
                    break;
                }
            }
            if src_ino.is_some() {
                break;
            }
        }
    }
    let src_ino = match src_ino {
        Some(v) => v,
        None => {
            error!(
                "mv source entry not found in old parent: old_path={} old_parent={} old_name={}",
                old_path, old_parent, old_name
            );
            return Err(BlockDevError::InvalidInput);
        }
    };
    let src_ft = src_ft.unwrap_or(Ext4DirEntry2::EXT4_FT_UNKNOWN);

    // new_parent 必须存在且是目录
    let (new_pino, new_parent_inode) = match get_inode_with_num(fs, block_dev, &new_parent)
        .ok()
        .flatten()
    {
        Some(v) => v,
        None => {
            error!("mv new parent not found: new_path={} new_parent={}", new_path, new_parent);
            return Err(BlockDevError::InvalidInput);
        }
    };
    if !new_parent_inode.is_dir() {
        error!("mv new parent is not dir: new_path={} new_parent={}", new_path, new_parent);
        return Err(BlockDevError::InvalidInput);
    }

    // new_path 已存在则返回
    if get_inode_with_num(fs, block_dev, &new_norm).ok().flatten().is_some() {
        error!("mv destination already exists: new_path={} new_norm={}", new_path, new_norm);
        return Err(BlockDevError::InvalidInput);
    }

    // old_path 不允许为根目录
    if old_norm == "/" {
        error!("mv refuses to move root: old_path={}", old_path);
        return Err(BlockDevError::InvalidInput);
    }

    // 插入新 entry 到 new_parent
    let mut new_parent_inode_copy = new_parent_inode;
    if insert_dir_entry(
        fs,
        block_dev,
        new_pino,
        &mut new_parent_inode_copy,
        src_ino,
        &new_name,
        src_ft,
    )
    .is_err()
    {
        error!(
            "mv insert_dir_entry failed: old_path={} new_path={} new_parent={} new_name={} src_ino={}",
            old_path,
            new_path,
            new_parent,
            new_name,
            src_ino
        );
        return Err(BlockDevError::WriteError);
    }

    // 删除旧 entry
    if !remove_inodeentry_from_parentdir(fs, block_dev, &old_parent, &old_name) {
        let _ = remove_inodeentry_from_parentdir(fs, block_dev, &new_parent, &new_name);
        error!(
            "mv remove old entry failed: old_parent={} old_name={} (rollback new_parent={} new_name={})",
            old_parent,
            old_name,
            new_parent,
            new_name
        );
        return Err(BlockDevError::WriteError);
    }

    // 目录跨父目录移动：更新 link 以及 '..'
    let mut moved_inode = match fs.get_inode_by_num(block_dev, src_ino) {
        Ok(v) => v,
        Err(e) => {
            error!("mv get_inode_by_num failed ino={} err={:?} ({})", src_ino, e, e);
            return Err(e);
        }
    };
    if moved_inode.is_dir() {
        // 父目录不同才需要改
        let old_pino = match get_inode_with_num(fs, block_dev, &old_parent)
            .ok()
            .flatten()
        {
            Some((n, _)) => n,
            None => {
                error!("mv old parent vanished while moving dir: old_parent={}", old_parent);
                return Err(BlockDevError::InvalidInput);
            }
        };
        if old_pino != new_pino {
            let _ = fs.modify_inode(block_dev, old_pino, |td| {
                td.i_links_count = td.i_links_count.saturating_sub(1);
            });
            let _ = fs.modify_inode(block_dev, new_pino, |td| {
                td.i_links_count = td.i_links_count.saturating_add(1);
            });

            // 更新被移动目录的 ".." 指向新父目录 inode
            let first_blk = match resolve_inode_block( block_dev, &mut moved_inode, 0) {
                Ok(Some(b)) => b,
                _ => {
                    error!("mv resolve_inode_block failed for moved dir ino={}", src_ino);
                    return Err(BlockDevError::Corrupted);
                }
            };
            let _ = fs
                .datablock_cache
                .modify(block_dev, first_blk as u64, |data| {
                    let block_bytes = BLOCK_SIZE;
                    if block_bytes < 24 {
                        return;
                    }
                    // '.' entry at offset 0
                    let rec_len0 = u16::from_le_bytes([data[4], data[5]]) as usize;
                    if rec_len0 == 0 || rec_len0 + 8 > block_bytes {
                        return;
                    }
                    let off1 = rec_len0;
                    if off1 + 4 > block_bytes {
                        return;
                    }
                    let bytes = new_pino.to_le_bytes();
                    data[off1] = bytes[0];
                    data[off1 + 1] = bytes[1];
                    data[off1 + 2] = bytes[2];
                    data[off1 + 3] = bytes[3];
                });
        }
    }

    Ok(())
}

///UnLink
pub fn unlink<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    link_path: &str,
) {
    //首先逐级扫描entry找到对应linkentry。
    let norm_path = split_paren_child_and_tranlatevalid(link_path);
    let (parent_path, child_name) = if let Some(pos) = norm_path.rfind('/') {
        let parent = if pos == 0 {
            "/".to_string()
        } else {
            norm_path[..pos].to_string()
        };
        let child = norm_path[pos + 1..].to_string();
        (parent, child)
    } else {
        ("/".to_string(), norm_path)
    };

    let (_pino, mut parent_inode) = match get_inode_with_num(fs, block_dev, &parent_path)
        .ok()
        .flatten()
    {
        Some(v) => v,
        None => {
            warn!("Parent directory not found, unlink failed: {parent_path}");
            return;
        }
    };

    let mut target_ino: Option<u32> = None;
    let blocks = match resolve_inode_block_allextend(fs, block_dev, &mut parent_inode) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Parse parent dir blocks failed, unlink failed: {e:?} parent={parent_path}"
            );
            return;
        }
    };

    for &phys in blocks.values() {
        let cached = match fs.datablock_cache.get_or_load(block_dev, phys) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let data = &cached.data[..BLOCK_SIZE];
        let iter = DirEntryIterator::new(data);
        for (entry, _) in iter {
            if entry.inode == 0 {
                continue;
            }
            if entry.name == child_name.as_bytes() {
                target_ino = Some(entry.inode);
                break;
            }
        }
        if target_ino.is_some() {
            break;
        }
    }

    let target_ino = match target_ino {
        Some(v) => v,
        None => {
            warn!("Link entry not found, unlink failed: {link_path}");
            return;
        }
    };

    let mut target_inode = match fs.get_inode_by_num(block_dev, target_ino) {
        Ok(v) => v,
        Err(e) => {
            warn!("get inode {target_ino} failed, unlink failed: {e:?}");
            return;
        }
    };

    //首先对指向inode 的link -1。
    let new_links = target_inode.i_links_count.saturating_sub(1);
    target_inode.i_links_count = new_links;
    if fs
        .modify_inode(block_dev, target_ino, |td| {
            td.i_links_count = new_links;
        })
        .is_err()
    {
        warn!("modify inode {target_ino} links_count failed in unlink");
        return;
    }

    //如果此时link数为0就调用deletefile删除对应文件.   这里不复用deletefile，因为需要额外的定位
    if new_links == 0 {
        let mut used_blocks: Vec<u64> =
            match resolve_inode_block_allextend(fs, block_dev, &mut target_inode) {
                Ok(v) => v.into_values().collect(),
                Err(e) => {
                    warn!("Parse inode blocks failed (unlink free): {e:?}");
                    return;
                }
            };
        used_blocks.sort();
        for blk in used_blocks {
            if let Err(e) = fs.free_block(block_dev, blk) {
                warn!("free_block failed for blk {blk}: {e:?}");
                return;
            }
        }
        if let Err(e) = fs.free_inode(block_dev, target_ino) {
            warn!("free_inode failed for inode {target_ino}: {e:?}");
            return;
        }
        let _ = fs.modify_inode(block_dev, target_ino, |td| {
            td.i_dtime = u32::MAX;
        });
    }

    //最后调用removeentryfromparent移除entry
    let removed = remove_inodeentry_from_parentdir(fs, block_dev, &parent_path, &child_name);
    if !removed {
        warn!(
            "Dir entry '{child_name}' not found under parent {parent_path} in unlink"
        );
    }
}

///Link
pub fn link<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    link_path: &str,
    linked_path: &str,
) {
    let link_norm = split_paren_child_and_tranlatevalid(link_path);
    let linked_norm = split_paren_child_and_tranlatevalid(linked_path);

    // 1.检查 被链接文件本身是否存在，不存在返回。
    let (target_ino, target_inode) = match get_file_inode(fs, block_dev, &linked_norm) {
        Ok(Some(v)) => v,
        _ => return,
    };

    // 1.5 不允许链接目录
    if target_inode.is_dir() {
        return;
    }

    // 2.检查链接文件本身是否已经存在同名entry，存在返回
    if get_file_inode(fs, block_dev, &link_norm)
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }

    // link_path 的父目录必须存在且是目录
    let (parent_path, child_name) = if let Some(pos) = link_norm.rfind('/') {
        let parent = if pos == 0 {
            "/".to_string()
        } else {
            link_norm[..pos].to_string()
        };
        let child = link_norm[pos + 1..].to_string();
        (parent, child)
    } else {
        ("/".to_string(), link_norm)
    };
    let (parent_ino, mut parent_inode) = match get_inode_with_num(fs, block_dev, &parent_path)
        .ok()
        .flatten()
    {
        Some(v) => v,
        None => return,
    };
    if !parent_inode.is_dir() {
        return;
    }

    // 3.复制目标entry（主要复制 file_type），插入到当前父目录（新名字）
    let (linked_parent_path, linked_child_name) = if let Some(pos) = linked_norm.rfind('/') {
        let parent = if pos == 0 {
            "/".to_string()
        } else {
            linked_norm[..pos].to_string()
        };
        let child = linked_norm[pos + 1..].to_string();
        (parent, child)
    } else {
        ("/".to_string(), linked_norm.clone())
    };

    let mut copied_ft: Option<u8> = None;
    if let Some((_lpino, mut lp_inode)) = get_inode_with_num(fs, block_dev, &linked_parent_path)
        .ok()
        .flatten()
        && let Ok(blocks) = resolve_inode_block_allextend(fs, block_dev, &mut lp_inode) {
            for &phys in blocks.values() {
                let cached = match fs.datablock_cache.get_or_load(block_dev, phys) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let data = &cached.data[..BLOCK_SIZE];
                let iter = DirEntryIterator::new(data);
                for (entry, _) in iter {
                    if entry.inode == 0 {
                        continue;
                    }
                    if entry.name == linked_child_name.as_bytes() {
                        copied_ft = Some(entry.file_type);
                        break;
                    }
                }
                if copied_ft.is_some() {
                    break;
                }
            }
        }

    let file_type = copied_ft.unwrap_or_else(|| {
        if target_inode.is_file() {
            Ext4DirEntry2::EXT4_FT_REG_FILE
        } else if target_inode.is_symlink() {
            Ext4DirEntry2::EXT4_FT_SYMLINK
        } else {
            Ext4DirEntry2::EXT4_FT_UNKNOWN
        }
    });

    // insert_dir_entry 会根据 child_name 重新计算 name_len/rec_len（满足“更新名字和长度信息”）
    if insert_dir_entry(
        fs,
        block_dev,
        parent_ino,
        &mut parent_inode,
        target_ino,
        &child_name,
        file_type,
    )
    .is_err()
    {
        return;
    }

    // 4.更新目标inode的link+1，失败则回滚刚插入的目录项
    if fs
        .modify_inode(block_dev, target_ino, |td| {
            td.i_links_count = td.i_links_count.saturating_add(1);
        })
        .is_err()
    {
        let _ = remove_inodeentry_from_parentdir(fs, block_dev, &parent_path, &child_name);
    }
}

pub fn remove_inodeentry_from_parentdir<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    parent_path: &str,
    child_name: &str,
) -> bool {
    let parent_info = match get_inode_with_num(fs, block_dev, parent_path)
        .ok()
        .flatten()
    {
        Some(v) => v,
        None => {
            warn!(
                "Parent directory not found for path {parent_path}, remove entry failed"
            );
            return false;
        }
    };
    let (_parent_ino_num, mut parent_inode) = parent_info;

    let total_size = parent_inode.size() as usize;
    let block_bytes = BLOCK_SIZE;
    let total_blocks = if total_size == 0 {
        0
    } else {
        total_size.div_ceil(block_bytes)
    };

    let mut removed = false;
    let name_bytes = child_name.as_bytes();

    for lbn in 0..total_blocks {
        if removed {
            break;
        }
        let phys = match resolve_inode_block( block_dev, &mut parent_inode, lbn as u32) {
            Ok(Some(b)) => b,
            _ => continue,
        };
        let _ = fs.datablock_cache.modify(block_dev, phys as u64, |data| {
            if removed {
                return;
            }
            let mut offset: usize = 0;
            let mut prev_off: Option<usize> = None;
            let mut prev_rec_len: u16 = 0;
            while offset + 8 <= block_bytes {
                let inode = u32::from_le_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]);
                let rec_len = u16::from_le_bytes([data[offset + 4], data[offset + 5]]);
                if rec_len < 8 {
                    break;
                }
                let name_len = data[offset + 6] as usize;
                let entry_end = offset + rec_len as usize;
                if entry_end > block_bytes {
                    break;
                }

                // Only compare name bytes within the current entry's rec_len.
                if name_len > 0 && offset + 8 + name_len <= entry_end {
                    let name = &data[offset + 8..offset + 8 + name_len];
                    if inode != 0 && name == name_bytes {
                        if let Some(poff) = prev_off {
                            // Merge current entry's space into previous entry.
                            let new_len = prev_rec_len.saturating_add(rec_len);
                            let bytes = new_len.to_le_bytes();
                            data[poff + 4] = bytes[0];
                            data[poff + 5] = bytes[1];

                            // Clear current entry inode so it will be treated as free.
                            let zero = 0u32.to_le_bytes();
                            data[offset] = zero[0];
                            data[offset + 1] = zero[1];
                            data[offset + 2] = zero[2];
                            data[offset + 3] = zero[3];
                        } else {
                            // No previous entry in this block: mark this entry free.
                            let zero = 0u32.to_le_bytes();
                            data[offset] = zero[0];
                            data[offset + 1] = zero[1];
                            data[offset + 2] = zero[2];
                            data[offset + 3] = zero[3];
                        }
                        removed = true;
                        break;
                    }
                }
                if entry_end >= block_bytes {
                    break;
                }
                prev_off = Some(offset);
                prev_rec_len = rec_len;
                offset = entry_end;
            }
        });
    }

    removed
}

///删除目录
pub fn delete_dir<B: BlockDevice>(fs: &mut Ext4FileSystem, block_dev: &mut Jbd2Dev<B>, path: &str) {
    #[derive(Clone)]
    struct DirFrame {
        path: alloc::string::String,
        ino_num: u32,
        inode: Ext4Inode,
        parent_path: Option<alloc::string::String>,
        name_in_parent: Option<alloc::string::String>,
        stage: u8, // 0=scan, 1=cleanup
    }

    let norm_path = split_paren_child_and_tranlatevalid(path);
    let (root_ino_num, root_inode) = match get_file_inode(fs, block_dev, &norm_path) {
        Ok(Some(v)) => v,
        Ok(None) => {
            warn!("Dir not exist, delete failed!");
            return;
        }
        Err(e) => {
            warn!("Dir lookup error, delete failed: {e:?}");
            return;
        }
    };
    if !root_inode.is_dir() {
        error!("path:{path} is not a dir!");
        return;
    }

    let (parent_path, child_name) = if norm_path == "/" {
        (None, None)
    } else if let Some(pos) = norm_path.rfind('/') {
        let parent = if pos == 0 {
            "/".to_string()
        } else {
            norm_path[..pos].to_string()
        };
        let child = norm_path[pos + 1..].to_string();
        (Some(parent), Some(child))
    } else {
        (Some("/".to_string()), Some(norm_path.clone()))
    };

    let mut stack: Vec<DirFrame> = Vec::new();
    stack.push(DirFrame {
        path: norm_path,
        ino_num: root_ino_num,
        inode: root_inode,
        parent_path,
        name_in_parent: child_name,
        stage: 0,
    });

    // 算法采用while显式栈实现。
    while let Some(mut frame) = stack.pop() {
        // 1.首先遍历对应目录块。DirEntryIterator遍历所有entry（跳过. ..）。
        if frame.stage == 0 {
            let block_bytes = BLOCK_SIZE;

            let dir_blocks =
                match resolve_inode_block_allextend(fs, block_dev, &mut frame.inode) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("Parse dir blocks failed: {:?} path={}", e, frame.path);
                        return;
                    }
                };

            let mut to_descend: Vec<(
                alloc::string::String,
                u32,
                Ext4Inode,
                alloc::string::String,
            )> = Vec::new();

            for &phys in dir_blocks.values() {
                // 先收集 entry，避免在持有 datablock_cache 借用时再次可变借用 fs
                let mut child_entries: Vec<(u32, alloc::string::String)> = Vec::new();
                {
                    let cached = match fs.datablock_cache.get_or_load(block_dev, phys) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(
                                "load dir block {} failed: {:?} path={}",
                                phys, e, frame.path
                            );
                            return;
                        }
                    };
                    let data = &cached.data[..block_bytes];
                    let iter = DirEntryIterator::new(data);
                    for (entry, _) in iter {
                        if entry.is_dot() || entry.is_dotdot() {
                            continue;
                        }
                        let child_name_bytes = entry.name.to_vec();
                        let child_name_str = match core::str::from_utf8(&child_name_bytes) {
                            Ok(s) => s,
                            Err(_) => {
                                warn!("invalid child name utf8 under dir {}", frame.path);
                                continue;
                            }
                        };
                        child_entries.push((entry.inode, child_name_str.to_string()));
                    }
                }

                for (child_ino, child_name) in child_entries {
                    let child_path = if frame.path == "/" {
                        alloc::format!("/{child_name}")
                    } else {
                        alloc::format!("{}/{}", frame.path, child_name)
                    };

                    // 每次扫描到的entry把entry的path 用error输出。
                    debug!("scan entry path={child_path}");

                    // 2.判断entry类型。
                    let child_inode = match fs.get_inode_by_num(block_dev, child_ino) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(
                                "get child inode {child_ino} failed: {e:?} path={child_path}"
                            );
                            continue;
                        }
                    };

                    // 是普通文件或者是链接，调用deletefile删除对应文件。
                    if !child_inode.is_dir() {
                        delete_file(fs, block_dev, &child_path);
                        continue;
                    }

                    // 是dir类型就更新父目录的inode链接数-1 然后继续深入这个目录（跳过. ..）。
                    let _ = fs.modify_inode(block_dev, frame.ino_num, |td| {
                        td.i_links_count = td.i_links_count.saturating_sub(1);
                    });

                    to_descend.push((child_path, child_ino, child_inode, child_name));
                }
            }

            // 深度优先：反向压栈
            let parent_path_for_children = frame.path.clone();

            frame.stage = 1;
            stack.push(frame);

            for (child_path, child_ino, child_inode, child_name) in to_descend.into_iter().rev() {
                stack.push(DirFrame {
                    path: child_path,
                    ino_num: child_ino,
                    inode: child_inode,
                    parent_path: Some(parent_path_for_children.clone()),
                    name_in_parent: Some(child_name),
                    stage: 0,
                });
            }
            continue;
        }

        // 当深入的目录为空时（只剩下.和..）返回上一级
        let mut cur_inode = match fs.get_inode_by_num(block_dev, frame.ino_num) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "get inode {} failed in cleanup: {:?} path={}",
                    frame.ino_num, e, frame.path
                );
                return;
            }
        };

        // 如果此时的dir类型的entrylinks数不是2就warn发出警告然后继续
        if cur_inode.i_links_count != 2 {
            warn!(
                "dir inode links_count != 2 (links={}) path={} ino={}",
                cur_inode.i_links_count, frame.path, frame.ino_num
            );
        }

        // 调用函数从父目录删除这条entry。
        if let (Some(pp), Some(name)) = (&frame.parent_path, &frame.name_in_parent) {
            let removed_path = if pp == "/" {
                alloc::format!("/{name}")
            } else {
                alloc::format!("{pp}/{name}")
            };
            // 删除entry时一样。
            debug!("delete entry path={removed_path}");

            let removed = remove_inodeentry_from_parentdir(fs, block_dev, pp, name);
            if !removed {
                warn!(
                    "Dir entry '{}' not found under parent {} (path={})",
                    name, pp, frame.path
                );
                return;
            }

            if let Some((pino, _)) = get_inode_with_num(fs, block_dev, pp).ok().flatten() {
                let _ = fs.modify_inode(block_dev, pino, |td| {
                    td.i_links_count = td.i_links_count.saturating_sub(1);
                });
            }
        }

        // 然后仿照deletefile的逻辑释放entry对应的inode的blocks和inode。
        let used_blocks: Vec<u64> =
            match resolve_inode_block_allextend(fs, block_dev, &mut cur_inode) {
                Ok(v) => v.into_values().collect(),
                Err(e) => {
                    warn!(
                        "Parse dir blocks failed (freeing): {:?} path={}",
                        e, frame.path
                    );
                    return;
                }
            };

        for blk in used_blocks {
            if let Err(e) = fs.free_block(block_dev, blk) {
                warn!(
                    "free_block failed for blk {}: {:?} path={}",
                    blk, e, frame.path
                );
                return;
            }
        }
        if let Err(e) = fs.free_inode(block_dev, frame.ino_num) {
            warn!(
                "free_inode failed for inode {}: {:?} path={}",
                frame.ino_num, e, frame.path
            );
            return;
        }

        // 最后更新块组的dir计数-1。
        let (group_idx, _idx_in_group) = fs.inode_allocator.global_to_group(frame.ino_num);
        if let Some(desc) = fs.get_group_desc_mut(group_idx) {
            let before = desc.used_dirs_count();
            let new_count = before.saturating_sub(1);
            desc.bg_used_dirs_count_lo = (new_count & 0xFFFF) as u16;
            desc.bg_used_dirs_count_hi = (new_count >> 16) as u16;
        }
    }
}

///删除文件/删除链接文件
pub fn delete_file<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    path: &str,
) {
    //find inode
    let norm_path = split_paren_child_and_tranlatevalid(path);
    let target = match get_file_inode(fs, block_dev, &norm_path) {
        Ok(Some((ino_num, inode))) => (ino_num, inode),
        Ok(None) => {
            warn!("File not exist, delete failed!");
            return;
        }
        Err(e) => {
            warn!("File lookup error, delete failed: {e:?}");
            return;
        }
    };
    let (ino_num, mut target_inode) = target;

    if target_inode.is_dir() {
        error!("file:{path} is a dir!");
        return;
    }

    //统计block（i_blocks 以 512 字节为单位，换算成数据块个数）
    let mut inode_used_blocks: Vec<u64> =
        resolve_inode_block_allextend(fs, block_dev, &mut target_inode)
            .expect("Parse inode extend failed")
            .into_values()
            .collect();
    inode_used_blocks.sort(); //排序block
    //link-1
    target_inode.i_links_count = target_inode.i_links_count.saturating_sub(1);
    //update target inode link
    if fs
        .modify_inode(block_dev, ino_num, |td| {
            td.i_links_count = target_inode.i_links_count;
        })
        .is_err()
    {
        error!("inode num:{ino_num} path:{path} modify faild!")
    }
    if target_inode.i_links_count == 0 {
        debug!("Will free inode:{ino_num} path:{path}");
        //设置dtime(删除时的时间戳) 太小会触发PR_1_LOW_DTIME问题，inode存在并且正常使用时应该为0.

        //释放inode所有的datablock
        for blk in inode_used_blocks {
            if let Err(e) = fs.free_block(block_dev, blk) {
                warn!("free_block failed for blk {blk}: {e:?}");
                return;
            }
        }
        //释放inode
        if let Err(e) = fs.free_inode(block_dev, ino_num) {
            warn!("free_inode failed for inode {ino_num}: {e:?}");
            return;
        }
    } else {
        error!(
            "Inode num:{} links:{} >0 ,only remove entry!",
            ino_num, target_inode.i_links_count
        );
    }

    // 计算父目录路径和子名
    let (parent_path, child_name) = if let Some(pos) = norm_path.rfind('/') {
        let parent = if pos == 0 {
            "/".to_string()
        } else {
            norm_path[..pos].to_string()
        };
        let child = norm_path[pos + 1..].to_string();
        (parent, child)
    } else {
        ("/".to_string(), norm_path)
    };

    // 查找父目录 inode
    let removed = remove_inodeentry_from_parentdir(fs, block_dev, &parent_path, &child_name);
    if !removed {
        warn!(
            "Dir entry '{child_name}' not found under parent {parent_path}, but inode/data already freed"
        );
    }
}

/// 根据数据块列表为普通文件 inode 构建块映射：
/// - 否则使用传统直接块指针（i_block[0..]）。
pub fn build_file_block_mapping<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    inode: &mut Ext4Inode,
    data_blocks: &[u64],
    block_dev: &mut Jbd2Dev<B>,
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
        if !inode.have_extend_header_and_use_extend() {
            inode.i_flags |=Ext4Inode::EXT4_EXTENTS_FL;
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
            tree.insert_extent(fs, extend, block_dev).expect("Extend insert Failed!");
        }
    } else {
        error!("not support tranditional block pointer");
    }
}

///创建文件类型entry通用接口
/// 传入文件名称,可选初始数据
pub fn mkfile<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    initial_data: Option<&[u8]>,
) -> Option<Ext4Inode> {
    mkfile_with_ino(device, fs, path, initial_data).map(|(_, inode)| inode)
}

pub fn mkfile_with_ino<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    initial_data: Option<&[u8]>,
) -> Option<(u32, Ext4Inode)> {
    // 规范化路径
    let norm_path = split_paren_child_and_tranlatevalid(path);

    // 如果目标已存在，直接返回
    if let Ok(Some((_ino_num, inode))) = get_file_inode(fs, device, &norm_path) {
        let ino = match get_inode_with_num(fs, device, &norm_path).ok().flatten() {
            Some((ino, _)) => ino,
            None => {
                error!("mkfile_with_ino existing file but failed to get ino path={}", path);
                return None;
            }
        };
        return Some((ino, inode));
    }

    // 拆 parent / child
    let mut valid_path = norm_path;
    let split_point = match valid_path.rfind('/') {
        Some(v) => v,
        None => {
            error!("mkfile invalid path(no '/'): path={}", path);
            return None;
        }
    };
    let child = valid_path.split_off(split_point)[1..].to_string();
    let parent = valid_path;

    // 确保父目录存在
    if mkdir(device, fs, &parent).is_none() {
        error!("mkfile mkdir parent failed path={} parent={}", path, parent);
        return None;
    }

    // 重新获取父目录 inode 及其 inode 号
    let (parent_ino_num, parent_inode) =
        match get_inode_with_num(fs, device, &parent).ok().flatten() {
            Some((n, ino)) => (n, ino),
            None => {
                error!("mkfile get parent inode failed path={} parent={}", path, parent);
                return None;
            }
        };

    //为新文件分配 inode（内部自动选择块组）
    let new_file_ino = match fs.alloc_inode(device) {
        Ok(ino) => ino,
        Err(e) => {
            error!("mkfile alloc_inode failed path={} err={:?} ({})", path, e, e);
            return None;
        }
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
                Err(e) => {
                    error!("mkfile alloc_block failed path={} err={:?} ({})", path, e, e);
                    break;
                }
            };

            let write_len = core::cmp::min(remaining, BLOCK_SIZE);

            // 将数据写入新分配的数据块，其余部分填零
            fs.datablock_cache.modify_new(blk, |data| {
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

    //extend是否开启
    if fs.superblock.has_extents() {
        new_inode.write_extend_header();
    }

    new_inode.i_links_count = 1;

    let size_lo = (total_written & 0xffffffff) as u32;
    let size_hi = ((total_written as u64) >> 32) as u32;

    if !data_blocks.is_empty() {
        // 有初始数据：多块或单块文件
        let used_databyte = data_blocks.len() as u64;
        let iblocks_used = used_databyte.saturating_mul(BLOCK_SIZE as u64 / 512) as u64;
        let used_blocks_lo = iblocks_used as u32;
        //let used_blocks_hi = (iblocks_used as u64 >> 32) as u16;
        new_inode.i_size_lo = size_lo;
        new_inode.i_size_high = size_hi;
        new_inode.i_blocks_lo = used_blocks_lo;
        new_inode.l_i_blocks_high = (iblocks_used as u64 >> 32) as u16;

        build_file_block_mapping(fs, &mut new_inode, &data_blocks, device);
    } else {
        //无初始数据：空文件
        new_inode.i_size_lo = 0;
        new_inode.i_size_high = 0;
        new_inode.i_blocks_lo = 0;
        new_inode.l_i_blocks_high = 0;
        if fs.superblock.has_extents() {
            new_inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
            new_inode.write_extend_header();
        } else {
            new_inode.i_block = [0; 15];
        }
    }

    if fs
        .modify_inode(device, new_file_ino, |on_disk| {
            *on_disk = new_inode;
        })
        .is_err()
    {
        error!("mkfile modify_inode failed path={} ino={}", path, new_file_ino);
        return None;
    }

    //在父目录中插入一个普通文件类型的目录项（必要时自动扩展目录块）
    let mut parent_inode_copy = parent_inode;
    if insert_dir_entry(
        fs,
        device,
        parent_ino_num,
        &mut parent_inode_copy,
        new_file_ino,
        &child,
        Ext4DirEntry2::EXT4_FT_REG_FILE,
    )
    .is_err()
    {
        error!(
            "mkfile insert_dir_entry failed path={} parent_ino={} child={} ino={}",
            path,
            parent_ino_num,
            child,
            new_file_ino
        );
        return None;
    }

    // 返回新文件 inode
    match fs.get_inode_by_num(device, new_file_ino) {
        Ok(inode) => Some((new_file_ino, inode)),
        Err(e) => {
            error!(
                "mkfile get_inode_by_num failed path={} ino={} err={:?} ({})",
                path,
                new_file_ino,
                e,
                e
            );
            None
        }
    }
}

///读取指定路径的整个文件内容
pub fn read_file<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> BlockDevResult<Option<Vec<u8>>> {
    read_file_follow(device, fs, path, 0)
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
    let (inode_num, _inode) = info;

    write_file_with_ino(device, fs, inode_num, offset, data)
}

pub fn write_file_with_ino<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    inode_num: u32,
    offset: usize,
    data: &[u8],
) -> BlockDevResult<()> {
    if data.is_empty() {
        return Ok(());
    }

    let mut inode = fs.get_inode_by_num(device, inode_num)?;

    let old_size = inode.size() as usize;
    let block_bytes = BLOCK_SIZE;

    // If extents are supported, make sure the inode has a valid extent header
    // before any extent-based operations. Some inodes may have EXTENTS flag set
    // but the on-disk header is missing/invalid.
    if fs.superblock.has_extents() {
        if !inode.have_extend_header_and_use_extend() {
            inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
            inode.write_extend_header();
        }
    }

    if offset > old_size {
        info!("Expend write!");
    }

    let end = offset.saturating_add(data.len());

    let start_lbn = offset / block_bytes;
    let end_lbn = (end - 1) / block_bytes;

    // Extent files may be sparse. For writes that cross holes, allocate blocks on-demand.
    if end > old_size {
        if !fs.superblock.has_extents() || !inode.have_extend_header_and_use_extend() {
            // 只在 extent 模式下支持扩展
            return Err(BlockDevError::Unsupported);
        }
    }

    let mut blocks_map = if inode.have_extend_header_and_use_extend() {
        Some(resolve_inode_block_allextend(fs, device, &mut inode)?)
    } else {
        None
    };

    for lbn in start_lbn..=end_lbn {
        let phys = if inode.have_extend_header_and_use_extend() {
            let map = blocks_map.as_mut().ok_or(BlockDevError::Corrupted)?;
            if let Some(&b) = map.get(&(lbn as u32)) {
                b
            } else {
                // Hole: allocate a new block and insert an extent for this single LBN.
                let new_phys = fs.alloc_block(device)?;
                fs.datablock_cache.modify_new(new_phys, |blk| {
                    for b in blk.iter_mut() {
                        *b = 0;
                    }
                });
                {
                    let mut tree = ExtentTree::new(&mut inode);
                    let ext = Ext4Extent::new(lbn as u32, new_phys, 1);
                    tree.insert_extent(fs, ext, device)?;
                }
                map.insert(lbn as u32, new_phys);

                let add_iblocks = (BLOCK_SIZE / 512) as u32;
                inode.i_blocks_lo = inode.i_blocks_lo.saturating_add(add_iblocks);

                new_phys
            }
        } else {
            match resolve_inode_block(device, &mut inode, lbn as u32)? {
                Some(b) => b as u64,
                None => return Err(BlockDevError::Unsupported),
            }
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

            blk[dst_off..dst_off + len].copy_from_slice(&data[src_off..src_off + len]);
        })?;
    }

    if end > old_size {
        inode.i_size_lo = (end as u64 & 0xffff_ffff) as u32;
        inode.i_size_high = ((end as u64) >> 32) as u32;

    }

    fs.modify_inode(device, inode_num, |td| {
        *td = inode;
    })?;

    Ok(())
}
