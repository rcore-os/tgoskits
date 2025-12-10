use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::ext4_backend::jbd2::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::jbd2::jbdstruct::*;
use crate::ext4_backend::endian::*;
use crate::ext4_backend::superblock::*;
use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::loopfile::*;
use crate::ext4_backend::entries::*;
use crate::ext4_backend::mkfile::*;
use crate::ext4_backend::*;
use crate::ext4_backend::datablock_cache::*;
use crate::ext4_backend::inodetable_cache::*;
use crate::ext4_backend::blockgroup_description::*;
use crate::ext4_backend::mkd::*;
use crate::ext4_backend::tool::*;
use crate::ext4_backend::jbd2::jbd2::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::bitmap::*;

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

///打开文件：可选自动创建
pub fn open_file<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    create: bool,
) -> BlockDevResult<OpenFile> {
    let norm_path = split_paren_child_and_tranlatevalid(path);

    if let Ok(Some(inode)) = get_file_inode(fs, dev, &norm_path) {
        return Ok(OpenFile { path: norm_path, inode, offset: 0 });
    }

    if !create {
        return Err(BlockDevError::WriteError);
    }

    let inode = match mkfile(dev, fs, &norm_path, None) {
        Some(ino) => ino,
        None => return Err(BlockDevError::WriteError),
    };

    Ok(OpenFile { path: norm_path, inode, offset: 0 })
}

///写入文件:基于当前offset追加写入
pub fn write_to_file<B: BlockDevice>(
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
pub fn read_file_all<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> BlockDevResult<Option<Vec<u8>>> {
    read_file(dev, fs, path)
}

///基于当前offset读取指定长度
pub fn read_from_file<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    file: &mut OpenFile,
    len: usize,
) -> BlockDevResult<Vec<u8>> {
    if len == 0 {
        return Ok(Vec::new());
    }

    let buf_opt = read_file(dev, fs, &file.path)?;
    let data = match buf_opt {
        Some(v) => v,
        None => return Ok(Vec::new()),
    };

    if file.offset >= data.len() {
        return Ok(Vec::new());
    }

    let end = core::cmp::min(data.len(), file.offset.saturating_add(len));
    let slice = data[file.offset..end].to_vec();
    file.offset = end;
    Ok(slice)
}

