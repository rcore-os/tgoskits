//! Operating System Abstraction Layer (OSAL) for RKNPU device layer
//!
//! This module provides platform-agnostic abstractions for system-dependent operations
//! such as memory management, time operations, and synchronization primitives.

/// Physical address type
pub type PhysAddr = u64;

/// DMA address type  
pub type DmaAddr = u64;

/// Time type for timestamps
pub type TimeStamp = u64;

/// Error types for OSAL operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsalError {
    OutOfMemory,
    InvalidParameter,
    TimeoutError,
    DeviceError,
    NotSupported,
}
