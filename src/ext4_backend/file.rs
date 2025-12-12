use core::u32;

use alloc::string::ToString;
use alloc::vec::Vec;
use log::debug;
use log::{error, warn};

use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::dir::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::entries::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::extents_tree::*;
use crate::ext4_backend::loopfile::*;

//mv
pub fn mv<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    old_path: &str,
    new_path: &str,
) {
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
        None => return,
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
        None => return,
    };

    // 找到 old entry（inode + file_type），找不到就返回
    let (_old_pino, mut old_parent_inode) = match get_inode_with_num(fs, block_dev, &old_parent)
        .ok()
        .flatten()
    {
        Some(v) => v,
        None => return,
    };

    let mut src_ino: Option<u32> = None;
    let mut src_ft: Option<u8> = None;
    if let Ok(blocks) = resolve_inode_block_allextend(fs, block_dev, &mut old_parent_inode) {
        for phys in blocks {
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
        None => return,
    };
    let src_ft = src_ft.unwrap_or(Ext4DirEntry2::EXT4_FT_UNKNOWN);

    // new_parent 必须存在且是目录
    let (new_pino, new_parent_inode) = match get_inode_with_num(fs, block_dev, &new_parent)
        .ok()
        .flatten()
    {
        Some(v) => v,
        None => return,
    };
    if !new_parent_inode.is_dir() {
        return;
    }

    // new_path 已存在则返回
    if get_file_inode(fs, block_dev, &new_norm)
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }

    // old_path 不允许为根目录
    if old_norm == "/" {
        return;
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
        return;
    }

    // 删除旧 entry
    if !remove_inodeentry_from_parentdir(fs, block_dev, &old_parent, &old_name) {
        let _ = remove_inodeentry_from_parentdir(fs, block_dev, &new_parent, &new_name);
        return;
    }

    // 目录跨父目录移动：更新 link 以及 '..'
    let mut moved_inode = match fs.get_inode_by_num(block_dev, src_ino) {
        Ok(v) => v,
        Err(_) => return,
    };
    if moved_inode.is_dir() {
        // 父目录不同才需要改
        let old_pino = match get_inode_with_num(fs, block_dev, &old_parent)
            .ok()
            .flatten()
        {
            Some((n, _)) => n,
            None => return,
        };
        if old_pino != new_pino {
            let _ = fs.modify_inode(block_dev, old_pino, |td| {
                td.i_links_count = td.i_links_count.saturating_sub(1);
            });
            let _ = fs.modify_inode(block_dev, new_pino, |td| {
                td.i_links_count = td.i_links_count.saturating_add(1);
            });

            // 更新被移动目录的 ".." 指向新父目录 inode
            let first_blk = match resolve_inode_block(fs, block_dev, &mut moved_inode, 0) {
                Ok(Some(b)) => b,
                _ => return,
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

    for phys in blocks {
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
                Ok(v) => v,
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
            for phys in blocks {
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
        let phys = match resolve_inode_block(fs, block_dev, &mut parent_inode, lbn as u32) {
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
                if name_len > 0 && offset + 8 + name_len <= block_bytes {
                    let name = &data[offset + 8..offset + 8 + name_len];
                    if inode != 0 && name == name_bytes {
                        if let Some(poff) = prev_off {
                            let new_len = prev_rec_len.saturating_add(rec_len);
                            let bytes = new_len.to_le_bytes();
                            data[poff + 4] = bytes[0];
                            data[poff + 5] = bytes[1];
                        } else {
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

            let dir_blocks: Vec<u64> =
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

            for &phys in &dir_blocks {
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
                Ok(v) => v,
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
            .expect("Parse inode extend failed");
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
            tree.insert_extent(fs, extend, block_dev).expect("Extend insert Failed!");
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
pub fn mkfile<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    initial_data: Option<&[u8]>,
) -> Option<Ext4Inode> {
    // 规范化路径
    let norm_path = split_paren_child_and_tranlatevalid(path);

    // 如果目标已存在，直接返回
    if let Ok(Some((_ino_num, inode))) = get_file_inode(fs, device, &norm_path) {
        return Some(inode);
    }

    // 拆 parent / child
    let mut valid_path = norm_path;
    let split_point = valid_path.rfind('/')?;
    let child = valid_path.split_off(split_point)[1..].to_string();
    let parent = valid_path;

    // 确保父目录存在
    mkdir(device, fs, &parent)?;

    // 重新获取父目录 inode 及其 inode 号
    let (parent_ino_num, parent_inode) =
        match get_inode_with_num(fs, device, &parent).ok().flatten() {
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
    new_inode.i_links_count = 1;

    let size_lo = (total_written & 0xffffffff) as u32;
    let size_hi = ((total_written as u64) >> 32) as u32;

    if !data_blocks.is_empty() {
        // 有初始数据：多块或单块文件
        let used_databyte = data_blocks.len() as u64;
        let iblocks_used = used_databyte.saturating_mul(BLOCK_SIZE as u64 / 512) as u32;
        let used_blocks_lo = iblocks_used;
        let used_blocks_hi = (iblocks_used as u64 >> 32) as u16;
        new_inode.i_size_lo = size_lo;
        new_inode.i_size_high = size_hi;
        new_inode.i_blocks_lo = used_blocks_lo;
        new_inode.l_i_blocks_high = used_blocks_hi;
        new_inode.l_i_blocks_high = 0;

        build_file_block_mapping(fs, &mut new_inode, &data_blocks, device);
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
            *on_disk = new_inode;
        })
        .is_err()
    {
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
        return None;
    }

    //返回新文件 inode
    get_file_inode(fs, device, path)
        .ok()
        .flatten()
        .map(|(_ino_num, inode)| inode)
}

///读取指定路径的整个文件内容
pub fn read_file<B: BlockDevice>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> BlockDevResult<Option<Vec<u8>>> {
    let mut inode = match get_file_inode(fs, device, path) {
        Ok(Some((_ino_num, ino))) => ino,
        Ok(None) => return Ok(None),
        Err(e) => return Err(e),
    };
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

    for lbn in 0..total_blocks {
        let phys = match resolve_inode_block(fs, device, &mut inode, lbn as u32)? {
            Some(b) => b,
            None => break,
        };

        let cached = fs.datablock_cache.get_or_load(device, phys as u64)?;
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
    let block_bytes = BLOCK_SIZE;

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
        old_size.div_ceil(block_bytes)
    };
    let new_blocks = if end == 0 {
        0
    } else {
        end.div_ceil(block_bytes)
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
        inode.i_size_high = ((new_size as u64) >> 32) as u32;
        let used_blocks = new_blocks as u32;
        inode.i_blocks_lo = used_blocks.saturating_mul((BLOCK_SIZE / 512) as u32);
        inode.l_i_blocks_high = 0;

        // 写回 inode 元数据
        let (group_idx, _idx) = fs.inode_allocator.global_to_group(inode_num);
        let inode_table_start = match fs.group_descs.get(group_idx as usize) {
            Some(desc) => desc.inode_table(),
            None => return Err(BlockDevError::Corrupted),
        };
        let (block_num, off, _g) = fs.inodetable_cahce.calc_inode_location(
            inode_num,
            fs.superblock.s_inodes_per_group,
            inode_table_start,
            BLOCK_SIZE,
        );

        fs.inodetable_cahce
            .modify(device, inode_num as u64, block_num, off, |on_disk| {
                on_disk.i_size_lo = inode.i_size_lo;
                on_disk.i_size_high = inode.i_size_high;
                on_disk.i_blocks_lo = inode.i_blocks_lo;
                on_disk.l_i_blocks_high = inode.l_i_blocks_high;
                on_disk.i_flags = inode.i_flags;
                on_disk.i_block = inode.i_block;
            })?;
    }

   

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

            blk[dst_off..dst_off + len].copy_from_slice(&data[src_off..src_off + len]);
        })?;
    }

    Ok(())
}
