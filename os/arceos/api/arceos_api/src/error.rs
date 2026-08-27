#[cfg(feature = "fs")]
use ax_fs_ng::VfsError;
use ax_io::IoError;
#[cfg(feature = "net")]
use ax_net::NetError;
use ax_runtime::RuntimeError;
use ax_runtime::task::TaskError;

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
    /// A scheduler operation failed in the task domain.
    #[error(transparent)]
    Task(#[from] TaskError),
    /// A public API argument is outside its accepted domain.
    #[error("invalid ArceOS API input")]
    InvalidInput,
    /// The selected object or policy does not support this operation.
    #[error("ArceOS API operation is not supported")]
    OperationNotSupported,
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
            ApiError::Task(error) => task_error_to_io_error(error),
            ApiError::InvalidInput => Self::InvalidInput,
            ApiError::OperationNotSupported => Self::OperationNotSupported,
            ApiError::PriorityUpdateFailed | ApiError::AffinityUpdateFailed => Self::BadState,
        }
    }
}
fn task_error_to_io_error(error: TaskError) -> IoError {
    match error {
        TaskError::InvalidConfiguration
        | TaskError::InvalidCpuCount(_)
        | TaskError::InvalidCpu(_)
        | TaskError::InvalidNice(_)
        | TaskError::InvalidRtPriority(_)
        | TaskError::InvalidRoundRobinQuantum
        | TaskError::InvalidDeadline { .. }
        | TaskError::UnsupportedDeadlineFlags(_) => IoError::InvalidInput,
        TaskError::DeadlineAdmission
        | TaskError::DeadlineAffinity
        | TaskError::ActiveTimerAffinity
        | TaskError::ThreadBusy => IoError::ResourceBusy,
        // Linux copy_process() reports the global thread limit as EAGAIN.
        TaskError::ThreadCapacity => IoError::WouldBlock,
        TaskError::TimerCapacity => IoError::NoMemory,
        TaskError::UnsafeContext => IoError::OperationNotPermitted,
        TaskError::StaleThreadId => IoError::NotFound,
        TaskError::NotInitialized
        | TaskError::InvalidRuntimeHandle
        | TaskError::CpuOwnerBorrowed
        | TaskError::CpuOwnerMismatch { .. }
        | TaskError::ExecutorOwnerMismatch { .. }
        | TaskError::CpuAlreadyOnline(_)
        | TaskError::CpuOffline(_)
        | TaskError::CpuNotQuiescent(_)
        | TaskError::LastOnlineCpu(_)
        | TaskError::InvalidTransition { .. }
        | TaskError::AlreadyQueued
        | TaskError::NotReady
        | TaskError::NotExited
        | TaskError::NoRunnableThread
        | TaskError::InvalidPiState
        | TaskError::InvalidPiWaitState(_)
        | TaskError::PiCycle
        | TaskError::PiChainLimit { .. }
        | TaskError::RuntimeFailure(_) => IoError::BadState,
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
        VfsError::DirectoryNotEmpty => IoError::DirectoryNotEmpty,
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
        VfsError::ReadOnlyFilesystem => IoError::ReadOnlyFilesystem,
        VfsError::ResourceBusy => IoError::ResourceBusy,
        VfsError::StorageFull => IoError::StorageFull,
        VfsError::TimedOut => IoError::TimedOut,
        VfsError::Unsupported => IoError::Unsupported,
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
