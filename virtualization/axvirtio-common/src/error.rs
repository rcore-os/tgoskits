//! VirtIO error types and runtime error conversion.
//!
//! This module defines common error types used across all VirtIO device implementations.

use alloc::format;

use axdevice_base::DeviceError;

/// VirtIO specific error types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioError {
    /// Invalid queue configuration
    InvalidQueue,
    /// Queue not ready for operation (not configured yet)
    QueueNotReady,
    /// Invalid descriptor
    InvalidDescriptor,
    /// Invalid access width for MMIO operation
    InvalidAccessWidth,
    /// Device not ready
    DeviceNotReady,
    /// Invalid device index
    InvalidDeviceIndex,
    /// Backend operation failed
    BackendError,
    /// Memory access error
    MemoryError,
    /// Invalid configuration
    InvalidConfig,
    /// Feature negotiation failed
    FeatureNegotiationFailed,
    /// Invalid request
    InvalidRequest,
    /// Operation not supported
    NotSupported,
    /// Invalid buffer size
    InvalidBufferSize,
    /// Invalid sector
    InvalidSector,
    /// Invalid register
    InvalidRegister,
    /// Invalid address or address translation failed
    InvalidAddress,
    /// One or more virtqueue rings are misaligned (desc 16B, avail 2B, used 4B)
    RingMisaligned,
    /// The virtqueue ring regions overlap each other
    RingOverlap,
    /// The virtqueue ring layout is invalid (zero address, region overflow, or
    /// a ring region lies outside the guest address space)
    InvalidRingLayout,
    /// The queue is faulted after a runtime ring/descriptor failure and must be
    /// reset before further use. Unlike [`QueueNotReady`](Self::QueueNotReady),
    /// which is a normal pre-configuration state, a faulted queue has served
    /// requests and hit a runtime failure; its guest-serving data paths
    /// (`pop`/`complete`, chain walks and data access) reject with this error
    /// and write no guest memory until `reset`, while the configuration
    /// setters remain usable.
    QueueFaulted,
    /// Resource not found
    NotFound,
    /// The operation is valid but cannot complete until asynchronous backend
    /// work makes progress.
    WouldBlock,
    /// Invalid input
    InvalidInput,
}

/// Result type for VirtIO operations
pub type VirtioResult<T> = Result<T, VirtioError>;

/// Maps a VirtIO error to the runtime category that determines the caller's
/// recovery action.
pub fn map_virtio_error(error: VirtioError, operation: &'static str) -> DeviceError {
    match error {
        VirtioError::BackendError => DeviceError::Backend {
            operation,
            detail: format!("{error:?}"),
        },
        VirtioError::QueueFaulted => DeviceError::InvalidState {
            operation,
            detail: "queue faulted; guest reset required".into(),
        },
        VirtioError::QueueNotReady | VirtioError::DeviceNotReady => DeviceError::InvalidState {
            operation,
            detail: format!("{error:?}"),
        },
        VirtioError::WouldBlock => DeviceError::ResourceBusy {
            operation,
            resource: "VirtIO queue or reset transition".into(),
        },
        VirtioError::NotSupported => DeviceError::Unsupported {
            operation,
            detail: format!("{error:?}"),
        },
        VirtioError::MemoryError | VirtioError::InvalidAddress => DeviceError::InvalidData {
            operation,
            detail: format!("{error:?}"),
        },
        VirtioError::InvalidAccessWidth
        | VirtioError::InvalidDeviceIndex
        | VirtioError::InvalidRegister
        | VirtioError::InvalidRequest
        | VirtioError::FeatureNegotiationFailed
        | VirtioError::InvalidInput => DeviceError::InvalidInput {
            operation,
            detail: format!("{error:?}"),
        },
        VirtioError::NotFound => DeviceError::NotFound,
        VirtioError::InvalidQueue
        | VirtioError::InvalidDescriptor
        | VirtioError::InvalidConfig
        | VirtioError::InvalidBufferSize
        | VirtioError::InvalidSector
        | VirtioError::RingMisaligned
        | VirtioError::RingOverlap
        | VirtioError::InvalidRingLayout => DeviceError::InvalidData {
            operation,
            detail: format!("{error:?}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_runtime_error_categories() {
        assert!(matches!(
            map_virtio_error(VirtioError::BackendError, "test operation"),
            DeviceError::Backend { .. }
        ));
        assert!(matches!(
            map_virtio_error(VirtioError::QueueFaulted, "test operation"),
            DeviceError::InvalidState { .. }
        ));
        assert!(matches!(
            map_virtio_error(VirtioError::WouldBlock, "test operation"),
            DeviceError::ResourceBusy { .. }
        ));
        assert!(matches!(
            map_virtio_error(VirtioError::InvalidAddress, "test operation"),
            DeviceError::InvalidData { .. }
        ));
    }
}
