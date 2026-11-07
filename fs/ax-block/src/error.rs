use irq_framework::IrqError;
use rdif_block::{BlkError, RequestOp};

/// Errors owned by the block runtime and its OS capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BlockError {
    #[error("block request is invalid")]
    InvalidRequest,
    #[error("block runtime is in an invalid state")]
    InvalidState,
    #[error("block runtime capability is not installed")]
    RuntimeUnavailable,
    #[error("block operation would block in this context")]
    WouldBlock,
    #[error("block runtime allocation failed")]
    NoMemory,
    #[error("block runtime operation is unsupported")]
    Unsupported,
    #[error("block operation timed out")]
    TimedOut,
    #[error("block runtime I/O failed")]
    Io,
    #[error("block runtime resource is busy")]
    ResourceBusy,
    #[error("block runtime resource was not found")]
    NotFound,
    /// The interrupt capability rejected a block-device IRQ operation.
    #[error(transparent)]
    Irq(#[from] IrqError),
    #[error("block {operation:?} at LBA {lba} failed during {stage}: {source}")]
    Device {
        stage: &'static str,
        operation: RequestOp,
        lba: u64,
        #[source]
        source: BlkError,
    },
}

/// A result returned by the block runtime.
pub type BlockResult<T = ()> = Result<T, BlockError>;

impl From<BlkError> for BlockError {
    fn from(error: BlkError) -> Self {
        match error {
            BlkError::NotSupported => Self::Unsupported,
            BlkError::Retry => Self::WouldBlock,
            BlkError::NoMemory => Self::NoMemory,
            BlkError::InvalidBlockIndex(_) | BlkError::InvalidRequest => Self::InvalidRequest,
            BlkError::TimedOut => Self::TimedOut,
            BlkError::Io | BlkError::Other(_) => Self::Io,
        }
    }
}
