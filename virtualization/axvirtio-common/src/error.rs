//! VirtIO Error Types
//!
//! This module defines common error types used across all VirtIO device implementations.

/// VirtIO specific error types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioError {
    /// Invalid queue configuration
    InvalidQueue,
    /// Queue not ready for operation
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
    /// Resource not found
    NotFound,
    /// Invalid input
    InvalidInput,
}

/// Result type for VirtIO operations
pub type VirtioResult<T> = Result<T, VirtioError>;
