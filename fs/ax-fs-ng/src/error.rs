use ax_io::IoError;
use axfs_ng_vfs::VfsError;
use irq_framework::IrqError;
use rdif_block::{BlkError, RequestOp};

/// Errors owned by the filesystem block runtime and its OS capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BlockError {
    #[error("block request is invalid")]
    InvalidRequest,
    #[error("block runtime is in an invalid state")]
    InvalidState,
    #[error("block runtime capability is not installed")]
    RuntimeUnavailable,
    #[error("block operation would block in this context")]
    WouldBlock,
    #[error("block runtime allocation failed")]
    NoMemory,
    #[error("block runtime operation is unsupported")]
    Unsupported,
    #[error("block operation timed out")]
    TimedOut,
    #[error("block runtime I/O failed")]
    Io,
    #[error("block runtime resource is busy")]
    ResourceBusy,
    #[error("block runtime resource was not found")]
    NotFound,
    /// The interrupt capability rejected a block-device IRQ operation.
    #[error(transparent)]
    Irq(#[from] IrqError),
    #[error("block {operation:?} at LBA {lba} failed during {stage}: {source}")]
    Device {
        stage: &'static str,
        operation: RequestOp,
        lba: u64,
        #[source]
        source: BlkError,
    },
}

/// A result returned by the filesystem block runtime.
pub type BlockResult<T = ()> = Result<T, BlockError>;

impl From<BlkError> for BlockError {
    fn from(error: BlkError) -> Self {
        match error {
            BlkError::NotSupported => Self::Unsupported,
            BlkError::Retry => Self::WouldBlock,
            BlkError::NoMemory => Self::NoMemory,
            BlkError::InvalidBlockIndex(_) | BlkError::InvalidRequest => Self::InvalidRequest,
            BlkError::TimedOut => Self::TimedOut,
            BlkError::Io | BlkError::Other(_) => Self::Io,
        }
    }
}

/// Adapt a block-runtime failure at the VFS implementation boundary.
#[cfg(any(feature = "ext4", feature = "fat"))]
pub(crate) fn block_error_to_vfs_error(error: BlockError) -> VfsError {
    match error {
        BlockError::InvalidRequest => VfsError::InvalidInput,
        BlockError::InvalidState | BlockError::RuntimeUnavailable => VfsError::BadState,
        BlockError::WouldBlock => VfsError::WouldBlock,
        BlockError::NoMemory => VfsError::NoMemory,
        BlockError::Unsupported => VfsError::Unsupported,
        BlockError::TimedOut => VfsError::TimedOut,
        BlockError::Io => VfsError::Io,
        BlockError::Device { source, .. } => block_error_to_vfs_error(source.into()),
        BlockError::ResourceBusy => VfsError::ResourceBusy,
        BlockError::NotFound => VfsError::NotFound,
        BlockError::Irq(error) => match error {
            IrqError::InvalidIrq | IrqError::InvalidCpu => VfsError::InvalidInput,
            IrqError::CpuOffline | IrqError::Unsupported => VfsError::Unsupported,
            IrqError::Busy | IrqError::InIrqContext => VfsError::ResourceBusy,
            IrqError::Timeout => VfsError::TimedOut,
            IrqError::NoMemory => VfsError::NoMemory,
            IrqError::NotFound => VfsError::NotFound,
            IrqError::Controller => VfsError::Io,
        },
    }
}

/// Adapt a VFS-domain failure while implementing an `ax-io` capability.
pub(crate) fn vfs_error_to_io_error(error: VfsError) -> IoError {
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
        VfsError::TimedOut => IoError::TimedOut,
        VfsError::TooManyLinks => IoError::Io,
        VfsError::Unsupported => IoError::Unsupported,
        VfsError::ValueOverflow => IoError::OutOfRange,
        VfsError::WouldBlock => IoError::WouldBlock,
    }
}

/// Adapt an `ax-io` failure while implementing a VFS operation.
pub(crate) fn io_error_to_vfs_error(error: IoError) -> VfsError {
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

#[cfg(all(test, any(feature = "ext4", feature = "fat")))]
mod tests {
    use super::*;

    #[test]
    fn contextual_device_errors_keep_their_vfs_category() {
        let cases = [
            (BlkError::NotSupported, VfsError::Unsupported),
            (BlkError::Retry, VfsError::WouldBlock),
            (BlkError::NoMemory, VfsError::NoMemory),
            (BlkError::InvalidBlockIndex(7), VfsError::InvalidInput),
            (BlkError::InvalidRequest, VfsError::InvalidInput),
            (BlkError::TimedOut, VfsError::TimedOut),
            (BlkError::Io, VfsError::Io),
            (BlkError::Other("device failure"), VfsError::Io),
        ];

        for (source, expected) in cases {
            let error = BlockError::Device {
                stage: "submit",
                operation: RequestOp::Read,
                lba: 11,
                source,
            };
            assert!(core::error::Error::source(&error).is_some());
            assert_eq!(block_error_to_vfs_error(error), expected);
        }
    }
}
