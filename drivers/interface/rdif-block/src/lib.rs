#![no_std]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

pub use dma_api;
pub use rdif_base::{DriverGeneric, KError, io};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlkError {
    NotSupported,
    Retry,
    NoMemory,
    InvalidBlockIndex(u64),
    InvalidRequest,
    Io,
    Other(&'static str),
}

impl core::fmt::Display for BlkError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BlkError::NotSupported => f.write_str("operation not supported"),
            BlkError::Retry => f.write_str("operation should be retried"),
            BlkError::NoMemory => f.write_str("insufficient memory"),
            BlkError::InvalidBlockIndex(index) => write!(f, "invalid block index: {index}"),
            BlkError::InvalidRequest => f.write_str("invalid block request"),
            BlkError::Io => f.write_str("block I/O error"),
            BlkError::Other(msg) => f.write_str(msg),
        }
    }
}

impl core::error::Error for BlkError {}

impl From<BlkError> for io::ErrorKind {
    fn from(value: BlkError) -> Self {
        match value {
            BlkError::NotSupported => io::ErrorKind::Unsupported,
            BlkError::Retry => io::ErrorKind::Interrupted,
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

#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo {
    pub num_blocks: u64,
    pub logical_block_size: usize,
    pub physical_block_size: usize,
    pub read_only: bool,
    pub name: Option<&'static str>,
    pub vendor: Option<&'static str>,
    pub model: Option<&'static str>,
}

impl DeviceInfo {
    pub const fn new(num_blocks: u64, logical_block_size: usize) -> Self {
        Self {
            num_blocks,
            logical_block_size,
            physical_block_size: logical_block_size,
            read_only: false,
            name: None,
            vendor: None,
            model: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct QueueLimits {
    pub dma_mask: u64,
    pub dma_alignment: usize,
    pub max_blocks_per_request: u32,
    pub max_segments: usize,
    pub max_segment_size: usize,
    pub supports_flush: bool,
    pub supports_discard: bool,
    pub supports_write_zeroes: bool,
}

impl QueueLimits {
    pub const fn simple(logical_block_size: usize, dma_mask: u64) -> Self {
        Self {
            dma_mask,
            dma_alignment: logical_block_size,
            max_blocks_per_request: u32::MAX,
            max_segments: 1,
            max_segment_size: usize::MAX,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct QueueTopology {
    pub max_queues: usize,
    pub default_queue_depth: usize,
    pub poll_queue_count: usize,
}

impl QueueTopology {
    pub const fn single(depth: usize) -> Self {
        Self {
            max_queues: 1,
            default_queue_depth: depth,
            poll_queue_count: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueMode {
    Interrupt,
    Polled,
}

#[derive(Debug, Clone, Copy)]
pub struct QueueConfig {
    pub id_hint: Option<usize>,
    pub depth: usize,
    pub mode: QueueMode,
}

impl QueueConfig {
    pub const fn new(depth: usize) -> Self {
        Self {
            id_hint: None,
            depth,
            mode: QueueMode::Interrupt,
        }
    }
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self::new(1)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct QueueInfo {
    pub id: usize,
    pub depth: usize,
    pub mode: QueueMode,
    pub device: DeviceInfo,
    pub limits: QueueLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqSourceInfo {
    pub id: usize,
    pub queues: IdList,
}

impl IrqSourceInfo {
    pub const fn new(id: usize, queues: IdList) -> Self {
        Self { id, queues }
    }

    pub const fn legacy(queues: IdList) -> Self {
        Self { id: 0, queues }
    }
}

pub type IrqSourceList = Vec<IrqSourceInfo>;

pub trait Interface: DriverGeneric {
    fn device_info(&self) -> DeviceInfo;

    fn queue_limits(&self) -> QueueLimits;

    fn queue_topology(&self) -> QueueTopology;

    fn create_queue(&mut self, config: QueueConfig) -> Option<Box<dyn IQueue>>;

    fn enable_irq(&self) {}

    fn disable_irq(&self) {}

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn irq_sources(&self) -> IrqSourceList {
        Vec::new()
    }

    fn take_irq_handler(&mut self, _source_id: usize) -> Option<Box<dyn IrqHandler>> {
        None
    }
}

pub trait IrqHandler: Send + Sync + 'static {
    fn handle_irq(&self) -> Event;
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdList(u64);

impl IdList {
    pub const fn none() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub fn contains(&self, id: usize) -> bool {
        id < 64 && (self.0 & (1 << id)) != 0
    }

    pub fn insert(&mut self, id: usize) {
        if id < 64 {
            self.0 |= 1 << id;
        }
    }

    pub fn remove(&mut self, id: usize) {
        if id < 64 {
            self.0 &= !(1 << id);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> {
        (0..64).filter(move |i| self.contains(*i))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Event {
    pub queues: IdList,
}

impl Event {
    pub const fn none() -> Self {
        Self {
            queues: IdList::none(),
        }
    }

    pub const fn from_queue_bits(bits: u64) -> Self {
        Self {
            queues: IdList::from_bits(bits),
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(usize);

impl RequestId {
    pub const fn new(id: usize) -> Self {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOp {
    Read,
    Write,
    Flush,
    Discard,
    WriteZeroes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestFlags(u32);

impl RequestFlags {
    pub const NONE: Self = Self(0);
    pub const FUA: Self = Self(1 << 0);
    pub const PREFLUSH: Self = Self(1 << 1);
    pub const SYNC: Self = Self(1 << 2);
    pub const META: Self = Self(1 << 3);
    pub const POLLED: Self = Self(1 << 4);
    pub const NOWAIT: Self = Self(1 << 5);

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl core::ops::BitOr for RequestFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for RequestFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl Default for RequestFlags {
    fn default() -> Self {
        Self::NONE
    }
}

#[derive(Clone, Copy)]
pub struct Segment<'a> {
    pub virt: *mut u8,
    pub bus: u64,
    pub len: usize,
    _marker: PhantomData<&'a mut [u8]>,
}

impl<'a> Segment<'a> {
    /// Creates a block I/O segment from caller-owned CPU and DMA addresses.
    ///
    /// # Safety
    ///
    /// `virt` must be valid for reads and writes of `len` bytes for the
    /// whole request lifetime, and `bus` must be the DMA/bus address for the
    /// same storage. The caller must keep the buffer and DMA mapping alive
    /// until `poll_request` reports `RequestStatus::Complete`.
    pub unsafe fn from_raw_parts(virt: *mut u8, bus: u64, len: usize) -> Self {
        Self {
            virt,
            bus,
            len,
            _marker: PhantomData,
        }
    }
}

impl Deref for Segment<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        unsafe { core::slice::from_raw_parts(self.virt, self.len) }
    }
}

impl DerefMut for Segment<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { core::slice::from_raw_parts_mut(self.virt, self.len) }
    }
}

pub type Buffer<'a> = Segment<'a>;

pub struct Request<'a> {
    pub op: RequestOp,
    pub lba: u64,
    pub block_count: u32,
    pub segments: &'a mut [Segment<'a>],
    pub flags: RequestFlags,
}

impl Request<'_> {
    pub fn data_len(&self) -> usize {
        self.segments.iter().map(|segment| segment.len).sum()
    }

    pub fn is_data_op(&self) -> bool {
        matches!(self.op, RequestOp::Read | RequestOp::Write)
    }
}

pub trait IQueue: Send + 'static {
    fn id(&self) -> usize;

    fn info(&self) -> QueueInfo;

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError>;

    fn poll_request(&mut self, request: RequestId) -> Result<RequestStatus, BlkError>;
}

pub fn validate_request_shape(
    info: DeviceInfo,
    limits: QueueLimits,
    request: &Request<'_>,
) -> Result<(), BlkError> {
    if request.block_count == 0 && !matches!(request.op, RequestOp::Flush) {
        return Err(BlkError::InvalidRequest);
    }

    if request.lba >= info.num_blocks
        || request
            .lba
            .checked_add(request.block_count as u64)
            .is_none_or(|end| end > info.num_blocks)
    {
        return Err(BlkError::InvalidBlockIndex(request.lba));
    }

    match request.op {
        RequestOp::Read | RequestOp::Write => {
            let expected = request
                .block_count
                .checked_mul(info.logical_block_size as u32)
                .map(|len| len as usize)
                .ok_or(BlkError::InvalidRequest)?;
            if request.segments.is_empty()
                || request.segments.len() > limits.max_segments
                || request.data_len() != expected
            {
                return Err(BlkError::InvalidRequest);
            }
            if request
                .segments
                .iter()
                .any(|segment| segment.len > limits.max_segment_size)
            {
                return Err(BlkError::InvalidRequest);
            }
        }
        RequestOp::Flush => {
            if !request.segments.is_empty() || request.block_count != 0 {
                return Err(BlkError::InvalidRequest);
            }
            if !limits.supports_flush {
                return Err(BlkError::NotSupported);
            }
        }
        RequestOp::Discard => {
            if !request.segments.is_empty() {
                return Err(BlkError::InvalidRequest);
            }
            if !limits.supports_discard {
                return Err(BlkError::NotSupported);
            }
        }
        RequestOp::WriteZeroes => {
            if !request.segments.is_empty() {
                return Err(BlkError::InvalidRequest);
            }
            if !limits.supports_write_zeroes {
                return Err(BlkError::NotSupported);
            }
        }
    }

    if request.block_count > limits.max_blocks_per_request {
        return Err(BlkError::InvalidRequest);
    }

    Ok(())
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
    fn segment_carries_cpu_and_dma_addresses() {
        let mut bytes = [0x5a_u8; 4];
        let segment = unsafe { Segment::from_raw_parts(bytes.as_mut_ptr(), 0x1000, bytes.len()) };

        assert_eq!(segment.bus, 0x1000);
        assert_eq!(&*segment, &[0x5a; 4]);
    }

    #[test]
    fn request_shape_checks_lba_and_segments() {
        let info = DeviceInfo::new(8, 512);
        let limits = QueueLimits::simple(512, u64::MAX);
        let mut bytes = [0_u8; 1024];
        let segment = unsafe { Segment::from_raw_parts(bytes.as_mut_ptr(), 0x1000, bytes.len()) };
        let mut segments = [segment];
        let request = Request {
            op: RequestOp::Read,
            lba: 1,
            block_count: 2,
            segments: &mut segments,
            flags: RequestFlags::NONE,
        };

        assert_eq!(validate_request_shape(info, limits, &request), Ok(()));
    }

    #[test]
    fn request_shape_rejects_wrong_segment_size() {
        let info = DeviceInfo::new(8, 512);
        let limits = QueueLimits::simple(512, u64::MAX);
        let mut bytes = [0_u8; 512];
        let segment = unsafe { Segment::from_raw_parts(bytes.as_mut_ptr(), 0x1000, bytes.len()) };
        let mut segments = [segment];
        let request = Request {
            op: RequestOp::Write,
            lba: 1,
            block_count: 2,
            segments: &mut segments,
            flags: RequestFlags::NONE,
        };

        assert_eq!(
            validate_request_shape(info, limits, &request),
            Err(BlkError::InvalidRequest)
        );
    }

    struct NoopIrq;

    impl IrqHandler for NoopIrq {
        fn handle_irq(&self) -> Event {
            let mut event = Event::none();
            event.queues.insert(1);
            event
        }
    }

    struct Queue;

    impl IQueue for Queue {
        fn id(&self) -> usize {
            1
        }

        fn info(&self) -> QueueInfo {
            QueueInfo {
                id: 1,
                depth: 8,
                mode: QueueMode::Interrupt,
                device: DeviceInfo::new(8, 512),
                limits: QueueLimits::simple(512, u64::MAX),
            }
        }

        fn submit_request(&mut self, _request: Request<'_>) -> Result<RequestId, BlkError> {
            Ok(RequestId::new(1))
        }

        fn poll_request(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
            Ok(RequestStatus::Complete)
        }
    }

    #[test]
    fn block_api_uses_unified_queue_and_irq_events() {
        fn assert_queue<T: IQueue>() {}
        fn assert_irq_handler<T: IrqHandler>() {}

        assert_queue::<Queue>();
        assert_irq_handler::<NoopIrq>();

        let event = NoopIrq.handle_irq();
        assert!(event.queues.contains(1));
    }

    #[test]
    fn irq_source_lists_queue_masks() {
        let mut queues = IdList::none();
        queues.insert(2);
        let source = IrqSourceInfo::legacy(queues);

        assert_eq!(source.id, 0);
        assert!(source.queues.contains(2));
    }
}
