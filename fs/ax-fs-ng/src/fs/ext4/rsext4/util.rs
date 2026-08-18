use axfs_ng_vfs::{NodeType, VfsError};
use rsext4::{Ext4Error, entries::Ext4DirEntry2};

pub fn into_vfs_err(err: Ext4Error) -> VfsError {
    match err.code {
        rsext4::error::Errno::ENOENT => VfsError::NotFound,
        rsext4::error::Errno::EEXIST => VfsError::AlreadyExists,
        rsext4::error::Errno::EISDIR => VfsError::IsADirectory,
        rsext4::error::Errno::ENOTDIR => VfsError::NotADirectory,
        rsext4::error::Errno::ENOTEMPTY => VfsError::DirectoryNotEmpty,
        rsext4::error::Errno::EACCES => VfsError::PermissionDenied,
        rsext4::error::Errno::EINVAL => VfsError::InvalidInput,
        rsext4::error::Errno::EFBIG => VfsError::FileTooLarge,
        rsext4::error::Errno::ENOSPC => VfsError::StorageFull,
        rsext4::error::Errno::EROFS => VfsError::ReadOnlyFilesystem,
        rsext4::error::Errno::EBUSY => VfsError::ResourceBusy,
        rsext4::error::Errno::EBADF => VfsError::BadFileDescriptor,
        rsext4::error::Errno::ENAMETOOLONG => VfsError::NameTooLong,
        rsext4::error::Errno::ELOOP => VfsError::FilesystemLoop,
        rsext4::error::Errno::ENOMEM => VfsError::NoMemory,
        rsext4::error::Errno::EPERM => VfsError::OperationNotPermitted,
        _ => VfsError::Io,
    }
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

pub fn inode_to_vfs_type(i_mode: u16) -> NodeType {
    use rsext4::disknode::Ext4Inode;
    match i_mode & Ext4Inode::S_IFMT {
        Ext4Inode::S_IFDIR => NodeType::Directory,
        Ext4Inode::S_IFREG => NodeType::RegularFile,
        Ext4Inode::S_IFLNK => NodeType::Symlink,
        Ext4Inode::S_IFCHR => NodeType::CharacterDevice,
        Ext4Inode::S_IFBLK => NodeType::BlockDevice,
        Ext4Inode::S_IFIFO => NodeType::Fifo,
        Ext4Inode::S_IFSOCK => NodeType::Socket,
        _ => NodeType::Unknown,
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
