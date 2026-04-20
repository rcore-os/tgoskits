use axfs_ng_vfs::{NodeType, VfsError};
use rsext4::{Ext4Error, entries::Ext4DirEntry2};

pub fn into_vfs_err(_err: Ext4Error) -> VfsError {
    VfsError::from(ax_errno::LinuxError::EIO).canonicalize()
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
