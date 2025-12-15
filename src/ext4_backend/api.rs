use alloc::string::String;
use alloc::vec::Vec;

use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::dir::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::file::*;
use crate::ext4_backend::loopfile::*;
use crate::ext4_backend::*;
use crate::BLOCK_SIZE;
/// 文件句柄
pub struct OpenFile {
    pub path: String,
    pub inode: Ext4Inode,
    pub offset: usize,
}

///挂载Ext4文件系统
pub fn fs_mount<B: BlockDevice>(dev: &mut Jbd2Dev<B>) -> BlockDevResult<Ext4FileSystem> {
    ext4::mount(dev)
}

///卸载Ext4文件系统
pub fn fs_umount<B: BlockDevice>(fs: Ext4FileSystem, dev: &mut Jbd2Dev<B>) -> BlockDevResult<()> {
    ext4::umount(fs, dev)
}
pub fn lseek(
    file:&mut OpenFile,
    location:usize
    )->bool{
        let file_size = file.inode.size();
        if location>(file_size-1) as usize {
            return false;
        }
        file.offset = location;
        true
    }

///打开文件：可选自动创建
pub fn open<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    create: bool,
) -> BlockDevResult<OpenFile> {
    let norm_path = split_paren_child_and_tranlatevalid(path);

    if let Ok(Some(inode)) = get_file_inode(fs, dev, &norm_path) {
        let real_inode = inode.1;
        return Ok(OpenFile {
            path: norm_path,
            inode: real_inode,
            offset: 0,
        });
    }

    if !create {
        return Err(BlockDevError::WriteError);
    }

    let inode = match mkfile(dev, fs, &norm_path, None) {
        Some(ino) => ino,
        None => return Err(BlockDevError::WriteError),
    };

    Ok(OpenFile {
        path: norm_path,
        inode,
        offset: 0,
    })
}

///写入文件:基于当前offset追加写入
pub fn append<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    file: &mut OpenFile,
    data: &[u8],
) -> BlockDevResult<()> {
    if data.is_empty() {
        return Ok(());
    }

    let off = file.offset;
    write_file(dev, fs, &file.path, off, data)?;
    file.offset = file.offset.saturating_add(data.len());
    Ok(())
}

///读取整个文件内容
pub fn read<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> BlockDevResult<Option<Vec<u8>>> {
    read_file(dev, fs, path)
}

///read_at 计算文件offset后读取
pub fn read_at<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    file: &mut OpenFile,
    len: usize,
) -> BlockDevResult<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }

    let file_size = file.inode.size() as usize;
    if file.offset >= file_size {
        return Ok(Vec::new());
    }

    let to_read = core::cmp::min(len, file_size - file.offset);
    if to_read == 0 {
        return Ok(Vec::new());
    }

    let block_bytes = BLOCK_SIZE;
    let start_off = file.offset;
    let end_off = start_off + to_read; // exclusive

    let start_lbn = start_off / block_bytes;
    let end_lbn = (end_off - 1) / block_bytes;

    let extent_map = if file.inode.is_extent() {
        Some(resolve_inode_block_allextend_lbn(fs, dev, &mut file.inode)?)
    } else {
        None
    };

    let mut out = Vec::with_capacity(to_read);
    for lbn in start_lbn..=end_lbn {
        let lbn_start = lbn * block_bytes;
        let lbn_end = lbn_start + block_bytes;

        let copy_start = core::cmp::max(start_off, lbn_start) - lbn_start;
        let copy_end = core::cmp::min(end_off, lbn_end) - lbn_start;
        let copy_len = copy_end.saturating_sub(copy_start);
        if copy_len == 0 {
            continue;
        }

        let mut phys_opt: Option<u64> = None;
        if let Some(ref map) = extent_map {
            if let Ok(idx) = map.binary_search_by_key(&(lbn as u32), |(k, _)| *k) {
                phys_opt = Some(map[idx].1);
            }
        }

        if phys_opt.is_none() {
            phys_opt = resolve_inode_block(fs, dev, &mut file.inode, lbn as u32)?.map(|v| v as u64);
        }

        if let Some(phys) = phys_opt {
            let cached = fs.datablock_cache.get_or_load(dev, phys)?;
            let data = &cached.data[..block_bytes];
            out.extend_from_slice(&data[copy_start..copy_start + copy_len]);
        } else {
            out.extend(core::iter::repeat_n(0u8, copy_len));
        }

        if out.len() >= to_read {
            break;
        }
    }

    out.truncate(to_read);
    file.offset = file.offset.saturating_add(out.len());
    Ok(out)
}
