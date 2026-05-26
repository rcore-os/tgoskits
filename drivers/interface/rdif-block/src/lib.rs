#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

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
/// This trait defines the device-level block capability boundary. Data
/// movement is split into independent read and write queues; IRQ event
/// extraction is exposed through a separately owned handler.
pub trait Interface: DriverGeneric {
    fn create_read_queue(&mut self) -> Option<Box<dyn IReadQueue>>;

    fn create_write_queue(&mut self) -> Option<Box<dyn IWriteQueue>>;

    /// Enable interrupts for the device.
    ///
    /// After calling this method, the device will generate interrupts
    /// for completed operations and other events.
    fn enable_irq(&self) {}

    /// Disable interrupts for the device.
    ///
    /// After calling this method, the device will not generate interrupts.
    /// This is useful during critical sections or device shutdown.
    fn disable_irq(&self) {}

    /// Check if interrupts are currently enabled.
    ///
    /// Returns `true` if interrupts are enabled, `false` otherwise.
    fn is_irq_enabled(&self) -> bool {
        false
    }

    /// Take the device IRQ event handler.
    ///
    /// IRQ-capable drivers should normally return `Some` exactly once and
    /// `None` afterwards. Polling-only drivers may keep the default.
    fn take_irq_handler(&mut self) -> Option<Box<dyn IrqHandler>> {
        None
    }
}

/// Lock-free IRQ event extraction for a block device.
pub trait IrqHandler: Send + Sync + 'static {
    /// Handles an IRQ from the device, returning queue event masks.
    fn handle_irq(&self) -> Event;
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
    /// Bitmask of read queue IDs that have events.
    pub read_queue: IdList,
    /// Bitmask of write queue IDs that have events.
    pub write_queue: IdList,
}

impl Event {
    pub const fn none() -> Self {
        Self {
            read_queue: IdList::none(),
            write_queue: IdList::none(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStatus {
    Pending,
    Complete,
}

#[derive(Clone, Copy)]
pub struct Buffer<'a> {
    pub virt: *mut u8,
    pub bus: u64,
    pub size: usize,
    _marker: PhantomData<&'a mut [u8]>,
}

impl<'a> Buffer<'a> {
    /// Creates a block I/O buffer from caller-owned CPU and DMA addresses.
    ///
    /// # Safety
    ///
    /// `virt` must be valid for reads and writes of `size` bytes for the
    /// whole request lifetime, and `bus` must be the DMA/bus address for the
    /// same storage. The caller must keep the buffer and DMA mapping alive
    /// until `poll_request` reports `RequestStatus::Complete`.
    pub unsafe fn from_raw_parts(virt: *mut u8, bus: u64, size: usize) -> Self {
        Self {
            virt,
            bus,
            size,
            _marker: PhantomData,
        }
    }
}

impl Deref for Buffer<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        unsafe { core::slice::from_raw_parts(self.virt, self.size) }
    }
}

impl DerefMut for Buffer<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { core::slice::from_raw_parts_mut(self.virt, self.size) }
    }
}

/// Common information exposed by block read and write queues.
pub trait QueueInfo {
    /// Get the queue identifier.
    fn id(&self) -> usize;

    /// Get the total number of blocks available.
    fn num_blocks(&self) -> usize;

    /// Get the size of each block in bytes.
    fn block_size(&self) -> usize;

    /// Get the buffer configuration for this queue.
    fn buffer_config(&self) -> BuffConfig;
}

/// Read queue trait for block devices.
pub trait IReadQueue: QueueInfo + Send + 'static {
    fn submit_read(&mut self, request: RequestRead<'_>) -> Result<RequestId, BlkError>;

    /// Poll the status of a previously submitted request.
    fn poll_read(&mut self, request: RequestId) -> Result<RequestStatus, BlkError>;
}

/// Write queue trait for block devices.
pub trait IWriteQueue: QueueInfo + Send + 'static {
    fn submit_write(&mut self, request: RequestWrite<'_>) -> Result<RequestId, BlkError>;

    /// Poll the status of a previously submitted request.
    fn poll_write(&mut self, request: RequestId) -> Result<RequestStatus, BlkError>;
}

#[derive(Clone)]
pub struct RequestRead<'a> {
    pub block_id: usize,
    pub buffer: Buffer<'a>,
}

#[derive(Clone)]
pub struct RequestWrite<'a> {
    pub block_id: usize,
    pub buffer: Buffer<'a>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_status_distinguishes_pending_from_errors() {
        assert_eq!(RequestStatus::Pending, RequestStatus::Pending);
        assert_ne!(RequestStatus::Pending, RequestStatus::Complete);
    }

    #[test]
    fn write_request_uses_dma_buffer_shape() {
        let mut bytes = [0x5a_u8; 4];
        let buffer = unsafe { Buffer::from_raw_parts(bytes.as_mut_ptr(), 0x1000, bytes.len()) };
        let request = RequestWrite {
            block_id: 7,
            buffer,
        };

        assert_eq!(request.block_id, 7);
        assert_eq!(request.buffer.bus, 0x1000);
        assert_eq!(&*request.buffer, &[0x5a; 4]);
    }

    struct NoopIrq;

    impl IrqHandler for NoopIrq {
        fn handle_irq(&self) -> Event {
            let mut event = Event::none();
            event.read_queue.insert(1);
            event.write_queue.insert(2);
            event
        }
    }

    #[test]
    fn block_api_separates_read_write_queues_and_irq_handler() {
        fn assert_read_queue<T: IReadQueue>() {}
        fn assert_write_queue<T: IWriteQueue>() {}
        fn assert_irq_handler<T: IrqHandler>() {}

        struct ReadOnly;
        struct WriteOnly;

        impl QueueInfo for ReadOnly {
            fn id(&self) -> usize {
                1
            }

            fn num_blocks(&self) -> usize {
                8
            }

            fn block_size(&self) -> usize {
                512
            }

            fn buffer_config(&self) -> BuffConfig {
                BuffConfig {
                    dma_mask: u64::MAX,
                    align: 512,
                    size: 512,
                }
            }
        }

        impl IReadQueue for ReadOnly {
            fn submit_read(&mut self, _request: RequestRead<'_>) -> Result<RequestId, BlkError> {
                Ok(RequestId::new(1))
            }

            fn poll_read(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
                Ok(RequestStatus::Complete)
            }
        }

        impl QueueInfo for WriteOnly {
            fn id(&self) -> usize {
                2
            }

            fn num_blocks(&self) -> usize {
                8
            }

            fn block_size(&self) -> usize {
                512
            }

            fn buffer_config(&self) -> BuffConfig {
                BuffConfig {
                    dma_mask: u64::MAX,
                    align: 512,
                    size: 512,
                }
            }
        }

        impl IWriteQueue for WriteOnly {
            fn submit_write(&mut self, _request: RequestWrite<'_>) -> Result<RequestId, BlkError> {
                Ok(RequestId::new(2))
            }

            fn poll_write(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
                Ok(RequestStatus::Complete)
            }
        }

        assert_read_queue::<ReadOnly>();
        assert_write_queue::<WriteOnly>();
        assert_irq_handler::<NoopIrq>();

        let event = NoopIrq.handle_irq();
        assert!(event.read_queue.contains(1));
        assert!(event.write_queue.contains(2));
    }
}
