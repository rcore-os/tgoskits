use alloc::boxed::Box;

use crate::io;

/// Specific error kinds for 3D GPU operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gpu3dErrorKind {
    IoError,
    Unsupported,
    NotReady,
    InvalidParam,
    Other,
}

#[derive(thiserror::Error, Debug)]
pub enum DisplayError {
    #[error("operation not supported")]
    NotSupported,
    #[error("device is not available")]
    NotAvailable,
    #[error("invalid framebuffer")]
    InvalidFramebuffer,
    #[error("GPU 3D error: {0:?}")]
    Gpu3dError(Gpu3dErrorKind),
    #[error("other error: {0}")]
    Other(Box<dyn core::error::Error>),
}

impl From<DisplayError> for io::ErrorKind {
    fn from(value: DisplayError) -> Self {
        match value {
            DisplayError::NotSupported => io::ErrorKind::Unsupported,
            DisplayError::NotAvailable => io::ErrorKind::NotAvailable,
            DisplayError::InvalidFramebuffer => io::ErrorKind::InvalidData,
            e @ DisplayError::Gpu3dError(_) => io::ErrorKind::Other(Box::new(e)),
            DisplayError::Other(error) => io::ErrorKind::Other(error),
        }
    }
}
