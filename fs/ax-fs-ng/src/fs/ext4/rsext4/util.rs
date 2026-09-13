use axfs_ng_vfs::{NodeType, VfsError};
use rsext4::{DirectoryEntryType, Ext4Error, Ext4ErrorKind};

pub fn into_vfs_err(err: Ext4Error) -> VfsError {
    match err.kind() {
        Ext4ErrorKind::NotFound => VfsError::NotFound,
        Ext4ErrorKind::AlreadyExists => VfsError::AlreadyExists,
        Ext4ErrorKind::IsDirectory => VfsError::IsADirectory,
        Ext4ErrorKind::NotDirectory => VfsError::NotADirectory,
        Ext4ErrorKind::NotEmpty => VfsError::DirectoryNotEmpty,
        Ext4ErrorKind::PermissionDenied => VfsError::PermissionDenied,
        Ext4ErrorKind::InvalidInput
        | Ext4ErrorKind::BadSuperblock
        | Ext4ErrorKind::InvalidMagic => VfsError::InvalidInput,
        Ext4ErrorKind::NoSpace => VfsError::StorageFull,
        Ext4ErrorKind::NoMemory => VfsError::NoMemory,
        Ext4ErrorKind::ReadOnly => VfsError::ReadOnlyFilesystem,
        Ext4ErrorKind::Busy => VfsError::ResourceBusy,
        Ext4ErrorKind::BadFileDescriptor => VfsError::BadFileDescriptor,
        Ext4ErrorKind::FileTooLarge => VfsError::FileTooLarge,
        Ext4ErrorKind::Overflow => VfsError::ValueOverflow,
        Ext4ErrorKind::Timeout => VfsError::TimedOut,
        Ext4ErrorKind::Unsupported
        | Ext4ErrorKind::UnsupportedFeature
        | Ext4ErrorKind::UnsupportedCapability => VfsError::OperationNotSupported,
        Ext4ErrorKind::Corrupted | Ext4ErrorKind::ChecksumMismatch => VfsError::FilesystemCorrupted,
        Ext4ErrorKind::QuotaExceeded => VfsError::QuotaExceeded,
        Ext4ErrorKind::TooManyLinks => VfsError::TooManyLinks,
        Ext4ErrorKind::Io | Ext4ErrorKind::JournalAborted => VfsError::Io,
    }
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
    use rsext4::{Ext4Error, Ext4ErrorKind, FeatureSet};

    use super::*;

    #[test]
    fn domain_errors_are_translated_only_at_the_vfs_boundary() {
        assert_eq!(
            into_vfs_err(Ext4Error::unsupported_feature(
                FeatureSet::Incompatible,
                0x8000_0000,
            )),
            VfsError::OperationNotSupported,
        );
        assert_eq!(
            into_vfs_err(Ext4Error::checksum()),
            VfsError::FilesystemCorrupted,
        );
        assert_eq!(into_vfs_err(Ext4Error::overflow()), VfsError::ValueOverflow,);
        assert_eq!(into_vfs_err(Ext4Error::no_memory()), VfsError::NoMemory,);
        assert_eq!(into_vfs_err(Ext4Error::journal_aborted()), VfsError::Io,);
        assert_eq!(
            into_vfs_err(Ext4Error::too_many_links()),
            VfsError::TooManyLinks,
        );
        assert_eq!(
            into_vfs_err(Ext4Error::new(Ext4ErrorKind::QuotaExceeded)),
            VfsError::QuotaExceeded,
        );
    }
}
