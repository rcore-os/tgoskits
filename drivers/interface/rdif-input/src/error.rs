use alloc::boxed::Box;

use crate::io;

#[derive(thiserror::Error, Debug)]
pub enum InputError {
    #[error("operation not supported")]
    NotSupported,
    #[error("no event available")]
    Again,
    #[error("device is not available")]
    NotAvailable,
    #[error("invalid event")]
    InvalidEvent,
    #[error("other error: {0}")]
    Other(Box<dyn core::error::Error + Send + Sync>),
}

impl From<InputError> for io::ErrorKind {
    fn from(value: InputError) -> Self {
        match value {
            InputError::NotSupported => io::ErrorKind::Unsupported,
            InputError::Again => io::ErrorKind::Interrupted,
            InputError::NotAvailable => io::ErrorKind::NotAvailable,
            InputError::InvalidEvent => io::ErrorKind::InvalidData,
            InputError::Other(error) => io::ErrorKind::Other(error),
        }
    }
}
