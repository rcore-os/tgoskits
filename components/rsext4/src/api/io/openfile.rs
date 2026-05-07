use crate::{
    BlockDevice, Ext4Error, Ext4FileSystem, Ext4Result, Jbd2Dev, api::OpenFile,
    dir::split_paren_child_and_tranlatevalid, loopfile::get_file_inode, mkfile,
};
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
