use crate::io;

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum BlkError {
    #[error("operation not supported")]
    NotSupported,
    #[error("operation should be retried")]
    Retry,
    #[error("insufficient memory")]
    NoMemory,
    #[error("invalid block index: {0}")]
    InvalidBlockIndex(u64),
    #[error("invalid block request")]
    InvalidRequest,
    #[error("block I/O timed out")]
    TimedOut,
    #[error("block I/O error")]
    Io,
    #[error("{0}")]
    Other(&'static str),
}

impl From<BlkError> for io::ErrorKind {
    fn from(value: BlkError) -> Self {
        match value {
            BlkError::NotSupported => io::ErrorKind::Unsupported,
            BlkError::Retry => io::ErrorKind::Interrupted,
            BlkError::NoMemory => io::ErrorKind::OutOfMemory,
            BlkError::InvalidBlockIndex(_) => io::ErrorKind::NotAvailable,
            BlkError::InvalidRequest => io::ErrorKind::InvalidParameter {
                name: "block request",
            },
            BlkError::TimedOut => io::ErrorKind::TimedOut,
            BlkError::Io => io::ErrorKind::Other("block I/O error".into()),
            BlkError::Other(msg) => io::ErrorKind::Other(msg.into()),
        }
    }
}

impl From<dma_api::DmaError> for BlkError {
    fn from(value: dma_api::DmaError) -> Self {
        match value {
            dma_api::DmaError::NoMemory => BlkError::NoMemory,
            _ => BlkError::Io,
        }
    }
}
