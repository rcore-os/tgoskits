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
    pub max_transfer_size: usize,
    pub preferred_transfer_size: usize,
    pub supported_flags: RequestFlags,
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
            max_transfer_size: usize::MAX,
            preferred_transfer_size: logical_block_size,
            supported_flags: RequestFlags::NONE,
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

#[derive(Debug, Clone, Copy)]
pub struct TransferRuntimeCaps {
    pub max_transfer_bytes: usize,
    pub max_segments: usize,
}

impl TransferRuntimeCaps {
    pub const fn new(max_transfer_bytes: usize, max_segments: usize) -> Self {
        Self {
            max_transfer_bytes,
            max_segments,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferSegment {
    /// Segment byte offset relative to the containing transfer chunk.
    pub byte_offset: usize,
    pub byte_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferChunk {
    pub lba: u64,
    pub block_count: u32,
    pub byte_offset: usize,
    pub byte_len: usize,
    max_segment_size: usize,
}

impl TransferChunk {
    pub fn segments(self) -> TransferSegments {
        TransferSegments {
            remaining_len: self.byte_len,
            byte_offset: 0,
            max_segment_size: self.max_segment_size,
        }
    }
}

pub struct TransferSegments {
    remaining_len: usize,
    byte_offset: usize,
    max_segment_size: usize,
}

impl Iterator for TransferSegments {
    type Item = TransferSegment;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining_len == 0 {
            return None;
        }

        let byte_len = self.remaining_len.min(self.max_segment_size);
        let segment = TransferSegment {
            byte_offset: self.byte_offset,
            byte_len,
        };
        self.byte_offset += byte_len;
        self.remaining_len -= byte_len;
        Some(segment)
    }
}

impl ExactSizeIterator for TransferSegments {
    fn len(&self) -> usize {
        self.remaining_len.div_ceil(self.max_segment_size)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TransferPlanner {
    device: DeviceInfo,
    limits: QueueLimits,
    max_chunk_size: usize,
}

impl TransferPlanner {
    pub fn new(
        device: DeviceInfo,
        limits: QueueLimits,
        caps: TransferRuntimeCaps,
    ) -> Result<Self, BlkError> {
        let max_chunk_size = planned_transfer_size(device, limits, caps)?;

        Ok(Self {
            device,
            limits,
            max_chunk_size,
        })
    }

    pub const fn chunk_size(&self) -> usize {
        self.max_chunk_size
    }

    pub fn plan(&self, lba: u64, byte_len: usize) -> Result<TransferPlan, BlkError> {
        TransferPlan::new(self.device, self.limits, self.max_chunk_size, lba, byte_len)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TransferPlan {
    next_lba: u64,
    byte_offset: usize,
    remaining_bytes: usize,
    block_size: usize,
    max_chunk_size: usize,
    max_segment_size: usize,
}

impl TransferPlan {
    fn new(
        device: DeviceInfo,
        limits: QueueLimits,
        max_chunk_size: usize,
        lba: u64,
        byte_len: usize,
    ) -> Result<Self, BlkError> {
        let block_size = device.logical_block_size;
        if block_size == 0 || byte_len == 0 || !byte_len.is_multiple_of(block_size) {
            return Err(BlkError::InvalidRequest);
        }

        let block_count = byte_len / block_size;
        let block_count_u64 = u64::try_from(block_count).map_err(|_| BlkError::InvalidRequest)?;
        if lba >= device.num_blocks
            || lba
                .checked_add(block_count_u64)
                .is_none_or(|end| end > device.num_blocks)
        {
            return Err(BlkError::InvalidBlockIndex(lba));
        }

        Ok(Self {
            next_lba: lba,
            byte_offset: 0,
            remaining_bytes: byte_len,
            block_size,
            max_chunk_size,
            max_segment_size: limits.max_segment_size,
        })
    }
}

fn planned_transfer_size(
    device: DeviceInfo,
    limits: QueueLimits,
    caps: TransferRuntimeCaps,
) -> Result<usize, BlkError> {
    let block_size = device.logical_block_size;
    let max_segments = limits.max_segments.min(caps.max_segments);
    if block_size == 0
        || limits.max_blocks_per_request == 0
        || max_segments == 0
        || limits.max_segment_size == 0
        || limits.max_transfer_size == 0
        || limits.preferred_transfer_size == 0
        || caps.max_transfer_bytes == 0
    {
        return Err(BlkError::InvalidRequest);
    }

    let max_by_blocks = block_size.saturating_mul(limits.max_blocks_per_request as usize);
    let max_by_segments = limits.max_segment_size.saturating_mul(max_segments);
    let max_chunk_size = [
        max_by_blocks,
        max_by_segments,
        limits.max_transfer_size,
        limits.preferred_transfer_size,
        caps.max_transfer_bytes,
    ]
    .into_iter()
    .min()
    .ok_or(BlkError::InvalidRequest)?;
    let max_chunk_size = align_down(max_chunk_size, block_size);
    if max_chunk_size < block_size {
        return Err(BlkError::InvalidRequest);
    }
    Ok(max_chunk_size)
}

impl Iterator for TransferPlan {
    type Item = TransferChunk;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining_bytes == 0 {
            return None;
        }

        let byte_len = self.remaining_bytes.min(self.max_chunk_size);
        let block_count = byte_len / self.block_size;
        let block_count_u32 = block_count as u32;
        let chunk = TransferChunk {
            lba: self.next_lba,
            block_count: block_count_u32,
            byte_offset: self.byte_offset,
            byte_len,
            max_segment_size: self.max_segment_size,
        };

        self.next_lba += block_count as u64;
        self.byte_offset += byte_len;
        self.remaining_bytes -= byte_len;
        Some(chunk)
    }
}

fn align_down(value: usize, align: usize) -> usize {
    value / align * align
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
    pub const ALL_KNOWN: Self = Self(
        Self::FUA.bits()
            | Self::PREFLUSH.bits()
            | Self::SYNC.bits()
            | Self::META.bits()
            | Self::POLLED.bits()
            | Self::NOWAIT.bits(),
    );

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    pub const fn unsupported_by(self, supported: Self) -> Self {
        Self(self.0 & !supported.0)
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

/// A request queue for one block device hardware/software queue.
///
/// # Safety
///
/// Implementers may access `Request` segments after `submit_request` returns
/// and until the matching `poll_request` returns `RequestStatus::Complete` or
/// an error. They must not access any segment before `submit_request` is called
/// or after completion/error has been reported, and request IDs must not alias
/// two concurrently pending requests in a way that extends this lifetime.
pub unsafe trait IQueue: Send + 'static {
    fn id(&self) -> usize;

    fn info(&self) -> QueueInfo;

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError>;

    fn poll_request(&mut self, request: RequestId) -> Result<RequestStatus, BlkError>;
}

pub fn validate_request(info: QueueInfo, request: &Request<'_>) -> Result<(), BlkError> {
    validate_request_flags(info, request)?;
    validate_request_shape(info.device, info.limits, request)
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
            if request.data_len() > limits.max_transfer_size {
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

fn validate_request_flags(info: QueueInfo, request: &Request<'_>) -> Result<(), BlkError> {
    let unknown = request.flags.unsupported_by(RequestFlags::ALL_KNOWN);
    if !unknown.is_empty() {
        return Err(BlkError::InvalidRequest);
    }

    let unsupported = request.flags.unsupported_by(info.limits.supported_flags);
    if !unsupported.is_empty() {
        return Err(BlkError::NotSupported);
    }

    if request.flags.intersects(RequestFlags::PREFLUSH) && !info.limits.supports_flush {
        return Err(BlkError::NotSupported);
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

    // SAFETY: This test queue never stores request segments beyond
    // `submit_request` and reports completion immediately.
    unsafe impl IQueue for Queue {
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

    fn queue_info_with(limits: QueueLimits) -> QueueInfo {
        QueueInfo {
            id: 0,
            depth: 8,
            mode: QueueMode::Polled,
            device: DeviceInfo::new(64, 512),
            limits,
        }
    }

    fn test_runtime_caps() -> TransferRuntimeCaps {
        TransferRuntimeCaps {
            max_transfer_bytes: 16 * 1024,
            max_segments: 16,
        }
    }

    fn chunk_summary(chunks: &[TransferChunk]) -> alloc::vec::Vec<(u64, u32, usize, usize, usize)> {
        chunks
            .iter()
            .map(|chunk| {
                let segments = chunk.segments();
                (
                    chunk.lba,
                    chunk.block_count,
                    chunk.byte_offset,
                    chunk.byte_len,
                    segments.len(),
                )
            })
            .collect()
    }

    #[test]
    fn simple_limits_prefer_single_block_transfers() {
        let info = queue_info_with(QueueLimits::simple(512, u64::MAX));
        let planner = TransferPlanner::new(info.device, info.limits, test_runtime_caps()).unwrap();
        let plan = planner.plan(0, 2048).unwrap();
        let chunks: alloc::vec::Vec<_> = plan.collect();

        assert_eq!(planner.chunk_size(), 512);
        assert_eq!(
            chunk_summary(&chunks),
            [
                (0, 1, 0, 512, 1),
                (1, 1, 512, 512, 1),
                (2, 1, 1024, 512, 1),
                (3, 1, 1536, 512, 1),
            ]
        );
    }

    #[test]
    fn transfer_plan_chunks_by_preferred_size() {
        let info = queue_info_with(QueueLimits {
            dma_mask: u64::MAX,
            dma_alignment: 512,
            max_blocks_per_request: 16,
            max_segments: 4,
            max_segment_size: 4096,
            max_transfer_size: 8192,
            preferred_transfer_size: 2048,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        });
        let planner = TransferPlanner::new(info.device, info.limits, test_runtime_caps()).unwrap();
        let plan = planner.plan(4, 5120).unwrap();
        let chunks: alloc::vec::Vec<_> = plan.collect();

        assert_eq!(planner.chunk_size(), 2048);
        assert_eq!(
            chunk_summary(&chunks),
            [
                (4, 4, 0, 2048, 1),
                (8, 4, 2048, 2048, 1),
                (12, 2, 4096, 1024, 1),
            ]
        );
    }

    #[test]
    fn transfer_chunk_segments_split_by_hard_segment_size() {
        let info = queue_info_with(QueueLimits {
            dma_mask: u64::MAX,
            dma_alignment: 512,
            max_blocks_per_request: 16,
            max_segments: 4,
            max_segment_size: 1024,
            max_transfer_size: 4096,
            preferred_transfer_size: 4096,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        });
        let planner = TransferPlanner::new(info.device, info.limits, test_runtime_caps()).unwrap();
        let mut plan = planner.plan(0, 4096).unwrap();
        let chunk = plan.next().unwrap();
        let segment_iter = chunk.segments();
        assert_eq!(segment_iter.len(), 4);
        let segments: alloc::vec::Vec<_> = segment_iter.collect();

        assert_eq!(
            segments,
            [
                TransferSegment {
                    byte_offset: 0,
                    byte_len: 1024,
                },
                TransferSegment {
                    byte_offset: 1024,
                    byte_len: 1024,
                },
                TransferSegment {
                    byte_offset: 2048,
                    byte_len: 1024,
                },
                TransferSegment {
                    byte_offset: 3072,
                    byte_len: 1024,
                },
            ]
        );
        assert!(plan.next().is_none());
    }

    #[test]
    fn transfer_plan_clamps_to_hard_transfer_size() {
        let info = queue_info_with(QueueLimits {
            dma_mask: u64::MAX,
            dma_alignment: 512,
            max_blocks_per_request: 16,
            max_segments: 8,
            max_segment_size: 4096,
            max_transfer_size: 2048,
            preferred_transfer_size: 8192,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        });
        let planner = TransferPlanner::new(info.device, info.limits, test_runtime_caps()).unwrap();
        let plan = planner.plan(0, 5120).unwrap();
        let chunks: alloc::vec::Vec<_> = plan.collect();

        assert_eq!(planner.chunk_size(), 2048);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.byte_len)
                .collect::<alloc::vec::Vec<_>>(),
            [2048, 2048, 1024]
        );
    }

    #[test]
    fn transfer_plan_clamps_to_runtime_limits() {
        let info = queue_info_with(QueueLimits {
            dma_mask: u64::MAX,
            dma_alignment: 512,
            max_blocks_per_request: 16,
            max_segments: 8,
            max_segment_size: 2048,
            max_transfer_size: 8192,
            preferred_transfer_size: 8192,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        });
        let planner = TransferPlanner::new(
            info.device,
            info.limits,
            TransferRuntimeCaps {
                max_transfer_bytes: 4096,
                max_segments: 1,
            },
        )
        .unwrap();
        let plan = planner.plan(0, 4096).unwrap();
        let chunks: alloc::vec::Vec<_> = plan.collect();

        assert_eq!(planner.chunk_size(), 2048);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.byte_len)
                .collect::<alloc::vec::Vec<_>>(),
            [2048, 2048]
        );
    }

    #[test]
    fn transfer_planner_rejects_too_small_runtime_cap() {
        let info = queue_info_with(QueueLimits {
            dma_mask: u64::MAX,
            dma_alignment: 512,
            max_blocks_per_request: 16,
            max_segments: 8,
            max_segment_size: 2048,
            max_transfer_size: 8192,
            preferred_transfer_size: 8192,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        });

        assert_eq!(
            TransferPlanner::new(
                info.device,
                info.limits,
                TransferRuntimeCaps {
                    max_transfer_bytes: 511,
                    max_segments: 1,
                },
            )
            .unwrap_err(),
            BlkError::InvalidRequest
        );
    }

    #[test]
    fn transfer_planner_does_not_depend_on_queue_identity() {
        let mut info = queue_info_with(QueueLimits {
            dma_mask: u64::MAX,
            dma_alignment: 512,
            max_blocks_per_request: 16,
            max_segments: 8,
            max_segment_size: 2048,
            max_transfer_size: 8192,
            preferred_transfer_size: 4096,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        });
        let first = TransferPlanner::new(info.device, info.limits, test_runtime_caps()).unwrap();
        info.id = 7;
        info.depth = 64;
        info.mode = QueueMode::Interrupt;
        let second = TransferPlanner::new(info.device, info.limits, test_runtime_caps()).unwrap();

        assert_eq!(first.chunk_size(), second.chunk_size());
    }

    #[test]
    fn transfer_planner_checks_range_when_creating_plan() {
        let info = queue_info_with(QueueLimits::simple(512, u64::MAX));
        let planner = TransferPlanner::new(info.device, info.limits, test_runtime_caps()).unwrap();

        assert_eq!(
            planner.plan(63, 1024).unwrap_err(),
            BlkError::InvalidBlockIndex(63)
        );
    }

    #[test]
    fn request_validation_rejects_unsupported_flags() {
        let info = queue_info_with(QueueLimits::simple(512, u64::MAX));
        let mut bytes = [0_u8; 512];
        let segment = unsafe { Segment::from_raw_parts(bytes.as_mut_ptr(), 0x1000, bytes.len()) };
        let mut segments = [segment];
        let request = Request {
            op: RequestOp::Write,
            lba: 0,
            block_count: 1,
            segments: &mut segments,
            flags: RequestFlags::FUA,
        };

        assert_eq!(
            validate_request(info, &request),
            Err(BlkError::NotSupported)
        );
    }

    #[test]
    fn request_validation_rejects_unknown_flags() {
        let info = queue_info_with(QueueLimits::simple(512, u64::MAX));
        let mut bytes = [0_u8; 512];
        let segment = unsafe { Segment::from_raw_parts(bytes.as_mut_ptr(), 0x1000, bytes.len()) };
        let mut segments = [segment];
        let request = Request {
            op: RequestOp::Read,
            lba: 0,
            block_count: 1,
            segments: &mut segments,
            flags: RequestFlags(1 << 24),
        };

        assert_eq!(
            validate_request(info, &request),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn request_validation_accepts_supported_flags() {
        let mut limits = QueueLimits::simple(512, u64::MAX);
        limits.supported_flags = RequestFlags::FUA;
        let info = queue_info_with(limits);
        let mut bytes = [0_u8; 512];
        let segment = unsafe { Segment::from_raw_parts(bytes.as_mut_ptr(), 0x1000, bytes.len()) };
        let mut segments = [segment];
        let request = Request {
            op: RequestOp::Write,
            lba: 0,
            block_count: 1,
            segments: &mut segments,
            flags: RequestFlags::FUA,
        };

        assert_eq!(validate_request(info, &request), Ok(()));
    }

    #[test]
    fn preflush_flag_requires_flush_support() {
        let mut limits = QueueLimits::simple(512, u64::MAX);
        limits.supported_flags = RequestFlags::PREFLUSH;
        let info = queue_info_with(limits);
        let mut bytes = [0_u8; 512];
        let segment = unsafe { Segment::from_raw_parts(bytes.as_mut_ptr(), 0x1000, bytes.len()) };
        let mut segments = [segment];
        let request = Request {
            op: RequestOp::Write,
            lba: 0,
            block_count: 1,
            segments: &mut segments,
            flags: RequestFlags::PREFLUSH,
        };

        assert_eq!(
            validate_request(info, &request),
            Err(BlkError::NotSupported)
        );
    }

    #[test]
    fn request_validation_rejects_transfer_larger_than_hard_limit() {
        let info = queue_info_with(QueueLimits {
            dma_mask: u64::MAX,
            dma_alignment: 512,
            max_blocks_per_request: 8,
            max_segments: 1,
            max_segment_size: 4096,
            max_transfer_size: 1024,
            preferred_transfer_size: 1024,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        });
        let mut bytes = [0_u8; 1536];
        let segment = unsafe { Segment::from_raw_parts(bytes.as_mut_ptr(), 0x1000, bytes.len()) };
        let mut segments = [segment];
        let request = Request {
            op: RequestOp::Write,
            lba: 0,
            block_count: 3,
            segments: &mut segments,
            flags: RequestFlags::NONE,
        };

        assert_eq!(
            validate_request(info, &request),
            Err(BlkError::InvalidRequest)
        );
    }
}
