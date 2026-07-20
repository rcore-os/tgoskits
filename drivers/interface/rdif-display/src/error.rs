use alloc::boxed::Box;

use crate::io;

#[derive(thiserror::Error, Debug)]
pub enum DisplayError {
    #[error("operation not supported")]
    NotSupported,
    #[error("device is not available")]
    NotAvailable,
    #[error("invalid framebuffer")]
    InvalidFramebuffer,
    #[error("other error: {0}")]
    Other(Box<dyn core::error::Error + Send + Sync>),
}

impl From<DisplayError> for io::ErrorKind {
    fn from(value: DisplayError) -> Self {
        match value {
            DisplayError::NotSupported => io::ErrorKind::Unsupported,
            DisplayError::NotAvailable => io::ErrorKind::NotAvailable,
            DisplayError::InvalidFramebuffer => io::ErrorKind::InvalidData,
            DisplayError::Other(error) => io::ErrorKind::Other(error),
        }
    }
}
