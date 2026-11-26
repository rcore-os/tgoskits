//! Error conversion at the generic I/O and VFS boundary.

use ax_io::IoError;

use crate::VfsError;

/// Adapt a VFS-domain failure while implementing an `ax-io` capability.
pub fn vfs_error_to_io_error(error: VfsError) -> IoError {
    match error {
        VfsError::AlreadyExists => IoError::AlreadyExists,
        VfsError::BadAddress => IoError::BadAddress,
        VfsError::BadFileDescriptor => IoError::BadFileDescriptor,
        VfsError::BadState => IoError::BadState,
        VfsError::CrossesDevices => IoError::CrossesDevices,
        // `ax-io` has no xattr-missing category. Preserve the exact ENODATA
        // identity in VFS/ABI users and use the closest data-domain fallback
        // only at this narrower capability boundary.
        VfsError::DataMissing => IoError::InvalidData,
        VfsError::DirectoryNotEmpty => IoError::DirectoryNotEmpty,
        VfsError::FilesystemCorrupted => IoError::InvalidData,
        VfsError::FilesystemLoop => IoError::FilesystemLoop,
        VfsError::FileTooLarge => IoError::FileTooLarge,
        VfsError::InvalidData => IoError::InvalidData,
        VfsError::InvalidInput => IoError::InvalidInput,
        VfsError::Interrupted => IoError::Interrupted,
        VfsError::Io => IoError::Io,
        VfsError::IsADirectory => IoError::IsADirectory,
        VfsError::NameTooLong => IoError::NameTooLong,
        VfsError::NoMemory => IoError::NoMemory,
        VfsError::NoSuchDevice => IoError::NoSuchDevice,
        VfsError::NoSuchDeviceOrAddress => IoError::NoSuchDeviceOrAddress,
        VfsError::NotADirectory => IoError::NotADirectory,
        VfsError::NotATty => IoError::NotATty,
        VfsError::NotFound => IoError::NotFound,
        VfsError::OperationNotPermitted => IoError::OperationNotPermitted,
        VfsError::OperationNotSupported => IoError::OperationNotSupported,
        VfsError::PermissionDenied => IoError::PermissionDenied,
        VfsError::QuotaExceeded => IoError::StorageFull,
        VfsError::ReadOnlyFilesystem => IoError::ReadOnlyFilesystem,
        VfsError::ResourceBusy => IoError::ResourceBusy,
        VfsError::StorageFull => IoError::StorageFull,
        VfsError::TextFileBusy => IoError::ResourceBusy,
        VfsError::TimedOut => IoError::TimedOut,
        // The ETIME/ETIMEDOUT split only matters to direct VFS consumers
        // (DRM syncobj waits); `ax-io` has no separate category, so the
        // deadline flavor folds into the plain timeout here.
        VfsError::TimerExpired => IoError::TimedOut,
        VfsError::TooManyLinks => IoError::Io,
        VfsError::Unsupported => IoError::Unsupported,
        VfsError::ValueOverflow => IoError::OutOfRange,
        VfsError::WouldBlock => IoError::WouldBlock,
    }
}

/// Adapt an `ax-io` failure while implementing a VFS operation.
pub fn io_error_to_vfs_error(error: IoError) -> VfsError {
    match error {
        IoError::AlreadyExists => VfsError::AlreadyExists,
        IoError::BadAddress => VfsError::BadAddress,
        IoError::BadFileDescriptor => VfsError::BadFileDescriptor,
        IoError::BadState => VfsError::BadState,
        IoError::CrossesDevices => VfsError::CrossesDevices,
        IoError::DirectoryNotEmpty => VfsError::DirectoryNotEmpty,
        IoError::FilesystemLoop => VfsError::FilesystemLoop,
        IoError::FileTooLarge => VfsError::FileTooLarge,
        IoError::InvalidData => VfsError::InvalidData,
        IoError::InvalidInput => VfsError::InvalidInput,
        IoError::Interrupted => VfsError::Interrupted,
        IoError::IsADirectory => VfsError::IsADirectory,
        IoError::NameTooLong => VfsError::NameTooLong,
        IoError::NoMemory => VfsError::NoMemory,
        IoError::NoSuchDevice => VfsError::NoSuchDevice,
        IoError::NoSuchDeviceOrAddress => VfsError::NoSuchDeviceOrAddress,
        IoError::NotADirectory => VfsError::NotADirectory,
        IoError::NotATty => VfsError::NotATty,
        IoError::NotFound => VfsError::NotFound,
        IoError::OperationNotPermitted => VfsError::OperationNotPermitted,
        IoError::OperationNotSupported => VfsError::OperationNotSupported,
        IoError::PermissionDenied => VfsError::PermissionDenied,
        IoError::ReadOnlyFilesystem => VfsError::ReadOnlyFilesystem,
        IoError::ResourceBusy => VfsError::ResourceBusy,
        IoError::StorageFull => VfsError::StorageFull,
        IoError::TimedOut => VfsError::TimedOut,
        IoError::Unsupported => VfsError::Unsupported,
        IoError::WouldBlock => VfsError::WouldBlock,
        _ => VfsError::Io,
    }
}
