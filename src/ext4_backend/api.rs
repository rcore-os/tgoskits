use alloc::string::String;
use alloc::vec::Vec;
use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::dir::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::file::*;
use crate::ext4_backend::loopfile::*;
use crate::ext4_backend::error::*;
use crate::ext4_backend::*;
use crate::BLOCK_SIZE;
/// 文件句柄
pub struct OpenFile {
    pub inode_num:u32,
    pub path: String,
    pub inode: Ext4Inode,
    pub offset: u64,
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
    location: u64
    )->bool{
        file.offset = location;
        true
    }

fn refresh_open_file_inode<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    file: &mut OpenFile,
) -> BlockDevResult<()> {
    let Some((_ino, inode)) = get_file_inode(fs, dev, &file.path)? else {
        return Err(BlockDevError::InvalidInput);
    };
    file.inode = inode;
    Ok(())
}

///打开文件 只能自动创建文件
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
            inode_num: inode.0,
            path: norm_path,
            inode: real_inode,
            offset: 0,
        });
    }

    if !create {
        return Err(BlockDevError::WriteError);
    }

    let inode = match mkfile_with_ino(dev, fs, &norm_path, None,None) {
        Some(ino) => ino,
        None => return Err(BlockDevError::WriteError),
    };

    Ok(OpenFile {
        inode_num:inode.0,
        path: norm_path,
        inode:inode.1,
        offset: 0,
    })
}

///写入文件:基于当前offset追加写入
pub fn write_at<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    file: &mut OpenFile,
    data: &[u8],
) -> BlockDevResult<()> {

    if data.len() > usize::MAX {
        // 超出平台支持的大小
        return Err(BlockDevError::Unsupported);
    }

    if data.is_empty() {
        return Ok(());
    }

    let off = file.offset;
    write_file(dev, fs, &file.path, off, data)?;
    file.offset = file.offset.saturating_add(data.len() as u64);
    refresh_open_file_inode(dev, fs, file)?;
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

    refresh_open_file_inode(dev, fs, file)?;

    let file_size = file.inode.size() as u64;
    if file.offset >= file_size {
        return Ok(Vec::new());
    }

    let to_read = core::cmp::min(len, (file_size - file.offset) as usize);
    let to_read = to_read as u64;
    if to_read == 0 {
        return Ok(Vec::new());
    }

    if !file.inode.have_extend_header_and_use_extend() {
        return Err(BlockDevError::Unsupported);
    }

    let block_bytes = BLOCK_SIZE as u64;
    let start_off = file.offset;
    let end_off = start_off + to_read; // exclusive

    let start_lbn = start_off / block_bytes;
    let end_lbn = (end_off - 1) / block_bytes;

    let extent_map = resolve_inode_block_allextend(fs, dev, &mut file.inode)?;

    let mut out = Vec::with_capacity(to_read as usize);
    for lbn in start_lbn..=end_lbn {
        let lbn_start = lbn * block_bytes;
        let lbn_end = lbn_start + block_bytes;

        let copy_start = core::cmp::max(start_off, lbn_start) - lbn_start;
        let copy_end = core::cmp::min(end_off, lbn_end) - lbn_start;
        let copy_len = copy_end.saturating_sub(copy_start);
        if copy_len == 0 {
            continue;
        }

        if let Some(&phys) = extent_map.get(&(lbn as u32)) {
            let cached = fs.datablock_cache.get_or_load(dev, phys)?;
            let data = &cached.data[..block_bytes as usize];
            out.extend_from_slice(&data[copy_start as usize ..(copy_start + copy_len) as usize]);
        } else {
            // Hole: return zeros for the requested logical range.
            
            out.extend(core::iter::repeat_n(0u8, copy_len as usize));
        }

        if out.len() as u64 >= to_read {
            break;
        }
    }

    out.truncate(to_read as usize);
    file.offset = file.offset.saturating_add(out.len() as u64);
    Ok(out)
}
