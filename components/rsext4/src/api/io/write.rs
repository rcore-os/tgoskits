use crate::{
    BlockDevice, Ext4Error, Ext4FileSystem, Ext4Result, Jbd2Dev,
    api::{OpenFile, refresh_open_file_inode_by_num},
    write_file,
};
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
    refresh_open_file_inode_by_num(dev, fs, file)?;
    Ok(())
}
