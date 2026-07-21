use crate::{LifecycleKind, RequestId, io};

/// Failure reported by a block device or queue operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BlkError {
    #[error("operation not supported")]
    NotSupported,
    /// The queue did not accept the request and staging may retry it later.
    #[error("operation should be retried")]
    Retry,
    #[error("accepted request cannot currently make progress")]
    Busy,
    #[error("block request timed out")]
    TimedOut,
    #[error("block request was cancelled")]
    Cancelled,
    #[error("block device is offline")]
    Offline,
    #[error("block request backing was quarantined")]
    Quarantined,
    /// A hardware-visible identifier generation can no longer advance without
    /// a queue quiesce and epoch change. The rejected request remains wholly
    /// software-owned; the runtime must recover/reinitialize before retrying.
    #[error("block queue identifier generation requires a new hardware epoch")]
    QueueEpochExhausted,
    #[error("DMA-quiescence proof does not belong to this queue's controller epoch")]
    InvalidDmaProof,
    #[error("insufficient memory")]
    NoMemory,
    #[error("invalid block index: {0}")]
    InvalidBlockIndex(u64),
    #[error("invalid block request")]
    InvalidRequest,
    #[error("block I/O error")]
    Io,
    #[error("{0}")]
    Other(&'static str),
}

/// Invalid queue/interrupt capability wiring detected before activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QueueContractError {
    #[error("block controller exposed no logical devices")]
    MissingLogicalDevices,
    #[error("block controller exposed duplicate logical device ID {device_id}")]
    DuplicateLogicalDeviceId { device_id: usize },
    #[error("logical block device {device_id} exposed no queues")]
    LogicalDeviceHasNoQueues { device_id: usize },
    #[error("block controller exposed duplicate queue ID {queue_id}")]
    DuplicateControllerQueueId { queue_id: usize },
    #[error(
        "block queue identity mismatch: driver advertises {advertised_id}, metadata names \
         {metadata_id}"
    )]
    QueueIdentityMismatch {
        advertised_id: usize,
        metadata_id: usize,
    },
    #[error("interrupt queue {queue_id} completed synchronously during submission")]
    SynchronousInterruptCompletion { queue_id: usize },
    #[error("inline queue {queue_id} retained a request after submission returned")]
    QueuedInlineRequest { queue_id: usize },
    #[error("inline queue {queue_id} requires RequestId::INLINE")]
    InlineRequestIdentityRequired { queue_id: usize },
    #[error("interrupt queue {queue_id} requires a generation-bearing request identity")]
    InterruptRequestIdentityRequired { queue_id: usize },
    #[error("queue {queue_id} returned request ID {returned:?} for submission {expected:?}")]
    SubmitRequestIdMismatch {
        queue_id: usize,
        expected: RequestId,
        returned: RequestId,
    },
    #[error("block controller queue ID {queue_id} is outside 0..64")]
    InvalidControllerQueueId { queue_id: usize },
    #[error("queue {queue_id} exposes invalid block-device geometry")]
    InvalidDeviceGeometry { queue_id: usize },
    #[error("queue {queue_id} exposes unusable request or DMA limits")]
    InvalidQueueLimits { queue_id: usize },
    #[error("queue {queue_id} completion kind and execution contract are inconsistent")]
    QueueExecutionMismatch { queue_id: usize },
    #[error(
        "queue {queue_id} metadata does not match logical block device {device_id} geometry and \
         limits"
    )]
    QueueDeviceMetadataMismatch { device_id: usize, queue_id: usize },
    #[error(
        "block controller lifecycle mismatch: queues require {expected:?}, interface provides \
         {actual:?}"
    )]
    LifecycleMismatch {
        expected: LifecycleKind,
        actual: LifecycleKind,
    },
    #[error("interrupt block controller lifecycle has no stable nonzero identity")]
    InvalidLifecycleIdentity,
    #[error("interrupt queue {queue_id} already has a bound controller identity")]
    LifecycleIdentityAlreadyBound { queue_id: usize },
    #[error("interrupt queue {queue_id} declares no logical interrupt sources")]
    MissingInterruptSources { queue_id: usize },
    #[error("interrupt queue {queue_id} declares no watchdog budget")]
    MissingWatchdog { queue_id: usize },
    #[error("queue {queue_id} requires undeclared logical interrupt source {source_id}")]
    UndeclaredInterruptSource { queue_id: usize, source_id: usize },
    #[error("queue {queue_id} requires unbound logical interrupt source {source_id}")]
    UnboundInterruptSource { queue_id: usize, source_id: usize },
}

/// Owner-side failure while reopening one device-masked interrupt source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IrqControlError {
    #[error("stale IRQ source generation: expected {expected}, got {actual}")]
    StaleGeneration { expected: u64, actual: u64 },
    #[error("stale IRQ source mask epoch: expected {expected}, got {actual}")]
    StaleMaskEpoch { expected: u64, actual: u64 },
    #[error("IRQ source bitmap {bitmap:#x} is not owned by the masked capture")]
    SourceNotMasked { bitmap: u64 },
    #[error("IRQ source owner is offline")]
    Offline,
    #[error("device IRQ control failed: {0}")]
    Hardware(#[from] BlkError),
}

impl From<BlkError> for io::ErrorKind {
    fn from(value: BlkError) -> Self {
        match value {
            BlkError::NotSupported => io::ErrorKind::Unsupported,
            BlkError::Retry | BlkError::Busy | BlkError::Cancelled => io::ErrorKind::Interrupted,
            BlkError::TimedOut => io::ErrorKind::TimedOut,
            BlkError::Offline | BlkError::Quarantined | BlkError::QueueEpochExhausted => {
                io::ErrorKind::NotAvailable
            }
            BlkError::InvalidDmaProof => io::ErrorKind::InvalidParameter {
                name: "DMA-quiescence proof",
            },
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
