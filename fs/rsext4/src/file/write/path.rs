//! Path resolution for the serialized file-write entry point.

use super::*;

pub fn write_file<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    path: &str,
    offset: u64,
    data: &[u8],
) -> Ext4Result<()> {
    if data.is_empty() {
        return Ok(());
    }

    // Resolve the inode once before switching to the inode-based writer.
    let info = match get_inode_with_num(fs, device, path)? {
        Some(v) => v,
        None => return Err(Ext4Error::not_found()),
    };
    let (inode_num, _inode) = info;

    write_inode_data(device, fs, inode_num, offset, data)
}
