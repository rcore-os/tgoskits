//! Complete backing I/O without treating a short transfer as a complete range.

use axfs_ng_vfs::{FileNode, VfsError, VfsResult};

pub(super) fn read_until_eof_at(
    file: &FileNode,
    bytes: &mut [u8],
    mut offset: u64,
) -> VfsResult<usize> {
    let mut total = 0;
    while total < bytes.len() {
        let read = file.read_at(&mut bytes[total..], offset)?;
        if read > bytes.len() - total {
            return Err(VfsError::Io);
        }
        if read == 0 {
            break;
        }
        total += read;
        offset = offset
            .checked_add(read as u64)
            .ok_or(VfsError::InvalidInput)?;
    }
    Ok(total)
}

pub(super) fn write_all_at(file: &FileNode, mut bytes: &[u8], mut offset: u64) -> VfsResult<()> {
    while !bytes.is_empty() {
        let written = file.write_at(bytes, offset)?;
        if written == 0 || written > bytes.len() {
            return Err(VfsError::Io);
        }
        bytes = &bytes[written..];
        offset = offset
            .checked_add(written as u64)
            .ok_or(VfsError::InvalidInput)?;
    }
    Ok(())
}
