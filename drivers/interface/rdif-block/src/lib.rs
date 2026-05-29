#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::ops::{Deref, DerefMut};

pub use dma_api;
pub use rdif_base::{DriverGeneric, KError, io};

/// Configuration for DMA buffer allocation.
///
/// This structure specifies the requirements for DMA buffers used in
/// block device operations. The configuration ensures that buffers
/// meet the hardware's alignment and addressing constraints.
pub struct BuffConfig {
    /// DMA addressing mask for the device.
    ///
    /// This mask defines the addressable memory range for DMA operations.
    /// For example, a 32-bit device would use `0xFFFFFFFF`.
    pub dma_mask: u64,

    /// Required alignment for buffer addresses.
    ///
    /// Buffers must be aligned to this boundary (in bytes) for optimal
    /// performance and hardware compatibility. Common values are 512 or 4096.
    pub align: usize,

    /// Size of each buffer in bytes.
    ///
    /// This typically matches the device's block size to ensure efficient
    /// data transfer and avoid partial block operations.
    pub size: usize,
}

/// Errors that can occur during block device operations.
///
/// These errors provide detailed information about what went wrong during
/// block device operations and how the caller should respond.
#[derive(thiserror::Error, Debug)]
pub enum BlkError {
    /// The requested operation is not supported by the device.
    ///
    /// This error occurs when attempting to perform an operation that the
    /// hardware or driver does not support. For example, trying to write
    /// to a read-only device.
    ///
    /// **Recovery**: Check device capabilities and use only supported operations.
    #[error("Operation not supported")]
    NotSupported,

    /// The operation should be retried later.
    ///
    /// This error indicates that the operation failed due to temporary conditions
    /// and should be retried. This commonly occurs when:
    /// - The device queue is full
    /// - The device is temporarily busy
    /// - Resource contention prevents immediate completion
    ///
    /// **Recovery**: Wait a short time and retry the operation. Consider implementing
    /// exponential backoff for repeated retries.
    #[error("Operation should be retried")]
    Retry,

    /// Insufficient memory to complete the operation.
    ///
    /// This error occurs when there is not enough memory available to:
    /// - Allocate DMA buffers
    /// - Create internal data structures
    /// - Complete the requested operation
    ///
    /// **Recovery**: Free unused resources or wait for memory to become available.
    /// Consider reducing the number of concurrent operations.
    #[error("Insufficient memory")]
    NoMemory,

    /// The specified block index is invalid or out of range.
    ///
    /// This error occurs when:
    /// - The block index exceeds the device's capacity
    /// - The block index is negative (in languages that allow it)
    /// - The block has been marked as bad or unusable
    ///
    /// **Recovery**: Verify that the block index is within the valid range
    /// (0 to `num_blocks() - 1`) and that the block is accessible.
    #[error("Invalid block index: {0} (check device capacity and block accessibility)")]
    InvalidBlockIndex(usize),

    /// An unspecified error occurred.
    ///
    /// This error wraps other error types that don't fit into the specific
    /// categories above. The wrapped error provides additional context about
    /// what went wrong.
    ///
    /// **Recovery**: Examine the wrapped error for specific recovery instructions.
    /// This often indicates a lower-level hardware or system error.
    #[error("Other error: {0}")]
    Other(Box<dyn core::error::Error>),
}

impl From<BlkError> for io::ErrorKind {
    fn from(value: BlkError) -> Self {
        match value {
            BlkError::NotSupported => io::ErrorKind::Unsupported,
            BlkError::Retry => io::ErrorKind::Interrupted,
            BlkError::NoMemory => io::ErrorKind::OutOfMemory,
            BlkError::InvalidBlockIndex(_) => io::ErrorKind::NotAvailable,
            BlkError::Other(e) => io::ErrorKind::Other(e),
        }
    }
}

impl From<dma_api::DmaError> for BlkError {
    fn from(value: dma_api::DmaError) -> Self {
        match value {
            dma_api::DmaError::NoMemory => BlkError::NoMemory,
            e => BlkError::Other(Box::new(e)),
        }
    }
}

/// Operations that require a block storage device driver to implement.
///
/// This trait defines the core interface that all block device drivers
/// must implement to work with the rdrive framework. It provides methods
/// for queue management, interrupt handling, and device lifecycle operations.
pub trait Interface: DriverGeneric {
    fn create_queue(&mut self) -> Option<Box<dyn IQueue>>;

    /// Enable interrupts for the device.
    ///
    /// After calling this method, the device will generate interrupts
    /// for completed operations and other events.
    fn enable_irq(&mut self);

    /// Disable interrupts for the device.
    ///
    /// After calling this method, the device will not generate interrupts.
    /// This is useful during critical sections or device shutdown.
    fn disable_irq(&mut self);

    /// Check if interrupts are currently enabled.
    ///
    /// Returns `true` if interrupts are enabled, `false` otherwise.
    fn is_irq_enabled(&self) -> bool;

    /// Handles an IRQ from the device, returning an event if applicable.
    ///
    /// This method should be called from the device's interrupt handler.
    /// It processes the interrupt and returns information about which
    /// queues have completed operations.
    fn handle_irq(&mut self) -> Event;
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct IdList(u64);

impl IdList {
    pub const fn none() -> Self {
        Self(0)
    }

    pub fn contains(&self, id: usize) -> bool {
        (self.0 & (1 << id)) != 0
    }

    pub fn insert(&mut self, id: usize) {
        self.0 |= 1 << id;
    }

    pub fn remove(&mut self, id: usize) {
        self.0 &= !(1 << id);
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> {
        (0..64).filter(move |i| self.contains(*i))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Event {
    /// Bitmask of queue IDs that have events.
    pub queue: IdList,
}

impl Event {
    pub const fn none() -> Self {
        Self {
            queue: IdList::none(),
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(usize);

impl RequestId {
    pub fn new(id: usize) -> Self {
        Self(id)
    }
}

impl From<RequestId> for usize {
    fn from(value: RequestId) -> Self {
        value.0
    }
}

#[derive(Clone, Copy)]
pub struct Buffer {
    pub virt: *mut u8,
    pub bus: u64,
    pub size: usize,
}

impl Deref for Buffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        unsafe { core::slice::from_raw_parts(self.virt, self.size) }
    }
}

impl DerefMut for Buffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { core::slice::from_raw_parts_mut(self.virt, self.size) }
    }
}

/// Read queue trait for block devices.
pub trait IQueue: Send + 'static {
    /// Get the queue identifier.
    fn id(&self) -> usize;

    /// Get the total number of blocks available.
    fn num_blocks(&self) -> usize;

    /// Get the size of each block in bytes.
    fn block_size(&self) -> usize;

    /// Get the buffer configuration for this queue.
    fn buff_config(&self) -> BuffConfig;

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError>;

    /// Try to read blocks directly into the caller-provided buffer.
    ///
    /// Implementations may return [`BlkError::NotSupported`] when the buffer is
    /// not suitable for direct DMA or the device does not provide a direct path.
    fn read_blocks_direct(&mut self, _block_id: usize, _buf: &mut [u8]) -> Result<(), BlkError> {
        Err(BlkError::NotSupported)
    }

    /// Poll the status of a previously submitted request.
    fn poll_request(&mut self, request: RequestId) -> Result<(), BlkError>;
}

#[derive(Clone)]
pub struct Request<'a> {
    pub block_id: usize,
    pub kind: RequestKind<'a>,
}

#[derive(Clone)]
pub enum RequestKind<'a> {
    Read(Buffer),
    Write(&'a [u8]),
}
