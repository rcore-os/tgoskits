use crate::{
    BlockDevice, Ext4Error, Ext4FileSystem, Ext4Result, Jbd2Dev,
    api::{OpenFile, Vec, refresh_open_file_inode_by_num},
    config::runtime_block_size,
    loopfile::resolve_inode_block,
    read_file,
};
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
        refresh_open_file_inode_by_num(dev, fs, file)?;
        fs.touch_inode_atime_if_needed(dev, file.inode_num)?;
        refresh_open_file_inode_by_num(dev, fs, file)?;
        return Ok(Vec::new());
    }

    // Refresh the cached inode before computing the readable window.
    refresh_open_file_inode_by_num(dev, fs, file)?;

    let file_size = file.inode.size();
    if file.offset >= file_size {
        fs.touch_inode_atime_if_needed(dev, file.inode_num)?;
        refresh_open_file_inode_by_num(dev, fs, file)?;
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

    let block_bytes = runtime_block_size() as u64;
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
    refresh_open_file_inode_by_num(dev, fs, file)?;
    file.offset = file.offset.saturating_add(out.len() as u64);
    Ok(out)
}
