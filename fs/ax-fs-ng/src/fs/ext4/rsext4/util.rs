use axfs_ng_vfs::{NodeType, VfsError};
use rsext4::{DirectoryEntryType, Ext4Error, Ext4ErrorKind};

pub fn into_vfs_err(err: Ext4Error) -> VfsError {
    let linux_err = match err.kind() {
        Ext4ErrorKind::NotFound => ax_errno::LinuxError::ENOENT,
        Ext4ErrorKind::AlreadyExists => ax_errno::LinuxError::EEXIST,
        Ext4ErrorKind::IsDirectory => ax_errno::LinuxError::EISDIR,
        Ext4ErrorKind::NotDirectory => ax_errno::LinuxError::ENOTDIR,
        Ext4ErrorKind::NotEmpty => ax_errno::LinuxError::ENOTEMPTY,
        Ext4ErrorKind::PermissionDenied => ax_errno::LinuxError::EACCES,
        Ext4ErrorKind::InvalidInput
        | Ext4ErrorKind::BadSuperblock
        | Ext4ErrorKind::InvalidMagic => ax_errno::LinuxError::EINVAL,
        Ext4ErrorKind::NoSpace => ax_errno::LinuxError::ENOSPC,
        Ext4ErrorKind::ReadOnly => ax_errno::LinuxError::EROFS,
        Ext4ErrorKind::Busy => ax_errno::LinuxError::EBUSY,
        Ext4ErrorKind::BadFileDescriptor => ax_errno::LinuxError::EBADF,
        Ext4ErrorKind::FileTooLarge => ax_errno::LinuxError::EFBIG,
        Ext4ErrorKind::Overflow => ax_errno::LinuxError::EOVERFLOW,
        Ext4ErrorKind::Timeout => ax_errno::LinuxError::ETIMEDOUT,
        Ext4ErrorKind::Unsupported
        | Ext4ErrorKind::UnsupportedFeature
        | Ext4ErrorKind::UnsupportedCapability => ax_errno::LinuxError::EOPNOTSUPP,
        Ext4ErrorKind::Corrupted | Ext4ErrorKind::ChecksumMismatch => ax_errno::LinuxError::EUCLEAN,
        Ext4ErrorKind::QuotaExceeded => ax_errno::LinuxError::EDQUOT,
        Ext4ErrorKind::TooManyLinks => ax_errno::LinuxError::EMLINK,
        Ext4ErrorKind::Io | Ext4ErrorKind::JournalAborted => ax_errno::LinuxError::EIO,
    };
    VfsError::from(linux_err).canonicalize()
}

pub fn directory_entry_type_to_vfs(file_type: DirectoryEntryType) -> NodeType {
    match file_type {
        DirectoryEntryType::Unknown => NodeType::Unknown,
        DirectoryEntryType::RegularFile => NodeType::RegularFile,
        DirectoryEntryType::Directory => NodeType::Directory,
        DirectoryEntryType::CharacterDevice => NodeType::CharacterDevice,
        DirectoryEntryType::BlockDevice => NodeType::BlockDevice,
        DirectoryEntryType::Fifo => NodeType::Fifo,
        DirectoryEntryType::Socket => NodeType::Socket,
        DirectoryEntryType::Symlink => NodeType::Symlink,
    }
}

#[cfg(test)]
mod tests {
    use rsext4::{Ext4Error, FeatureSet};

    use super::*;

    fn expected(error: ax_errno::LinuxError) -> VfsError {
        VfsError::from(error).canonicalize()
    }

    #[test]
    fn domain_errors_are_translated_only_at_the_vfs_boundary() {
        assert_eq!(
            into_vfs_err(Ext4Error::unsupported_feature(
                FeatureSet::Incompatible,
                0x8000_0000,
            )),
            expected(ax_errno::LinuxError::EOPNOTSUPP),
        );
        assert_eq!(
            into_vfs_err(Ext4Error::checksum()),
            expected(ax_errno::LinuxError::EUCLEAN),
        );
        assert_eq!(
            into_vfs_err(Ext4Error::overflow()),
            expected(ax_errno::LinuxError::EOVERFLOW),
        );
        assert_eq!(
            into_vfs_err(Ext4Error::journal_aborted()),
            expected(ax_errno::LinuxError::EIO),
        );
        assert_eq!(
            into_vfs_err(Ext4Error::too_many_links()),
            expected(ax_errno::LinuxError::EMLINK),
        );
    }
}
