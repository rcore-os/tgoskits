//创建文件夹功能模块

use core::{error::Error};

use alloc::string::String;
use alloc::vec::Vec;
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
use crate::ext4_backend::entries::*;
use crate::alloc::string::ToString;
use crate::ext4_backend::mkfile::*;
use log::{debug, error};

#[derive(Debug)]
pub enum FileError {
    DirExist,
    FileExist,
    DirNotFound,
    FileNotFound,
}

///合法化路径：去掉重复的 '/'
pub fn split_paren_child_and_tranlatevalid(pat:&str)->String{
    //去掉重复///类型和中间空路径
    let mut last_c ='\0';
    let mut result_s = String::new();
    for ch in pat.chars() {
        if ch =='/' && last_c=='/' {
            continue;
        }
        result_s.push(ch);
        last_c=ch;

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
    let mut components = path.split('/').filter(|s| !s.is_empty());

    // 从根开始
    let mut current_inode = fs.get_root(device)?;
    let mut current_ino: u32 = fs.root_inode;

    // 根目录所在 inode 表起始块（先按 group0 处理）
    let inode_table_start = match fs.group_descs.get(0) {
        Some(desc) => desc.inode_table() as u64,
        None => return Err(BlockDevError::Corrupted),
    };

    while let Some(name) = components.next() {
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
        let block_bytes = BLOCK_SIZE as usize;
        let total_blocks = if total_size == 0 {
            0
        } else {
            (total_size + block_bytes - 1) / block_bytes
        };

        let mut found_inode_num: Option<u64> = None;

        for lbn in 0..total_blocks {
            let phys = match resolve_inode_block(fs, device, &mut current_inode, lbn as u32)? {
                Some(b) => b,
                None => continue,
            };

            let cached_block = fs
                .datablock_cache
                .get_or_load(device, phys as u64)?;
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
    let block_bytes = BLOCK_SIZE as usize;
    let total_blocks = if total_size == 0 {
        0
    } else {
        (total_size + block_bytes - 1) / block_bytes
    };

    let mut inserted = false;

    for lbn in 0..total_blocks {
        if inserted {
            break;
        }

        let phys = match resolve_inode_block(fs, device, parent_inode, lbn as u32) {
            Ok(Some(b)) => b,
            _ => continue,
        };

        let _ = fs.datablock_cache.modify(device, phys as u64, |data| {
            if inserted {
                return;
            }

            let block_bytes = BLOCK_SIZE as usize;

            // 从头扫描，找到“最后一条有效目录项”的偏移及其 rec_len
            let mut offset = 0usize;
            let mut last_offset = None;
            let mut last_rec_len = 0usize;

            while offset + 8 <= block_bytes {
                let inode = u32::from_le_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]);
                let rec_len = u16::from_le_bytes([
                    data[offset + 4],
                    data[offset + 5],
                ]) as usize;

                if inode == 0 || rec_len == 0 {
                    break;
                }

                last_offset = Some(offset);
                last_rec_len = rec_len;

                if offset + rec_len >= block_bytes {
                    break;
                }

                offset += rec_len;
            }

            let (last_off, old_rec_len) = match last_offset {
                Some(o) => (o, last_rec_len),
                None => {
                    // 整个块目前没有有效目录项，直接把新 entry 占满整个块
                    if new_rec_len <= block_bytes {
                        let mut full_entry = new_entry;
                        full_entry.rec_len = block_bytes as u16;
                        full_entry.to_disk_bytes(&mut data[0..8]);
                        let nlen = full_entry.name_len as usize;
                        data[8..8 + nlen].copy_from_slice(&full_entry.name[..nlen]);
                        inserted = true;
                    }
                    return;
                }
            };

            // 计算最后一条 entry 的理想长度（对齐到 4 字节）
            // Ext4DirEntry2: inode(4) + rec_len(2) + name_len(1) + file_type(1) + name
            // name_len 在偏移 last_off + 6
            let last_name_len = data[last_off + 6] as usize;
            let mut ideal = 8 + last_name_len;
            ideal = (ideal + 3) & !3; // 4 字节对齐

            if ideal > old_rec_len {
                // 不合理，本块放弃插入
                return;
            }

            let tail = old_rec_len - ideal;
            if tail < new_rec_len {
                // 尾部空间不足，本块放弃插入
                return;
            }

            // 1) 缩短最后一条目录项的 rec_len 为 ideal
            let ideal_bytes = (ideal as u16).to_le_bytes();
            data[last_off + 4] = ideal_bytes[0];
            data[last_off + 5] = ideal_bytes[1];

            // 2) 在尾部写入新 entry，占用剩余空间
            let new_off = last_off + ideal;
            let new_rec_len_total = tail as u16;

            let mut full_entry = new_entry;
            full_entry.rec_len = new_rec_len_total;
            full_entry.to_disk_bytes(&mut data[new_off..new_off + 8]);
            let nlen = full_entry.name_len as usize;
            data[new_off + 8..new_off + 8 + nlen]
                .copy_from_slice(&full_entry.name[..nlen]);

            inserted = true;
        });
    }

    if inserted {
        return Ok(());
    }

    // 所有现有逻辑块都无法容纳新目录项：为目录分配一个新数据块，并扩展 inode 映射
    let new_block = fs.alloc_block(device)?;

    // 更新 parent_inode 的块映射（extent 或直接块）和大小统计
    let block_bytes = BLOCK_SIZE as usize;
    let old_blocks = if total_size == 0 {
        0
    } else {
        (total_size + block_bytes - 1) / block_bytes
    };
    let new_lbn = old_blocks as u32; // 新块对应的逻辑块号

    if fs.superblock.has_extents() && parent_inode.is_extent() {
        // extent 目录：通过 ExtentTree 追加一个长度为 1 的 extent
        let new_ext = Ext4Extent::new(new_lbn, new_block, 1);
        let mut tree = ExtentTree::new( parent_inode);
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
    parent_inode.i_size_high = (new_size >> 32) as u32;
    let used_blocks = old_blocks.saturating_add(1) as u32;
    parent_inode.i_blocks_lo = used_blocks.saturating_mul((BLOCK_SIZE / 512) as u32);
    parent_inode.l_i_blocks_high = 0;

    let (p_group, _pidx) = fs.inode_allocator.global_to_group(parent_ino_num);
    let inode_table_start = match fs.group_descs.get(p_group as usize) {
        Some(desc) => desc.inode_table() as u64,
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
    fs.datablock_cache.modify(device, new_block as u64, |data| {
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
pub fn mkdir<B: BlockDevice>(device: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem, path: &str) -> Option<Ext4Inode> {
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
            debug!("create_root_directory_entry failed: {:?}", e);
            return None;
        }
        return fs.get_root(device).ok();
    }

    // 拆分规范化路径，构建 path_vec
    let parts: Vec<&str> = norm_path.split('/')
        .filter(|s| !s.is_empty())
        .collect();

    if parts.is_empty() {
        return fs.get_root(device).ok();
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
            mkdir(device, fs, &cur_path);
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
    let (parent_ino_num, mut parent_inode) = match get_inode_with_num(fs, device, &parent).ok().flatten() {
        Some((n, ino)) => (n, ino),
        None => return None,
    };

    // 特殊情况：根目录本身
    if (parent == "" || parent == "/") && child.is_empty() {
        debug!("Creating root directory");
        if let Err(e) = create_root_directory_entry(fs, device) {
            debug!("create_root_directory_entry failed: {:?}", e);
            return None;
        }
        return fs.get_root(device).ok();
    }

    // 特殊情况：/lost+found
    if (parent == "" || parent == "/") && child == "lost+found" {
        debug!("Creating /lost+found directory");
        if let Err(e) = create_lost_found_directory(fs, device) {
            debug!("create_lost_found_directory failed: {:?}", e);
            return None;
        }
        return fs.find_file(device, "/lost+found");
    }

    // 为新目录分配 inode（内部自动选择块组）
    let new_dir_ino = match fs.alloc_inode(device) {
        Ok(ino) => ino,
        Err(_) => return None,
    };

    // 为新目录分配数据块（内部自动选择块组）
    let data_block = match fs.alloc_block(device) {
        Ok(b) => b,
        Err(_) => return None,
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
            data[offset + 8..offset + 8 + name_len]
                .copy_from_slice(&dotdot.name[..name_len]);
        }
    }

    // 写新目录 inode（单块目录，按特性选择 extent 或直接块）
    let (group_idx, _idx) = fs.inode_allocator.global_to_group(new_dir_ino);
      //仅仅的视图，修改过后的
    
    let mut inode_pre= fs.get_inode_by_num(device, new_dir_ino).expect("Can't getinode");
    build_file_block_mapping(fs,  &mut inode_pre, &[data_block],device);
    if fs.modify_inode(
            device,
            new_dir_ino as u32,
            |inode| {
                inode.i_block = inode_pre.i_block;
                inode.i_mode = Ext4Inode::S_IFDIR | 0o755;
                inode.i_links_count = 2; // . 和 entires本身
                inode.i_size_lo = BLOCK_SIZE as u32;
                inode.i_size_high = 0;
                inode.i_blocks_lo = (BLOCK_SIZE / 512) as u32;
                inode.l_i_blocks_high = 0;
                inode.i_flags |= inode_pre.i_flags | Ext4Inode::EXT4_EXTENTS_FL | Ext4Inode::EXT4_INDEX_FL

                //由于借用冲突，暂时先把mapping移步到外面
            },
        )
        .is_err()
    {
        return None;
    }
  
    //更新父目录的i_links_count+1
    {
        let (p_group, _pidx) = fs.inode_allocator.global_to_group(parent_ino_num);
        let p_inode_table_start = match fs.group_descs.get(p_group as usize) {
            Some(desc) => desc.inode_table() as u64,
            None => return None,
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
    if let Some(desc) = fs.get_group_desc_mut(group_idx as u32) {
        let newc = desc.used_dirs_count().saturating_add(1);
        desc.bg_used_dirs_count_lo = (newc & 0xFFFF) as u16;
        desc.bg_used_dirs_count_hi = ((newc >> 16) & 0xFFFF) as u16;
    }

    // 在父目录的数据块中插入新目录项（线性目录，多块遍历，必要时自动扩展目录块）
    if insert_dir_entry(fs, device, parent_ino_num, &mut parent_inode, new_dir_ino, &child, Ext4DirEntry2::EXT4_FT_DIR).is_err() {
        return None;
    }

    // 返回新目录的 inode
    get_file_inode(fs, device, path).ok().flatten()
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
            data[offset + 8..offset + 8 + name_len]
                .copy_from_slice(&dotdot.name[..name_len]);
        }
    }

 
    //仅仅的视图，修改过后的
    let root_inode_num = fs.root_inode;
    let mut inode_pre= fs.get_inode_by_num(block_dev, root_inode_num).expect("Can't getinode");
    build_file_block_mapping(fs,  &mut inode_pre, &[data_block],block_dev);

    fs.modify_inode(
            block_dev,
            fs.root_inode,
            |inode| {
                inode.i_flags = inode_pre.i_flags | Ext4Inode::EXT4_INDEX_FL;
                inode.i_block=inode_pre.i_block;
                inode.i_mode = Ext4Inode::S_IFDIR | 0o755; // 目录 + 权限
                inode.i_links_count = 2; // . 和 ..
                inode.i_size_lo = BLOCK_SIZE as u32;
                inode.i_size_high = 0;
                // i_blocks 以 512 字节为单位
                inode.i_blocks_lo = (BLOCK_SIZE / 512) as u32;
                inode.l_i_blocks_high = 0;
            },
        )?;


    //块组描述符更新 目录数
    if let Some(desc) = fs.get_group_desc_mut(0) {
        let newc = desc.used_dirs_count().saturating_add(1);
        desc.bg_used_dirs_count_lo = (newc & 0xFFFF) as u16;
        desc.bg_used_dirs_count_hi = ((newc >> 16) & 0xFFFF) as u16;
    }

    debug!("Root directory created: inode={}, data_block={}", fs.root_inode, data_block);
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
    debug!("lost+found inode: {}", lost_ino);

    //  分配数据块（内部自动选择块组）
    let data_block = fs.alloc_block(block_dev)?;

    //  初始化 lost+found 目录块（".", ".."）
    {
        let cached = fs.datablock_cache.create_new(data_block);
        let data = &mut cached.data;

        let dot_name = b".";
        let dot_rec_len = Ext4DirEntry2::entry_len(dot_name.len() as u8);
        let dot = Ext4DirEntry2::new(
            lost_ino,
            dot_rec_len,
            Ext4DirEntry2::EXT4_FT_DIR,
            dot_name,
        );

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
            data[offset + 8..offset + 8 + name_len]
                .copy_from_slice(&dotdot.name[..name_len]);
        }
    }

    //  写 lost+found inode
    let (lf_group, _idx) = fs.inode_allocator.global_to_group(lost_ino);
    let inode_table_start = match fs.group_descs.get(lf_group as usize) {
        Some(desc) => desc.inode_table() as u64,
        None => return Err(BlockDevError::Corrupted),
    };
    let (block_num, offset, _group_idx) = fs.inodetable_cahce.calc_inode_location(
        lost_ino,
        fs.superblock.s_inodes_per_group,
        inode_table_start,
        BLOCK_SIZE,
    );

    //仅仅的视图，修改过后的
    let mut inode_pre= fs.get_inode_by_num(block_dev, lost_ino).expect("Can't getinode");
    build_file_block_mapping(fs,  &mut inode_pre, &[data_block],block_dev);
    error!("When create lost+found inode iblock,:{:?} ,data_block:{:?}",inode_pre.i_block,data_block);
    // lost+found 的数据块映射与根目录保持一致：单块目录，按特性选择 extent 或直接块
    fs
        .modify_inode(
            block_dev,
            lost_ino,
            |inode| {
                // 写回 build_block_dir_mapping 已经构建好的块映射和标志
                inode.i_block = inode_pre.i_block;
                inode.i_flags = inode_pre.i_flags | Ext4Inode::EXT4_INDEX_FL;
                inode.i_mode = Ext4Inode::S_IFDIR | 0o755;
                inode.i_links_count = 2;
                inode.i_size_lo = BLOCK_SIZE as u32;
                inode.i_blocks_lo = (BLOCK_SIZE / 512) as u32;
            },
        )?;
 

    if let Some(desc) = fs.get_group_desc_mut(lf_group) {
        let newc = desc.used_dirs_count().saturating_add(1);
        desc.bg_used_dirs_count_lo = (newc & 0xFFFF) as u16;
        desc.bg_used_dirs_count_hi = ((newc >> 16) & 0xFFFF) as u16;
    }

     //  更新根目录数据块：加入 lost+found 目录项

    //这里也需要根据extend来解析
    let mut root_inode = fs.get_root(block_dev)?;
    let mut root_block=resolve_inode_block(fs, block_dev, &mut root_inode, 0)?.expect("lost+found logical_block can't map to physical blcok!");

    if root_block == 0 {
        return Err(BlockDevError::Corrupted);
    }

    fs.datablock_cache.modify(block_dev, root_block as u64, move |data| {
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
        let lost = Ext4DirEntry2::new(
            lost_ino,
            lf_rec_len,
            Ext4DirEntry2::EXT4_FT_DIR,
            lf_name,
        );

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
        data[offset + 8..offset + 8 + dd_len]
            .copy_from_slice(&dotdot.name[..dd_len]);

        // 写 lost+found
        offset += dotdot_rec_len as usize;
        lost.to_disk_bytes(&mut data[offset..offset + 8]);
        let lf_len = lost.name_len as usize;
        data[offset + 8..offset + 8 + lf_len]
            .copy_from_slice(&lost.name[..lf_len]);
    })?;

    //  更新根 inode 的链接计数（多了一个子目录）
    let inode_table_start = match fs.group_descs.get(0) {
        Some(desc) => desc.inode_table() as u64,
        None => return Err(BlockDevError::Corrupted),
    };
    let (block_num, offset, _group_idx) = fs.inodetable_cahce.calc_inode_location(
        fs.root_inode,
        fs.superblock.s_inodes_per_group,
        inode_table_start,
        BLOCK_SIZE,
    );

    let _ = fs.inodetable_cahce.modify(
        block_dev,
        fs.root_inode as u64,
        block_num,
        offset,
        |inode| {
            let old = inode.i_links_count;
            inode.i_links_count = inode.i_links_count.saturating_add(1);
        },
    )?;

    //  记录到超级块
    fs.superblock.s_lpf_ino = lost_ino;

    debug!(
        "lost+found directory created: inode={}, data_block={}",
        lost_ino,
        data_block
    );



    Ok(())
}