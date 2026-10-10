//! Errors returned by the VirtIO GPU protocol core.

use thiserror::Error as ThisError;

/// Error type of the VirtIO GPU protocol core.
///
/// Domain failures that callers can act on are named directly. Transport
/// errors are retained as [`Error::VirtIo`] unless command completion is
/// ambiguous and the device must be reset.
#[derive(Debug, ThisError)]
#[non_exhaustive]
pub enum Error {
    /// The device did not negotiate the feature this command needs.
    #[error("the device did not negotiate the required feature")]
    Unsupported,
    /// The device is not in a state that supports the requested operation.
    #[error("the device is not ready")]
    NotReady,
    /// The device was reset because a command's completion could not be confirmed.
    #[error("the device was lost after an unconfirmed command")]
    DeviceLost,
    /// A parameter supplied by the caller is invalid.
    #[error("invalid parameter")]
    InvalidParam,
    /// The device answered with something other than the expected response.
    #[error("unexpected response from the device")]
    InvalidResponse,
    /// The device explicitly rejected a command with this VirtIO GPU response code.
    #[error("device rejected command with response code {0:#x}")]
    DeviceRejected(u32),
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
    /// The queue was invalidated (device reset) or the device reported a used
    /// entry that belongs to no submission on this queue.
    ///
    /// The used ring is FIFO and this crate cannot consume a foreign entry,
    /// so no later completion can ever be reclaimed: the queue rejects all
    /// further operations with this error instead of wedging silently. The
    /// consumer must treat the device as lost and reset it.
    #[error("control queue is broken: the queue was reset or reported a foreign completion")]
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
