//创建文件夹功能模块


use crate::alloc::string::ToString;
use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::endian::*;
use crate::ext4_backend::entries::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::extents_tree::*;
use crate::ext4_backend::file::*;
use crate::ext4_backend::loopfile::*;
use crate::ext4_backend::error::*;
use alloc::string::String;
use alloc::vec::Vec;
use log::error;
use log::debug;

#[derive(Debug)]
pub enum FileError {
    DirExist,
    FileExist,
    DirNotFound,
    FileNotFound,
}

///合法化路径：去掉重复的 '/'
pub fn split_paren_child_and_tranlatevalid(pat: &str) -> String {
    //去掉重复///类型和中间空路径
    let mut last_c = '\0';
    let mut result_s = String::new();
    for ch in pat.chars() {
        if ch == '/' && last_c == '/' {
            continue;
        }
        result_s.push(ch);
        last_c = ch;
    }
    // 去掉末尾多余的 '/'，但保留单独的根"/"
    while result_s.len() > 1 && result_s.ends_with('/') {
        result_s.pop();
    }

    result_s
}

/// 路径解析，返回 (inode_num, inode)
pub fn get_inode_with_num<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    path: &str,
) -> BlockDevResult<Option<(u32, Ext4Inode)>> {
    // 根目录特殊处理
    if path.is_empty() || path == "/" {
        let inode = fs.get_root(device)?;
        return Ok(Some((fs.root_inode, inode)));
    }

    // 按 '/' 分割
    let components = path.split('/').filter(|s| !s.is_empty());

    // 从根开始
    let mut current_inode = fs.get_root(device)?;
    let mut current_ino: u32 = fs.root_inode;

    for name in components {
        if !current_inode.is_dir() {
            return Ok(None);
        }

        if name == "." {
            continue;
        }
        if name == ".." {
            // 这里只处理简单情况：根的父仍为根
            if current_ino == fs.root_inode {
                continue;
            }
            // 更完整的 ".." 解析可以在后续扩展
            continue;
        }

        let target = name.as_bytes();

        let total_size = current_inode.size() as usize;
        let block_bytes = BLOCK_SIZE;
        let total_blocks = if total_size == 0 {
            0
        } else {
            total_size.div_ceil(block_bytes)
        };

        let mut found_inode_num: Option<u64> = None;

        for lbn in 0..total_blocks {
            let phys = match resolve_inode_block( device, &mut current_inode, lbn as u32)? {
                Some(b) => b,
                None => continue,
            };

            let cached_block = fs.datablock_cache.get_or_load(device, phys as u64)?;
            let block_data = &cached_block.data[..block_bytes];

            if let Some(entry) = classic_dir::find_entry(block_data, target) {
                found_inode_num = Some(entry.inode as u64);
                break;
            }
        }

        let inode_num = match found_inode_num {
            Some(n) => n,
            None => return Ok(None),
        };

        let (inode_group_idx, _idx_in_group) = fs.inode_allocator.global_to_group(inode_num as u32);
        let inode_table_start = fs
            .group_descs
            .get(inode_group_idx as usize)
            .ok_or(BlockDevError::Corrupted)?
            .inode_table();

        let (block_num, offset, _group_idx) = fs.inodetable_cahce.calc_inode_location(
            inode_num as u32,
            fs.superblock.s_inodes_per_group,
            inode_table_start,
            BLOCK_SIZE,
        );

        let cached_inode = fs
            .inodetable_cahce
            .get_or_load(device, inode_num, block_num, offset)?;
        current_inode = cached_inode.inode;
        current_ino = inode_num as u32;
    }

    Ok(Some((current_ino, current_inode)))
}

/// 在父目录的所有逻辑块中查找空闲空间并插入一个目录项；
/// 若所有现有块都无法容纳，则自动为目录分配一个新数据块并扩展 inode 映射和大小。
pub fn insert_dir_entry<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    parent_ino_num: u32,
    parent_inode: &mut Ext4Inode,
    child_ino: u32,
    child_name: &str,
    file_type: u8,
) -> BlockDevResult<()> {
    let name_bytes = child_name.as_bytes();
    let name_len = core::cmp::min(name_bytes.len(), Ext4DirEntry2::MAX_NAME_LEN as usize);
    let new_rec_len = Ext4DirEntry2::entry_len(name_len as u8) as usize;
    let new_entry = Ext4DirEntry2::new(
        child_ino,
        Ext4DirEntry2::entry_len(name_len as u8),
        file_type,
        &name_bytes[..name_len],
    );

    let total_size = parent_inode.size() as usize;
    let block_bytes = BLOCK_SIZE;
    let total_blocks = if total_size == 0 {
        0
    } else {
        total_size.div_ceil(block_bytes)
    };

    let mut inserted = false;

    let blocks = resolve_inode_block_allextend(fs, device, parent_inode)?;

    for lbn in 0..total_blocks {
        if inserted {
            break;
        }

        let phys = match blocks.get(&(lbn as u32)) {
            Some(&b) => b,
            None => {
                error!(
                    "insert_dir_entry: missing extent mapping for parent_ino={} lbn={} name={}",
                    parent_ino_num, lbn, child_name
                );
                return Err(BlockDevError::Corrupted);
            }
        };

        let _ = fs.datablock_cache.modify(device, phys as u64, |data| {
            if inserted {
                return;
            }

            let block_bytes = BLOCK_SIZE;

            let mut offset = 0usize;
            while offset + 8 <= block_bytes {
                let inode = u32::from_le_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]);
                let rec_len = u16::from_le_bytes([data[offset + 4], data[offset + 5]]) as usize;
                if rec_len < 8 {
                    return;
                }
                let entry_end = offset + rec_len;
                if entry_end > block_bytes {
                    return;
                }

                // Free entry: directly use it if it can hold the new entry.
                if inode == 0 {
                    if rec_len >= new_rec_len {
                        let mut full_entry = new_entry;
                        full_entry.rec_len = rec_len as u16;
                        full_entry.to_disk_bytes(&mut data[offset..offset + 8]);
                        let nlen = full_entry.name_len as usize;
                        data[offset + 8..offset + 8 + nlen]
                            .copy_from_slice(&full_entry.name[..nlen]);
                        inserted = true;
                    }
                    return;
                }

                // Occupied entry: try to split tail space.
                let cur_name_len = data[offset + 6] as usize;
                let mut ideal = 8 + cur_name_len;
                ideal = (ideal + 3) & !3;
                if ideal <= rec_len {
                    let tail = rec_len - ideal;
                    if tail >= new_rec_len {
                        let ideal_bytes = (ideal as u16).to_le_bytes();
                        data[offset + 4] = ideal_bytes[0];
                        data[offset + 5] = ideal_bytes[1];

                        let new_off = offset + ideal;
                        let mut full_entry = new_entry;
                        full_entry.rec_len = tail as u16;
                        full_entry.to_disk_bytes(&mut data[new_off..new_off + 8]);
                        let nlen = full_entry.name_len as usize;
                        data[new_off + 8..new_off + 8 + nlen]
                            .copy_from_slice(&full_entry.name[..nlen]);
                        inserted = true;
                        return;
                    }
                }

                if entry_end == block_bytes {
                    return;
                }
                offset = entry_end;
            }
        });
    }

    if inserted {
        return Ok(());
    }

    // 所有现有逻辑块都无法容纳新目录项：为目录分配一个新数据块，并扩展 inode 映射
    let new_block = fs.alloc_block(device)?;

    // 更新 parent_inode 的块映射（extent 或直接块）和大小统计
    let block_bytes = BLOCK_SIZE;
    let old_blocks = if total_size == 0 {
        0
    } else {
        total_size.div_ceil(block_bytes)
    };
    let new_lbn = old_blocks as u32; // 新块对应的逻辑块号

    if fs.superblock.has_extents() && parent_inode.have_extend_header_and_use_extend() {
        // extent 目录：通过 ExtentTree 追加一个长度为 1 的 extent
        let new_ext = Ext4Extent::new(new_lbn, new_block, 1);
        let mut tree = ExtentTree::new(parent_inode);
        tree.insert_extent(fs, new_ext, device)?;
    } else {
        // 传统直接块模式：仅支持追加到前 12 个直接块
        if old_blocks >= 12 {
            return Err(BlockDevError::Unsupported);
        }
        parent_inode.i_block[old_blocks] = new_block as u32;
    }

    // 更新 parent_inode 的 i_size / i_blocks，并写回 inode 表
    let new_size = total_size + block_bytes;
    parent_inode.i_size_lo = new_size as u32;
    parent_inode.i_size_high = ((new_size as u64) >> 32) as u32;
    let used_blocks = old_blocks.saturating_add(1) as u32;
    parent_inode.i_blocks_lo = used_blocks.saturating_mul((BLOCK_SIZE / 512) as u32);
    parent_inode.l_i_blocks_high = 0;

    let (p_group, _pidx) = fs.inode_allocator.global_to_group(parent_ino_num);
    let inode_table_start = match fs.group_descs.get(p_group as usize) {
        Some(desc) => desc.inode_table(),
        None => return Err(BlockDevError::Corrupted),
    };
    let (p_block_num, p_offset, _pg) = fs.inodetable_cahce.calc_inode_location(
        parent_ino_num,
        fs.superblock.s_inodes_per_group,
        inode_table_start,
        BLOCK_SIZE,
    );

    fs.inodetable_cahce.modify(
        device,
        parent_ino_num as u64,
        p_block_num,
        p_offset,
        |inode| {
            inode.i_size_lo = parent_inode.i_size_lo;
            inode.i_size_high = parent_inode.i_size_high;
            inode.i_blocks_lo = parent_inode.i_blocks_lo;
            inode.l_i_blocks_high = parent_inode.l_i_blocks_high;
            inode.i_flags = parent_inode.i_flags;
            inode.i_block = parent_inode.i_block;
        },
    )?;

    // 在新分配的数据块中写入唯一的目录项，占满整个块
    fs.datablock_cache
        .modify(device, new_block, |data| {
            for b in data.iter_mut() {
                *b = 0;
            }
            let mut full_entry = new_entry;
            full_entry.rec_len = BLOCK_SIZE as u16;
            full_entry.to_disk_bytes(&mut data[0..8]);
            let nlen = full_entry.name_len as usize;
            data[8..8 + nlen].copy_from_slice(&full_entry.name[..nlen]);
        })?;

    Ok(())
}

/// 默认开启hashtree查找
/// 通用文件创建：支持多级路径、递归创建父目录
pub fn mkdir<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> Option<Ext4Inode> {
    mkdir_with_ino(device, fs, path).map(|(_, inode)| inode)
}

pub fn mkdir_with_ino<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> Option<(u32, Ext4Inode)> {
    // 先对传入路径做规范化（去掉重复的 '/' 等）
    let norm_path = split_paren_child_and_tranlatevalid(path);

    // 若目标已存在，直接返回
    if let Ok(Some(inode)) = get_file_inode(fs, device, &norm_path) {
        return Some(inode);
    }

    // 根目录和空路径的特殊情况
    if norm_path.is_empty() || norm_path == "/" {
        debug!("Creating root directory");
        if let Err(e) = create_root_directory_entry(fs, device) {
            error!("mkdir create_root_directory_entry failed path={} err={:?} ({})", path, e, e);
            return None;
        }
        return match fs.get_root(device) {
            Ok(inode) => Some((fs.root_inode, inode)),
            Err(e) => {
                error!("mkdir get_root failed path={} err={:?} ({})", path, e, e);
                None
            }
        };
    }

    // 拆分规范化路径，构建 path_vec
    let parts: Vec<&str> = norm_path.split('/').filter(|s| !s.is_empty()).collect();

    if parts.is_empty() {
        return match fs.get_root(device) {
            Ok(inode) => Some((fs.root_inode, inode)),
            Err(e) => {
                error!("mkdir get_root failed(empty parts) path={} err={:?} ({})", path, e, e);
                None
            }
        };
    }

    // 从头逐一判断父路径是否存在，不存在则递归创建
    // 只针对中间父目录，最后一个组件留给当前 mkd 创建
    let mut cur_path = String::from("");
    for i in 0..(parts.len().saturating_sub(1)) {
        if cur_path.is_empty() {
            cur_path.push('/');
            cur_path.push_str(parts[i]);
        } else {
            cur_path.push('/');
            cur_path.push_str(parts[i]);
        }

        if let Ok(None) = get_file_inode(fs, device, &cur_path) {
            if mkdir(device, fs, &cur_path).is_none() {
                error!("mkdir recursive parent create failed path={} parent={}", path, cur_path);
                return None;
            }
        }
    }

    // 计算 parent 与 child
    let child = parts.last().unwrap().to_string();
    let parent = if parts.len() == 1 {
        "/".to_string()
    } else {
        let mut p = String::from("");
        for i in 0..(parts.len() - 1) {
            p.push('/');
            p.push_str(parts[i]);
        }
        p
    };

    // 再次获取父目录 inode 及其 inode 号
    let (parent_ino_num, mut parent_inode) =
        match get_inode_with_num(fs, device, &parent).ok().flatten() {
            Some((n, ino)) => (n, ino),
            None => {
                error!("mkdir get parent inode failed path={} parent={} child={}", path, parent, child);
                return None;
            }
        };

    // 特殊情况：根目录本身
    if (parent.is_empty() || parent == "/") && child.is_empty() {
        debug!("Creating root directory");
        if let Err(e) = create_root_directory_entry(fs, device) {
            error!("mkdir create_root_directory_entry failed path={} err={:?} ({})", path, e, e);
            return None;
        }
        return match fs.get_root(device) {
            Ok(inode) => Some((fs.root_inode, inode)),
            Err(e) => {
                error!("mkdir get_root failed path={} err={:?} ({})", path, e, e);
                None
            }
        };
    }

    // 特殊情况：/lost+found
    if (parent.is_empty() || parent == "/") && child == "lost+found" {
        debug!("Creating /lost+found directory");
        if let Err(e) = create_lost_found_directory(fs, device) {
            error!("mkdir create_lost_found_directory failed path={} err={:?} ({})", path, e, e);
            return None;
        }
        return match get_inode_with_num(fs, device, "/lost+found").ok().flatten() {
            Some((ino, inode)) => Some((ino, inode)),
            None => {
                error!("mkdir post-create lost+found lookup failed path={}", path);
                None
            }
        };
    }

    // 为新目录分配 inode（内部自动选择块组）
    let new_dir_ino = match fs.alloc_inode(device) {
        Ok(ino) => ino,
        Err(e) => {
            error!("mkdir alloc_inode failed path={} parent={} child={} err={:?} ({})", path, parent, child, e, e);
            return None;
        }
    };

    // 为新目录分配数据块（内部自动选择块组）
    let data_block = match fs.alloc_block(device) {
        Ok(b) => b,
        Err(e) => {
            error!("mkdir alloc_block failed path={} ino={} err={:?} ({})", path, new_dir_ino, e, e);
            return None;
        }
    };

    // 初始化新目录的数据块：写 '.' 和 '..'
    {
        let cached = fs.datablock_cache.create_new(data_block);
        let data = &mut cached.data;

        let dot_name = b".";
        let dot_rec_len = Ext4DirEntry2::entry_len(dot_name.len() as u8);
        let dot = Ext4DirEntry2::new(
            new_dir_ino,
            dot_rec_len,
            Ext4DirEntry2::EXT4_FT_DIR,
            dot_name,
        );

        let dotdot_name = b"..";
        let dotdot_rec_len = (BLOCK_SIZE as u16).saturating_sub(dot_rec_len);
        let dotdot = Ext4DirEntry2::new(
            parent_ino_num,
            dotdot_rec_len,
            Ext4DirEntry2::EXT4_FT_DIR,
            dotdot_name,
        );

        {
            dot.to_disk_bytes(&mut data[0..8]);
            let name_len = dot.name_len as usize;
            data[8..8 + name_len].copy_from_slice(&dot.name[..name_len]);
        }

        {
            let offset = dot_rec_len as usize;
            dotdot.to_disk_bytes(&mut data[offset..offset + 8]);
            let name_len = dotdot.name_len as usize;
            data[offset + 8..offset + 8 + name_len].copy_from_slice(&dotdot.name[..name_len]);
        }
    }

    // 写新目录 inode（单块目录，按特性选择 extent 或直接块）
    let (group_idx, _idx) = fs.inode_allocator.global_to_group(new_dir_ino);
    //仅仅的视图，修改过后的

    let mut inode_pre = fs
        .get_inode_by_num(device, new_dir_ino)
        .expect("Can't getinode");
    build_file_block_mapping(fs, &mut inode_pre, &[data_block], device);
    if fs
        .modify_inode(device, new_dir_ino, |inode| {
            inode.i_block = inode_pre.i_block;
            inode.i_mode = Ext4Inode::S_IFDIR | 0o755;
            inode.i_links_count = 2; // . 和 entires本身
            inode.i_size_lo = BLOCK_SIZE as u32;
            inode.i_size_high = 0;
            inode.i_blocks_lo = (BLOCK_SIZE / 512) as u32;
            inode.l_i_blocks_high = 0;
            inode.i_dtime = 0;
            inode.i_flags |= inode_pre.i_flags

            //由于借用冲突，暂时先把mapping移步到外面
        })
        .is_err()
    {
        error!("mkdir modify_inode failed path={} ino={}", path, new_dir_ino);
        return None;
    }

    //更新父目录的i_links_count+1
    {
        let (p_group, _pidx) = fs.inode_allocator.global_to_group(parent_ino_num);
        let p_inode_table_start = match fs.group_descs.get(p_group as usize) {
            Some(desc) => desc.inode_table(),
            None => {
                error!("mkdir parent group desc missing path={} parent_ino={} group={}", path, parent_ino_num, p_group);
                return None;
            }
        };
        let (p_block_num, p_offset, _pg) = fs.inodetable_cahce.calc_inode_location(
            parent_ino_num,
            fs.superblock.s_inodes_per_group,
            p_inode_table_start,
            BLOCK_SIZE,
        );

        let _ = fs.inodetable_cahce.modify(
            device,
            parent_ino_num as u64,
            p_block_num,
            p_offset,
            |inode| {
                inode.i_links_count = inode.i_links_count.saturating_add(1);
            },
        );
    }

    // 更新新目录所属块组的目录计数
    if let Some(desc) = fs.get_group_desc_mut(group_idx) {
        let newc = desc.used_dirs_count().saturating_add(1);
        desc.bg_used_dirs_count_lo = (newc & 0xFFFF) as u16;
        desc.bg_used_dirs_count_hi = ((newc >> 16) & 0xFFFF) as u16;
    }

    // 在父目录的数据块中插入新目录项（线性目录，多块遍历，必要时自动扩展目录块）
    if insert_dir_entry(
        fs,
        device,
        parent_ino_num,
        &mut parent_inode,
        new_dir_ino,
        &child,
        Ext4DirEntry2::EXT4_FT_DIR,
    )
    .is_err()
    {
        error!(
            "mkdir insert_dir_entry failed path={} parent_ino={} child={} ino={}",
            path,
            parent_ino_num,
            child,
            new_dir_ino
        );
        return None;
    }

    match fs.get_inode_by_num(device, new_dir_ino) {
        Ok(inode) => Some((new_dir_ino, inode)),
        Err(e) => {
            error!(
                "mkdir get_inode_by_num failed path={} ino={} err={:?} ({})",
                path,
                new_dir_ino,
                e,
                e
            );
            None
        }
    }
}

/// 根目录创建实现
pub fn create_root_directory_entry<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
) -> BlockDevResult<()> {
    debug!("Initializing root directory...");
    // 是否需要创建根目录由挂载流程基于 inode 内容判断，这里只负责真正的创建

    //  为根目录分配一个数据块（内部自动选择块组）
    let root_inode_num = fs.root_inode;
    let data_block = fs.alloc_block(block_dev)?;

    //  写入目录项 . 和 ..
    {
        let cached = fs.datablock_cache.create_new(data_block);
        let data = &mut cached.data;

        // . 目录项
        let dot_name = b".";
        let dot_rec_len = Ext4DirEntry2::entry_len(dot_name.len() as u8);
        let dot = Ext4DirEntry2::new(
            root_inode_num,
            dot_rec_len,
            Ext4DirEntry2::EXT4_FT_DIR,
            dot_name,
        );

        // ..目录项（根的父目录仍为自己）
        let dotdot_name = b"..";
        let dotdot_rec_len = (BLOCK_SIZE as u16).saturating_sub(dot_rec_len);
        let dotdot = Ext4DirEntry2::new(
            root_inode_num,
            dotdot_rec_len,
            Ext4DirEntry2::EXT4_FT_DIR,
            dotdot_name,
        );

        {
            dot.to_disk_bytes(&mut data[0..8]);
            let name_len = dot.name_len as usize;
            data[8..8 + name_len].copy_from_slice(&dot.name[..name_len]);
        }

        {
            let offset = dot_rec_len as usize;
            dotdot.to_disk_bytes(&mut data[offset..offset + 8]);
            let name_len = dotdot.name_len as usize;
            data[offset + 8..offset + 8 + name_len].copy_from_slice(&dotdot.name[..name_len]);
        }
    }

    //仅仅的视图，修改过后的
    let root_inode_num = fs.root_inode;
    let mut inode_pre = fs
        .get_inode_by_num(block_dev, root_inode_num)
        .expect("Can't getinode");
    build_file_block_mapping(fs, &mut inode_pre, &[data_block], block_dev);

    fs.modify_inode(block_dev, fs.root_inode, |inode| {
        inode.i_flags = inode_pre.i_flags;
        inode.i_block = inode_pre.i_block;
        inode.i_mode = Ext4Inode::S_IFDIR | 0o755; // 目录 + 权限
        inode.i_links_count = 2; // . 和 ..
        inode.i_size_lo = BLOCK_SIZE as u32;
        inode.i_size_high = 0;
        // i_blocks 以 512 字节为单位
        inode.i_blocks_lo = (BLOCK_SIZE / 512) as u32;
        inode.l_i_blocks_high = 0;
    })?;

    //块组描述符更新 目录数
    if let Some(desc) = fs.get_group_desc_mut(0) {
        let newc = desc.used_dirs_count().saturating_add(1);
        desc.bg_used_dirs_count_lo = (newc & 0xFFFF) as u16;
        desc.bg_used_dirs_count_hi = ((newc >> 16) & 0xFFFF) as u16;
    }

    debug!(
        "Root directory created: inode={}, data_block={}",
        fs.root_inode, data_block
    );
    Ok(())
}

/// 创建 /lost+found 目录，并将其挂到根目录下
pub fn create_lost_found_directory<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
) -> BlockDevResult<()> {
    // 如果已经存在则直接返回
    if file_entry_exisr(fs, block_dev, "/lost+found") {
        return Ok(());
    }

    let root_inode_num = fs.root_inode;

    //  分配 inode（内部自动选择块组）
    let lost_ino = fs.alloc_inode(block_dev)?;
    debug!("lost+found inode: {lost_ino}");

    //  分配数据块（内部自动选择块组）
    let data_block = fs.alloc_block(block_dev)?;

    //  初始化 lost+found 目录块（".", ".."）
    {
        let cached = fs.datablock_cache.create_new(data_block);
        let data = &mut cached.data;

        let dot_name = b".";
        let dot_rec_len = Ext4DirEntry2::entry_len(dot_name.len() as u8);
        let dot = Ext4DirEntry2::new(lost_ino, dot_rec_len, Ext4DirEntry2::EXT4_FT_DIR, dot_name);

        let dotdot_name = b"..";
        let dotdot_rec_len = (BLOCK_SIZE as u16).saturating_sub(dot_rec_len);
        let dotdot = Ext4DirEntry2::new(
            root_inode_num,
            dotdot_rec_len,
            Ext4DirEntry2::EXT4_FT_DIR,
            dotdot_name,
        );

        {
            dot.to_disk_bytes(&mut data[0..8]);
            let name_len = dot.name_len as usize;
            data[8..8 + name_len].copy_from_slice(&dot.name[..name_len]);
        }

        {
            let offset = dot_rec_len as usize;
            dotdot.to_disk_bytes(&mut data[offset..offset + 8]);
            let name_len = dotdot.name_len as usize;
            data[offset + 8..offset + 8 + name_len].copy_from_slice(&dotdot.name[..name_len]);
        }
    }

    //  写 lost+found inode
    let (lf_group, _idx) = fs.inode_allocator.global_to_group(lost_ino);

    //仅仅的视图，修改过后的
    let mut inode_pre = fs
        .get_inode_by_num(block_dev, lost_ino)
        .expect("Can't getinode");
    build_file_block_mapping(fs, &mut inode_pre, &[data_block], block_dev);
    debug!(
        "When create lost+found inode iblock,:{:?} ,data_block:{:?}",
        inode_pre.i_block, data_block
    );
    // lost+found 的数据块映射与根目录保持一致：单块目录，按特性选择 extent 或直接块
    fs.modify_inode(block_dev, lost_ino, |inode| {
        // 写回 build_block_dir_mapping 已经构建好的块映射和标志
        inode.i_block = inode_pre.i_block;
        inode.i_flags = inode_pre.i_flags;
        inode.i_mode = Ext4Inode::S_IFDIR | 0o755;
        inode.i_links_count = 2;
        inode.i_size_lo = BLOCK_SIZE as u32;
        inode.i_blocks_lo = (BLOCK_SIZE / 512) as u32;
    })?;

    if let Some(desc) = fs.get_group_desc_mut(lf_group) {
        let newc = desc.used_dirs_count().saturating_add(1);
        desc.bg_used_dirs_count_lo = (newc & 0xFFFF) as u16;
        desc.bg_used_dirs_count_hi = ((newc >> 16) & 0xFFFF) as u16;
    }

    //  更新根目录数据块：加入 lost+found 目录项

    //这里也需要根据extend来解析
    let mut root_inode = fs.get_root(block_dev)?;
    let root_block = resolve_inode_block( block_dev, &mut root_inode, 0)?
        .expect("lost+found logical_block can't map to physical blcok!");

    if root_block == 0 {
        return Err(BlockDevError::Corrupted);
    }

    fs.datablock_cache
        .modify(block_dev, root_block as u64, move |data| {
            let dot_name = b".";
            let dot_rec_len = Ext4DirEntry2::entry_len(dot_name.len() as u8);
            let dot = Ext4DirEntry2::new(
                root_inode_num,
                dot_rec_len,
                Ext4DirEntry2::EXT4_FT_DIR,
                dot_name,
            );

            let dotdot_name = b"..";
            let dotdot_rec_len = Ext4DirEntry2::entry_len(dotdot_name.len() as u8);
            let dotdot = Ext4DirEntry2::new(
                root_inode_num,
                dotdot_rec_len,
                Ext4DirEntry2::EXT4_FT_DIR,
                dotdot_name,
            );

            let lf_name = b"lost+found";
            let lf_rec_len = (BLOCK_SIZE as u16).saturating_sub(dot_rec_len + dotdot_rec_len);
            let lost =
                Ext4DirEntry2::new(lost_ino, lf_rec_len, Ext4DirEntry2::EXT4_FT_DIR, lf_name);

            // 清零整个块
            for b in data.iter_mut() {
                *b = 0;
            }

            // 写 .
            dot.to_disk_bytes(&mut data[0..8]);
            let name_len = dot.name_len as usize;
            data[8..8 + name_len].copy_from_slice(&dot.name[..name_len]);

            // 写 ..
            let mut offset = dot_rec_len as usize;
            dotdot.to_disk_bytes(&mut data[offset..offset + 8]);
            let dd_len = dotdot.name_len as usize;
            data[offset + 8..offset + 8 + dd_len].copy_from_slice(&dotdot.name[..dd_len]);

            // 写 lost+found
            offset += dotdot_rec_len as usize;
            lost.to_disk_bytes(&mut data[offset..offset + 8]);
            let lf_len = lost.name_len as usize;
            data[offset + 8..offset + 8 + lf_len].copy_from_slice(&lost.name[..lf_len]);
        })?;

    //  更新根 inode 的链接计数（多了一个子目录）
    let inode_table_start = match fs.group_descs.first() {
        Some(desc) => desc.inode_table(),
        None => return Err(BlockDevError::Corrupted),
    };
    let (block_num, offset, _group_idx) = fs.inodetable_cahce.calc_inode_location(
        fs.root_inode,
        fs.superblock.s_inodes_per_group,
        inode_table_start,
        BLOCK_SIZE,
    );

    fs.inodetable_cahce.modify(
        block_dev,
        fs.root_inode as u64,
        block_num,
        offset,
        |inode| {
            inode.i_links_count = inode.i_links_count.saturating_add(1);
        },
    )?;

    //  记录到超级块
    fs.superblock.s_lpf_ino = lost_ino;

    debug!(
        "lost+found directory created: inode={lost_ino}, data_block={data_block}"
    );

    Ok(())
}
