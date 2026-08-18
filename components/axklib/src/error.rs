/// Errors owned by the kernel capability boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum KlibError {
    #[error("kernel capability input is invalid")]
    InvalidInput,
    #[error("kernel capability allocation failed")]
    NoMemory,
    #[error("kernel mapping already exists")]
    AlreadyExists,
    #[error("kernel address is invalid")]
    BadAddress,
    #[error("kernel capability is in an invalid state")]
    BadState,
    #[error("kernel capability is unsupported")]
    Unsupported,
    #[error("kernel capability operation timed out")]
    TimedOut,
    #[error("kernel capability resource is busy")]
    ResourceBusy,
    #[error("kernel capability resource was not found")]
    NotFound,
    #[error("kernel capability I/O failed")]
    Io,
}

/// A result returned by a [`crate::Klib`] capability.
pub type KlibResult<T = ()> = Result<T, KlibError>;
