//! VirtIO-net error types.

use axvirtio_common::VirtioError;

/// Errors returned by the network device's public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    /// Device/driver not ready (no `DRIVER_OK` or the relevant queue is not ready).
    NotReady,
    /// A required feature (e.g. `VIRTIO_NET_F_STATUS`) was not negotiated.
    FeatureNotNegotiated,
    /// Link is down.
    LinkDown,
    /// Malformed guest descriptor chain.
    InvalidDescriptor,
    /// Guest TX header requested an offload this device does not support.
    UnsupportedOffload,
    /// Frame exceeds the configured maximum size.
    FrameTooLarge,
    /// Guest memory could not be read or written.
    GuestMemoryFault,
    /// A queue operation failed.
    Queue(VirtioError),
    /// The host backend rejected the operation.
    Backend(crate::NetworkBackendError),
}

impl core::fmt::Display for NetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotReady => write!(f, "virtio-net device not ready"),
            Self::FeatureNotNegotiated => write!(f, "required virtio-net feature not negotiated"),
            Self::LinkDown => write!(f, "virtio-net link is down"),
            Self::InvalidDescriptor => write!(f, "invalid virtio-net descriptor chain"),
            Self::UnsupportedOffload => write!(f, "unsupported virtio-net offload requested"),
            Self::FrameTooLarge => write!(f, "virtio-net frame exceeds maximum size"),
            Self::GuestMemoryFault => write!(f, "virtio-net guest memory access failed"),
            Self::Queue(e) => write!(f, "virtio-net queue error: {e:?}"),
            Self::Backend(e) => write!(f, "virtio-net backend error: {e}"),
        }
    }
}

impl core::error::Error for NetError {}

impl From<VirtioError> for NetError {
    fn from(e: VirtioError) -> Self {
        match e {
            VirtioError::InvalidDescriptor | VirtioError::InvalidQueue => Self::InvalidDescriptor,
            VirtioError::QueueNotReady | VirtioError::DeviceNotReady => Self::NotReady,
            VirtioError::InvalidAddress | VirtioError::MemoryError => Self::GuestMemoryFault,
            other => Self::Queue(other),
        }
    }
}

impl From<crate::NetworkBackendError> for NetError {
    fn from(e: crate::NetworkBackendError) -> Self {
        Self::Backend(e)
    }
}

/// Errors returned by a [`NetworkBackend`](crate::NetworkBackend).
///
/// Concrete backends with richer error information map their internal errors
/// into this stable type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkBackendError {
    /// The backend could not transmit the frame.
    TransmitFailed,
}

impl core::fmt::Display for NetworkBackendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TransmitFailed => write!(f, "network backend transmit failed"),
        }
    }
}

impl core::error::Error for NetworkBackendError {}
