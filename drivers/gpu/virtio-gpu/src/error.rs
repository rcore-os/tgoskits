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
