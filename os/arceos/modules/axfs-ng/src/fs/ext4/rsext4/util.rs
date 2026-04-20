use axfs_ng_vfs::{NodeType, VfsError};
use rsext4::{Ext4Error, entries::Ext4DirEntry2};

pub fn into_vfs_err(err: Ext4Error) -> VfsError {
    let linux_err = match err.code {
        rsext4::error::Errno::ENOENT => ax_errno::LinuxError::ENOENT,
        rsext4::error::Errno::EEXIST => ax_errno::LinuxError::EEXIST,
        rsext4::error::Errno::EISDIR => ax_errno::LinuxError::EISDIR,
        rsext4::error::Errno::ENOTDIR => ax_errno::LinuxError::ENOTDIR,
        rsext4::error::Errno::ENOTEMPTY => ax_errno::LinuxError::ENOTEMPTY,
        rsext4::error::Errno::EACCES => ax_errno::LinuxError::EACCES,
        rsext4::error::Errno::EINVAL => ax_errno::LinuxError::EINVAL,
        rsext4::error::Errno::ENOSPC => ax_errno::LinuxError::ENOSPC,
        rsext4::error::Errno::EROFS => ax_errno::LinuxError::EROFS,
        rsext4::error::Errno::EBUSY => ax_errno::LinuxError::EBUSY,
        rsext4::error::Errno::EBADF => ax_errno::LinuxError::EBADF,
        rsext4::error::Errno::ENAMETOOLONG => ax_errno::LinuxError::ENAMETOOLONG,
        rsext4::error::Errno::ELOOP => ax_errno::LinuxError::ELOOP,
        rsext4::error::Errno::ENOMEM => ax_errno::LinuxError::ENOMEM,
        rsext4::error::Errno::EPERM => ax_errno::LinuxError::EPERM,
        _ => ax_errno::LinuxError::EIO,
    };
    VfsError::from(linux_err).canonicalize()
}

pub fn dir_entry_type_to_vfs(file_type: u8) -> NodeType {
    match file_type {
        Ext4DirEntry2::EXT4_FT_REG_FILE => NodeType::RegularFile,
        Ext4DirEntry2::EXT4_FT_DIR => NodeType::Directory,
        Ext4DirEntry2::EXT4_FT_CHRDEV => NodeType::CharacterDevice,
        Ext4DirEntry2::EXT4_FT_BLKDEV => NodeType::BlockDevice,
        Ext4DirEntry2::EXT4_FT_FIFO => NodeType::Fifo,
        Ext4DirEntry2::EXT4_FT_SOCK => NodeType::Socket,
        Ext4DirEntry2::EXT4_FT_SYMLINK => NodeType::Symlink,
        _ => NodeType::Unknown,
    }
}

pub fn inode_to_vfs_type(is_dir: bool, is_file: bool, is_symlink: bool) -> NodeType {
    if is_dir {
        NodeType::Directory
    } else if is_file {
        NodeType::RegularFile
    } else if is_symlink {
        NodeType::Symlink
    } else {
        NodeType::Unknown
    }
}

pub fn vfs_type_to_dir_entry(ty: NodeType) -> Option<u8> {
    Some(match ty {
        NodeType::RegularFile => Ext4DirEntry2::EXT4_FT_REG_FILE,
        NodeType::Directory => Ext4DirEntry2::EXT4_FT_DIR,
        NodeType::CharacterDevice => Ext4DirEntry2::EXT4_FT_CHRDEV,
        NodeType::BlockDevice => Ext4DirEntry2::EXT4_FT_BLKDEV,
        NodeType::Fifo => Ext4DirEntry2::EXT4_FT_FIFO,
        NodeType::Socket => Ext4DirEntry2::EXT4_FT_SOCK,
        NodeType::Symlink => Ext4DirEntry2::EXT4_FT_SYMLINK,
        NodeType::Unknown => return None,
    })
}
