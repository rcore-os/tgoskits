use alloc::boxed::Box;

use crate::io;

#[derive(thiserror::Error, Debug)]
pub enum VsockError {
    #[error("operation not supported")]
    NotSupported,
    #[error("operation should be retried")]
    Retry,
    #[error("connection not found")]
    NotConnected,
    #[error("connection already exists")]
    AlreadyExists,
    #[error("device is not available")]
    NotAvailable,
    #[error("other error: {0}")]
    Other(Box<dyn core::error::Error>),
}

impl From<VsockError> for io::ErrorKind {
    fn from(value: VsockError) -> Self {
        match value {
            VsockError::NotSupported => io::ErrorKind::Unsupported,
            VsockError::Retry => io::ErrorKind::Interrupted,
            VsockError::NotConnected => io::ErrorKind::BrokenPipe,
            VsockError::AlreadyExists => io::ErrorKind::NotAvailable,
            VsockError::NotAvailable => io::ErrorKind::NotAvailable,
            VsockError::Other(error) => io::ErrorKind::Other(error),
        }
    }
}
