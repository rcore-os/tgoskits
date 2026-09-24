//! Errors returned by the VirtIO GPU protocol core.

use thiserror::Error as ThisError;

/// Error type of the VirtIO GPU protocol core.
///
/// Domain failures that callers can act on are named directly; anything else
/// from the underlying virtio transport or driver is kept as
/// [`Error::VirtIo`] so no information is lost.
#[derive(Debug, ThisError)]
#[non_exhaustive]
pub enum Error {
    /// The device did not negotiate the feature this command needs.
    #[error("the device did not negotiate the required feature")]
    Unsupported,
    /// The device is not in a state that supports the requested operation.
    #[error("the device is not ready")]
    NotReady,
    /// A parameter supplied by the caller is invalid.
    #[error("invalid parameter")]
    InvalidParam,
    /// The device answered with something other than the expected response.
    #[error("unexpected response from the device")]
    InvalidResponse,
    /// The response does not fit into the receive buffer.
    #[error("the device response does not fit into the receive buffer")]
    ResponseTooLarge,
    /// The request does not fit into the control send buffer.
    #[error("the request does not fit into the control send buffer")]
    RequestTooLarge,
    /// A size or length computation overflowed.
    #[error("size arithmetic overflowed")]
    Overflow,
    /// DMA memory could not be allocated.
    #[error("failed to allocate DMA memory")]
    DmaError,
    /// Every control-queue slot is live (ring full, parking FIFO at cap) and
    /// the host has not completed anything, so no slot can be reclaimed.
    ///
    /// Returned instead of blocking: this crate cannot sleep, and its callers
    /// may hold a global lock while calling in. Linux blocks at the same
    /// boundary (`virtio_gpu_queue_ctrl_sgs` waits on `ctrlq.ackq` for free
    /// vbufs), so this error is a deliberate, documented deviation: the
    /// consumer retries once completions have been reclaimed.
    #[error("control queue is exhausted and the host is not draining; retry")]
    QueueBusy,
    /// A waited-for completion did not arrive within the wait timeout.
    ///
    /// The bounded waits (`wait_fence`, `wait_idle`) exist so a stalled or
    /// dead host unwedges the caller instead of spinning forever; after this
    /// error the queue itself is still usable once the host resumes.
    #[error("the device did not complete the waited-for work in time")]
    TimedOut,
    /// The device reported a used entry that does not belong to any submission
    /// on this queue (a foreign completion).
    ///
    /// The used ring is FIFO and this crate cannot consume a foreign entry,
    /// so no later completion can ever be reclaimed: the queue rejects all
    /// further operations with this error instead of wedging silently.
    #[error("control queue is broken: the device reported a foreign completion")]
    QueueBroken,
    /// Failure reported by the underlying virtio transport or driver.
    #[error("virtio error: {0}")]
    VirtIo(virtio_drivers::Error),
}

impl From<virtio_drivers::Error> for Error {
    fn from(err: virtio_drivers::Error) -> Self {
        // Keep the domain meaning of the failures callers branch on and wrap
        // everything else unchanged.
        match err {
            virtio_drivers::Error::Unsupported => Error::Unsupported,
            virtio_drivers::Error::NotReady => Error::NotReady,
            virtio_drivers::Error::InvalidParam => Error::InvalidParam,
            other => Error::VirtIo(other),
        }
    }
}
