#[cfg(feature = "fs")]
use ax_fs_ng::VfsError;
use ax_io::IoError;
#[cfg(feature = "net")]
use ax_net::NetError;
use ax_runtime::RuntimeError;

/// Errors owned by the public ArceOS API facade.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ApiError {
    /// A filesystem API failed in the VFS domain.
    #[cfg(feature = "fs")]
    #[error(transparent)]
    Vfs(#[from] VfsError),
    /// A socket API failed in the network domain.
    #[cfg(feature = "net")]
    #[error(transparent)]
    Net(#[from] NetError),
    /// A runtime-owned console operation failed.
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    /// The scheduler rejected a priority update.
    #[error("failed to update the current task priority")]
    PriorityUpdateFailed,
    /// The scheduler rejected an affinity update.
    #[error("failed to update the current task affinity")]
    AffinityUpdateFailed,
}

/// A result returned by the public ArceOS API facade.
pub type ApiResult<T = ()> = Result<T, ApiError>;

impl From<ApiError> for IoError {
    fn from(error: ApiError) -> Self {
        match error {
            #[cfg(feature = "fs")]
            ApiError::Vfs(error) => vfs_error_to_io_error(error),
            #[cfg(feature = "net")]
            ApiError::Net(error) => error.into(),
            ApiError::Runtime(error) => runtime_error_to_io_error(error),
            ApiError::PriorityUpdateFailed | ApiError::AffinityUpdateFailed => Self::BadState,
        }
    }
}

#[cfg(feature = "fs")]
fn vfs_error_to_io_error(error: VfsError) -> IoError {
    match error {
        VfsError::AlreadyExists => IoError::AlreadyExists,
        VfsError::BadAddress => IoError::BadAddress,
        VfsError::BadFileDescriptor => IoError::BadFileDescriptor,
        VfsError::BadState => IoError::BadState,
        VfsError::CrossesDevices => IoError::CrossesDevices,
        // These VFS categories have no exact `ax-io` representation. Keep
        // them exact for VFS/POSIX callers and degrade only at this facade.
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

fn runtime_error_to_io_error(error: RuntimeError) -> IoError {
    match error {
        RuntimeError::ConsoleFailedClosed => IoError::BadState,
        RuntimeError::SerialNotStarted => IoError::BadState,
        RuntimeError::SerialControlBusy => IoError::ResourceBusy,
        RuntimeError::WouldBlock => IoError::WouldBlock,
        RuntimeError::OperationNotSupported => IoError::OperationNotSupported,
        RuntimeError::InvalidCpu { .. } => IoError::InvalidInput,
        _ => IoError::Io,
    }
}
