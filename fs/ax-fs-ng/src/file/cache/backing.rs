//! Complete backing writes without acknowledging an unwritten suffix.

use axfs_ng_vfs::{FileNode, VfsError, VfsResult};

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
