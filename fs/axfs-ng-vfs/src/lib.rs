#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod fs;
mod mount;
mod node;
pub mod path;
mod poll;
mod types;

pub use fs::*;
pub use mount::*;
pub use node::*;
pub use poll::*;
pub use types::*;

/// Errors owned by the virtual-filesystem domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VfsError {
    #[error("filesystem object already exists")]
    AlreadyExists,
    #[error("bad filesystem address")]
    BadAddress,
    #[error("bad file descriptor")]
    BadFileDescriptor,
    #[error("filesystem is in an invalid state")]
    BadState,
    #[error("operation crosses filesystem devices")]
    CrossesDevices,
    #[error("filesystem extended attribute data is missing")]
    DataMissing,
    #[error("directory is not empty")]
    DirectoryNotEmpty,
    #[error("filesystem metadata is corrupted")]
    FilesystemCorrupted,
    #[error("filesystem traversal loop detected")]
    FilesystemLoop,
    #[error("filesystem file is too large")]
    FileTooLarge,
    #[error("filesystem data is invalid")]
    InvalidData,
    #[error("filesystem input is invalid")]
    InvalidInput,
    #[error("filesystem operation was interrupted")]
    Interrupted,
    #[error("filesystem I/O failed")]
    Io,
    #[error("filesystem object is a directory")]
    IsADirectory,
    #[error("filesystem name is too long")]
    NameTooLong,
    #[error("filesystem allocation failed")]
    NoMemory,
    #[error("filesystem device does not exist")]
    NoSuchDevice,
    #[error("filesystem device or address does not exist")]
    NoSuchDeviceOrAddress,
    #[error("filesystem object is not a directory")]
    NotADirectory,
    #[error("filesystem object is not a tty")]
    NotATty,
    #[error("filesystem object was not found")]
    NotFound,
    #[error("filesystem operation is not permitted")]
    OperationNotPermitted,
    #[error("filesystem operation is not supported by this object")]
    OperationNotSupported,
    #[error("filesystem permission denied")]
    PermissionDenied,
    #[error("filesystem quota is exceeded")]
    QuotaExceeded,
    #[error("filesystem is read-only")]
    ReadOnlyFilesystem,
    #[error("filesystem resource is busy")]
    ResourceBusy,
    #[error("filesystem storage is full")]
    StorageFull,
    #[error("filesystem operation timed out")]
    TimedOut,
    #[error("filesystem object has too many links")]
    TooManyLinks,
    #[error("filesystem operation is not implemented")]
    Unsupported,
    #[error("filesystem operation would block")]
    WouldBlock,
    #[error("filesystem value cannot be represented")]
    ValueOverflow,
}

pub type VfsResult<T = ()> = Result<T, VfsError>;

pub type Mutex<T> = ax_sync::SpinLock<T>;
pub type MutexGuard<'a, T> = ax_sync::SpinLockGuard<'a, T>;
