use crate::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlkError {
    NotSupported,
    Retry,
    NoMemory,
    InvalidBlockIndex(u64),
    InvalidRequest,
    Io,
    Other(&'static str),
}

impl core::fmt::Display for BlkError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BlkError::NotSupported => f.write_str("operation not supported"),
            BlkError::Retry => f.write_str("operation should be retried"),
            BlkError::NoMemory => f.write_str("insufficient memory"),
            BlkError::InvalidBlockIndex(index) => write!(f, "invalid block index: {index}"),
            BlkError::InvalidRequest => f.write_str("invalid block request"),
            BlkError::Io => f.write_str("block I/O error"),
            BlkError::Other(msg) => f.write_str(msg),
        }
    }
}

impl core::error::Error for BlkError {}

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
