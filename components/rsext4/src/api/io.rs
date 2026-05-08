use super::*;

/// Set the current file offset.
pub fn lseek(file: &mut OpenFile, location: u64) -> Ext4Result<()> {
    file.offset = location;
    Ok(())
}

fn refresh_open_file_inode<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    file: &mut OpenFile,
) -> Ext4Result<()> {
    let Some((ino, inode)) = get_file_inode(fs, dev, &file.path)? else {
        return Err(Ext4Error::not_found());
    };
    file.inode_num = ino;
    file.inode = inode;
    Ok(())
}

/// Open a file by path.
pub fn open<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    create: bool,
) -> Ext4Result<OpenFile> {
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
        return Err(Ext4Error::not_found());
    }

    mkfile(dev, fs, &norm_path, None, None)?;

    let inode = match get_file_inode(fs, dev, &norm_path)? {
        Some(ino) => ino,
        None => return Err(Ext4Error::corrupted()),
    };

    Ok(OpenFile {
        inode_num: inode.0,
        path: norm_path,
        inode: inode.1,
        offset: 0,
    })
}

/// Writes data at the current file offset.
pub fn write_at<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    file: &mut OpenFile,
    data: &[u8],
) -> Ext4Result<()> {
    if false {
        return Err(Ext4Error::unsupported());
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

/// Read a whole file into memory.
pub fn read<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
) -> Ext4Result<Vec<u8>> {
    read_file(dev, fs, path)
}

/// Reads data from the current file offset.
///
/// The helper refreshes the inode view, clamps the request to EOF, resolves the
/// mapped extent blocks, and returns zero-filled data for sparse holes.
pub fn read_at<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    file: &mut OpenFile,
    len: usize,
) -> Ext4Result<Vec<u8>> {
    if len == 0 {
        refresh_open_file_inode(dev, fs, file)?;
        fs.touch_inode_atime_if_needed(dev, file.inode_num)?;
        refresh_open_file_inode(dev, fs, file)?;
        return Ok(Vec::new());
    }

    // Refresh the cached inode before computing the readable window.
    refresh_open_file_inode(dev, fs, file)?;

    let file_size = file.inode.size();
    if file.offset >= file_size {
        fs.touch_inode_atime_if_needed(dev, file.inode_num)?;
        refresh_open_file_inode(dev, fs, file)?;
        return Ok(Vec::new());
    }

    // Clamp the request to the current file size.
    let to_read = core::cmp::min(len, (file_size - file.offset) as usize);
    let to_read = to_read as u64;
    if to_read == 0 {
        return Ok(Vec::new());
    }

    if !file.inode.have_extend_header_and_use_extend() {
        return Err(Ext4Error::unsupported());
    }

    let block_bytes = BLOCK_SIZE as u64;
    let start_off = file.offset;
    let end_off = start_off + to_read; // exclusive

    let start_lbn = start_off / block_bytes;
    let end_lbn = (end_off - 1) / block_bytes;

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

        if let Some(phys) = resolve_inode_block(dev, &mut file.inode, lbn as u32)? {
            let cached = fs.datablock_cache.get_or_load(dev, phys)?;
            let data = &cached.data[..block_bytes as usize];
            out.extend_from_slice(&data[copy_start as usize..(copy_start + copy_len) as usize]);
        } else {
            // Sparse holes read back as zeroes for the requested logical range.
            out.extend(core::iter::repeat_n(0u8, copy_len as usize));
        }

        if out.len() as u64 >= to_read {
            break;
        }
    }

    out.truncate(to_read as usize);
    // Update atime only after the read path has completed successfully.
    fs.touch_inode_atime_if_needed(dev, file.inode_num)?;
    refresh_open_file_inode(dev, fs, file)?;
    file.offset = file.offset.saturating_add(out.len() as u64);
    Ok(out)
}
