use thiserror::Error;

/// V4L2 操作结果。
pub type Result<T> = core::result::Result<T, V4l2Error>;

/// V4L2 错误码。
#[derive(Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum V4l2Error {
    #[error("invalid argument")]
    InvalidArgument,
    #[error("no such device")]
    NoSuchDevice,
    #[error("I/O error")]
    Io,
    #[error("operation not supported")]
    NotSupported,
    #[error("device or resource busy")]
    Busy,
    #[error("timed out")]
    Timeout,
    #[error("out of memory")]
    NoMemory,
    #[error("access denied")]
    AccessDenied,
    #[error("bad file descriptor")]
    BadFileDescriptor,
    #[error("try again")]
    WouldBlock,
    #[error("no such file or entry")]
    NoEntry,
    #[error("no such device or address")]
    NoSuchDeviceOrAddress,
    #[error("operation not permitted")]
    OperationNotPermitted,
    #[error("interrupted")]
    Interrupted,
    #[error("inappropriate ioctl for device")]
    NotATty,
    #[error("no space left on device")]
    StorageFull,
    #[error("result out of range")]
    OutOfRange,
    #[error("message too long")]
    MessageTooLong,
}
