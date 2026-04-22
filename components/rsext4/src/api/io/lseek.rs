use alloc::collections::BTreeMap;

use crate::{
    Ext4Result,
    api::OpenFile,
    blockdev::{BlockDevice, Jbd2Dev},
    bmalloc::{AbsoluteBN, LogicalBN},
    error::{Errno, Ext4Error},
    ext4::Ext4FileSystem,
    loopfile::resolve_inode_block_allextend,
    tool::ext4_get_maxbytes,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekWhence {
    /// Seek relative to the start of the file (SEEK_SET).
    Set,
    /// Seek relative to the current offset (SEEK_CUR).
    Cur,
    /// Seek relative to the end of the file (SEEK_END).
    End,
    /// Seek to the next data region (SEEK_DATA).
    ///
    /// Linux ext4 defines "data" vs "hole" using the block mapping; we model
    /// this via the inode's extent map.
    Data,
    /// Seek to the next hole region (SEEK_HOLE).
    ///
    /// A virtual hole exists at EOF; if there are no holes before EOF, the
    /// returned offset is the file size.
    Hole,
}

impl TryFrom<i32> for SeekWhence {
    type Error = Ext4Error;

    /// Validates and converts a Linux-style `whence` integer.
    ///
    /// Matches UAPI `SEEK_SET=0`, `SEEK_CUR=1`, `SEEK_END=2`, `SEEK_DATA=3`,
    /// `SEEK_HOLE=4`. Any other value returns `EINVAL`.
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(SeekWhence::Set),
            1 => Ok(SeekWhence::Cur),
            2 => Ok(SeekWhence::End),
            3 => Ok(SeekWhence::Data),
            4 => Ok(SeekWhence::Hole),
            _ => Err(Ext4Error::from(Errno::EINVAL)),
        }
    }
}

/// Validates a resolved absolute file position against the ext4 maxbytes limit.
///
/// This is a Rust-level equivalent of Linux `vfs_setpos()` validation logic.
fn validate_new_pos(candidate_abs_pos: i128, maxbytes: u64) -> Ext4Result<u64> {
    if candidate_abs_pos < 0 || candidate_abs_pos > i128::from(maxbytes) {
        return Err(Ext4Error::from(Errno::EINVAL));
    }
    Ok(candidate_abs_pos as u64)
}

/// SEEK_DATA implementation for extent-backed inodes.
///
/// Returns the input offset if it is already within a mapped block; otherwise
/// returns the start of the next mapped block. Returns `None` if no data exists
/// before EOF.
fn seek_data_in_extents(
    extent_map: &BTreeMap<LogicalBN, AbsoluteBN>,
    start_off: u64,
    file_size: u64,
    block_bytes: u64,
) -> Option<u64> {
    let start_lbn = LogicalBN::new(u32::try_from(start_off / block_bytes).ok()?);
    if extent_map.contains_key(&start_lbn) {
        return Some(start_off);
    }

    let next_lbn = extent_map.range(start_lbn..).next().map(|(lbn, _)| *lbn)?;
    let next_off = u64::from(next_lbn.raw()) * block_bytes;
    if next_off < file_size {
        Some(next_off)
    } else {
        None
    }
}

/// SEEK_HOLE implementation for extent-backed inodes.
///
/// Returns the input offset if it lies in a hole; otherwise returns the start
/// of the next hole. If there are no holes before EOF, returns `file_size`
/// (the virtual hole at EOF).
fn seek_hole_in_extents(
    extent_map: &BTreeMap<LogicalBN, AbsoluteBN>,
    start_off: u64,
    file_size: u64,
    block_bytes: u64,
) -> Option<u64> {
    let start_lbn = LogicalBN::new(u32::try_from(start_off / block_bytes).ok()?);
    if !extent_map.contains_key(&start_lbn) {
        return Some(start_off);
    }

    let eof_lbn = LogicalBN::new(u32::try_from((file_size - 1) / block_bytes).ok()?);
    let mut expected = start_lbn;

    for (&lbn, _) in extent_map.range(start_lbn..) {
        if lbn > eof_lbn {
            break;
        }
        if lbn > expected {
            return Some(u64::from(expected.raw()) * block_bytes);
        }
        if lbn == expected {
            expected = LogicalBN::new(expected.raw().saturating_add(1));
        }
    }

    if expected <= eof_lbn {
        Some(u64::from(expected.raw()) * block_bytes)
    } else {
        Some(file_size)
    }
}

/// Linux-like ext4 `lseek` implementation.
///
/// Returns the new absolute offset on success. On error the original file
/// offset is preserved.
///
/// Note: this API treats `OpenFile` as the authoritative in-memory view. If
/// other handles/path-based operations mutate the same inode (truncate/write),
/// callers must refresh `file.inode` before using SEEK_END/DATA/HOLE.
pub fn lseek<B: BlockDevice>(
    dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    file: &mut OpenFile,
    offset: i64,
    whence: SeekWhence,
) -> Ext4Result<u64> {
    // TODO(linux-lseek-authoritative-inode): 这里使用 `OpenFile` 缓存 inode 计算
    // i_size/maxbytes。若同 inode 被其他句柄更新，可能与 Linux 当下 inode 状态偏离。
    // 目标语义：在 SEEK_END/SEEK_DATA/SEEK_HOLE 路径使用 authoritative inode 视图。
    let file_size = file.inode.size();
    let maxbytes = ext4_get_maxbytes(&fs.superblock, &file.inode);
    let block_bytes = fs.superblock.block_size() as u64;

    let new_off = match whence {
        SeekWhence::Set => validate_new_pos(i128::from(offset), maxbytes)?,
        SeekWhence::Cur => {
            // Linux special-cases (0, SEEK_CUR) as a pure query.
            if offset == 0 {
                return Ok(file.offset);
            }
            let candidate = i128::from(file.offset)
                .checked_add(i128::from(offset))
                .ok_or_else(|| Ext4Error::from(Errno::EINVAL))?;
            validate_new_pos(candidate, maxbytes)?
        }
        SeekWhence::End => {
            // Negative `offset` is allowed; the resolved absolute position must be valid.
            let candidate = i128::from(file_size)
                .checked_add(i128::from(offset))
                .ok_or_else(|| Ext4Error::from(Errno::EINVAL))?;
            validate_new_pos(candidate, maxbytes)?
        }
        SeekWhence::Data => {
            // Linux iomap SEEK_DATA returns ENXIO for pos < 0 or pos >= i_size.
            if offset < 0 {
                return Err(Ext4Error::from(Errno::ENXIO));
            }
            let start = offset as u64;
            if start >= file_size {
                return Err(Ext4Error::from(Errno::ENXIO));
            }

            if !(fs.superblock.has_extents() && file.inode.have_extend_header_and_use_extend()) {
                // Traditional block pointers are not implemented yet.
                // TODO(linux-lseek-indirect): 补齐非 extent inode 的 SEEK_DATA/SEEK_HOLE 语义，
                // 避免直接 `EOPNOTSUPP`。
                return Err(Ext4Error::unsupported());
            }

            // TODO(linux-lseek-iomap-details): 当前是简化 extent-map 语义，
            // 未完整对齐 Linux iomap 对 unwritten/page-cache 的处理。
            let extent_map = resolve_inode_block_allextend(dev, &mut file.inode)?;
            let found = seek_data_in_extents(&extent_map, start, file_size, block_bytes)
                .ok_or_else(|| Ext4Error::from(Errno::ENXIO))?;
            validate_new_pos(i128::from(found), maxbytes)?
        }
        SeekWhence::Hole => {
            // Linux iomap SEEK_HOLE returns ENXIO for pos < 0 or pos >= i_size.
            if offset < 0 {
                return Err(Ext4Error::from(Errno::ENXIO));
            }
            let start = offset as u64;
            if start >= file_size {
                return Err(Ext4Error::from(Errno::ENXIO));
            }

            if !(fs.superblock.has_extents() && file.inode.have_extend_header_and_use_extend()) {
                // TODO(linux-lseek-indirect): 补齐非 extent inode 的 SEEK_DATA/SEEK_HOLE 语义，
                // 避免直接 `EOPNOTSUPP`。
                return Err(Ext4Error::unsupported());
            }

            // TODO(linux-lseek-iomap-details): 当前是简化 extent-map 语义，
            // 未完整对齐 Linux iomap 对 unwritten/page-cache 的处理。
            let extent_map = resolve_inode_block_allextend(dev, &mut file.inode)?;
            let found = seek_hole_in_extents(&extent_map, start, file_size, block_bytes)
                .ok_or_else(|| Ext4Error::from(Errno::ENXIO))?;
            validate_new_pos(i128::from(found), maxbytes)?
        }
    };

    file.offset = new_off;
    Ok(new_off)
}
