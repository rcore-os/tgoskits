use core::fmt;

use dma_api::{CpuDmaBuffer, DmaDirection};

use crate::{BlkError, DeviceInfo, QueueInfo, QueueLimits};

/// Identity carried by one queue request.
///
/// An interrupt runtime allocates generation-bearing values before submission
/// so completion routing does not depend on a driver-local allocator or a
/// later lookup by buffer address. Inline queues always use [`Self::INLINE`]
/// because ownership never leaves the submission call.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(usize);

impl RequestId {
    /// Sentinel used by a [`crate::QueueKind::Inline`] request.
    ///
    /// Inline queues return ownership in the submission call and therefore do
    /// not need a generation, waiter, tag, or completion-table identity. The
    /// all-ones value is reserved so it can never alias an interrupt-backed
    /// request identity.
    pub const INLINE: Self = Self(usize::MAX);

    /// Tries to create an interrupt-backed generation or tag identity.
    ///
    /// Returns `None` for the representation reserved by [`Self::INLINE`].
    pub const fn try_new(id: usize) -> Option<Self> {
        if id == usize::MAX {
            None
        } else {
            Some(Self(id))
        }
    }

    /// Creates an interrupt-backed generation or tag identity.
    ///
    /// # Panics
    ///
    /// Panics when `id` is the reserved [`Self::INLINE`] representation.
    pub const fn new(id: usize) -> Self {
        match Self::try_new(id) {
            Some(id) => id,
            None => panic!("usize::MAX is reserved for RequestId::INLINE"),
        }
    }

    /// Whether this value names a call-stack-only inline request.
    pub const fn is_inline(self) -> bool {
        self.0 == Self::INLINE.0
    }
}

impl From<RequestId> for usize {
    fn from(value: RequestId) -> Self {
        value.0
    }
}

/// Operation performed by a block request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOp {
    Read,
    Write,
    Flush,
    Discard,
    WriteZeroes,
}

/// Request behavior flags supported independently of completion dispatch.
///
/// Completion dispatch is deliberately not a request property. Queues declare
/// their inline or interrupt completion contract through [`crate::QueueKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestFlags(u32);

impl RequestFlags {
    pub const NONE: Self = Self(0);
    pub const FUA: Self = Self(1 << 0);
    pub const PREFLUSH: Self = Self(1 << 1);
    pub const SYNC: Self = Self(1 << 2);
    pub const META: Self = Self(1 << 3);
    pub const NOWAIT: Self = Self(1 << 4);
    pub const ALL_KNOWN: Self = Self(
        Self::FUA.bits()
            | Self::PREFLUSH.bits()
            | Self::SYNC.bits()
            | Self::META.bits()
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

/// Block I/O request whose optional data buffer is owned by the runtime.
///
/// At the public queue boundary the DMA buffer is always CPU-owned. An
/// interrupt-backed queue prepares it immediately before arming hardware and
/// restores CPU ownership after the hardware is quiesced. Consequently both a
/// submit failure and a terminal completion can return this exact request.
pub struct OwnedRequest {
    pub op: RequestOp,
    pub lba: u64,
    pub block_count: u32,
    pub data: Option<CpuDmaBuffer>,
    pub flags: RequestFlags,
}

impl OwnedRequest {
    pub fn data_len(&self) -> usize {
        self.data.as_ref().map_or(0, |data| data.len().get())
    }

    pub fn is_data_op(&self) -> bool {
        matches!(self.op, RequestOp::Read | RequestOp::Write)
    }
}

impl fmt::Debug for OwnedRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedRequest")
            .field("op", &self.op)
            .field("lba", &self.lba)
            .field("block_count", &self.block_count)
            .field("data_len", &self.data_len())
            .field("flags", &self.flags)
            .finish()
    }
}

/// Submit-side failure that returns request ownership to the runtime.
#[derive(Debug, thiserror::Error)]
#[error("block request submission failed: {error}")]
pub struct SubmitError {
    id: RequestId,
    error: BlkError,
    request: OwnedRequest,
}

impl SubmitError {
    pub fn new(id: RequestId, error: BlkError, request: OwnedRequest) -> Self {
        Self { id, error, request }
    }

    pub const fn id(&self) -> RequestId {
        self.id
    }

    pub const fn error(&self) -> BlkError {
        self.error
    }

    pub fn request(&self) -> &OwnedRequest {
        &self.request
    }

    pub fn into_parts(self) -> (RequestId, BlkError, OwnedRequest) {
        (self.id, self.error, self.request)
    }
}

/// One terminal result with complete request ownership returned to the runtime.
#[derive(Debug)]
pub struct CompletedRequest {
    pub id: RequestId,
    pub result: Result<(), BlkError>,
    pub request: OwnedRequest,
}

impl CompletedRequest {
    pub const fn new(id: RequestId, result: Result<(), BlkError>, request: OwnedRequest) -> Self {
        Self {
            id,
            result,
            request,
        }
    }
}

/// Result of submitting an owned request.
#[derive(Debug)]
pub enum SubmitOutcome {
    /// The request completed inline and ownership is already back at runtime.
    Completed(CompletedRequest),
    /// The queue owns the request until it reports one terminal completion.
    Queued,
}

pub fn validate_owned_request(info: QueueInfo, request: &OwnedRequest) -> Result<(), BlkError> {
    validate_request_flags(info, request.flags)?;
    validate_owned_request_shape(info.device, info.limits, request)
}

pub fn validate_owned_request_shape(
    info: DeviceInfo,
    limits: QueueLimits,
    request: &OwnedRequest,
) -> Result<(), BlkError> {
    if request.block_count == 0 && !matches!(request.op, RequestOp::Flush) {
        return Err(BlkError::InvalidRequest);
    }
    if info.read_only
        && matches!(
            request.op,
            RequestOp::Write | RequestOp::Discard | RequestOp::WriteZeroes
        )
    {
        return Err(BlkError::NotSupported);
    }
    if matches!(request.op, RequestOp::Flush) && request.lba != 0 {
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
        RequestOp::Read | RequestOp::Write => validate_data_request(info, limits, request)?,
        RequestOp::Flush => {
            if request.data.is_some() || request.block_count != 0 {
                return Err(BlkError::InvalidRequest);
            }
            if !limits.supports_flush {
                return Err(BlkError::NotSupported);
            }
        }
        RequestOp::Discard => {
            if request.data.is_some() {
                return Err(BlkError::InvalidRequest);
            }
            if !limits.supports_discard {
                return Err(BlkError::NotSupported);
            }
        }
        RequestOp::WriteZeroes => {
            if request.data.is_some() {
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

fn validate_data_request(
    info: DeviceInfo,
    limits: QueueLimits,
    request: &OwnedRequest,
) -> Result<(), BlkError> {
    let expected = usize::try_from(request.block_count)
        .ok()
        .and_then(|blocks| blocks.checked_mul(info.logical_block_size))
        .ok_or(BlkError::InvalidRequest)?;
    let Some(data) = &request.data else {
        return Err(BlkError::InvalidRequest);
    };
    let segment_capacity = limits.max_segment_size.saturating_mul(limits.max_segments);
    if request.data_len() != expected
        || limits.max_segments == 0
        || data.len().get() > segment_capacity
    {
        return Err(BlkError::InvalidRequest);
    }
    let direction_matches = match request.op {
        RequestOp::Read => matches!(
            data.direction(),
            DmaDirection::FromDevice | DmaDirection::Bidirectional
        ),
        RequestOp::Write => matches!(
            data.direction(),
            DmaDirection::ToDevice | DmaDirection::Bidirectional
        ),
        RequestOp::Flush | RequestOp::Discard | RequestOp::WriteZeroes => false,
    };
    if !direction_matches || data.domain_id() != limits.dma_domain {
        return Err(BlkError::InvalidRequest);
    }

    let dma_alignment = u64::try_from(limits.dma_alignment)
        .ok()
        .filter(|alignment| *alignment != 0)
        .ok_or(BlkError::InvalidRequest)?;
    let dma_start = data.dma_addr().as_u64();
    let dma_len = u64::try_from(data.len().get()).map_err(|_| BlkError::InvalidRequest)?;
    let dma_end = dma_start
        .checked_add(dma_len - 1)
        .ok_or(BlkError::InvalidRequest)?;
    if !dma_start.is_multiple_of(dma_alignment) || dma_end > limits.dma_mask {
        return Err(BlkError::InvalidRequest);
    }
    Ok(())
}

fn validate_request_flags(info: QueueInfo, flags: RequestFlags) -> Result<(), BlkError> {
    let unknown = flags.unsupported_by(RequestFlags::ALL_KNOWN);
    if !unknown.is_empty() {
        return Err(BlkError::InvalidRequest);
    }

    let unsupported = flags.unsupported_by(info.limits.supported_flags);
    if !unsupported.is_empty() {
        return Err(BlkError::NotSupported);
    }

    if flags.intersects(RequestFlags::PREFLUSH) && !info.limits.supports_flush {
        return Err(BlkError::NotSupported);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};
    use std::alloc::{alloc_zeroed, dealloc};

    use super::*;
    use crate::{
        DispatchMode, QueueKind,
        dma_api::{
            DeviceDma, DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp,
        },
    };

    struct TestDma;

    impl DmaOp for TestDma {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
            Some(unsafe { DmaAllocHandle::new(ptr, (ptr.as_ptr() as u64).into(), layout) })
        }

        unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
            unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
        }

        unsafe fn alloc_coherent(
            &self,
            constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            unsafe { self.alloc_contiguous(constraints, layout) }
        }

        unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
            unsafe { self.dealloc_contiguous(handle) };
        }

        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            addr: NonNull<u8>,
            size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            let layout = Layout::from_size_align(size.get(), 1)?;
            Ok(unsafe { DmaMapHandle::new(addr, (addr.as_ptr() as u64).into(), layout, None) })
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
    }

    static TEST_DMA: TestDma = TestDma;

    fn dma_buffer(len: usize) -> CpuDmaBuffer {
        dma_buffer_for_direction(len, DmaDirection::FromDevice)
    }

    fn dma_buffer_for_direction(len: usize, direction: DmaDirection) -> CpuDmaBuffer {
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        CpuDmaBuffer::new_zero(
            &dma,
            NonZeroUsize::new(len).expect("test DMA buffer must be non-empty"),
            1,
            direction,
        )
        .expect("test DMA allocation must succeed")
    }

    fn queue_info_with(limits: QueueLimits) -> QueueInfo {
        let mut sources = crate::IdList::none();
        sources.insert(0);
        QueueInfo {
            id: 0,
            device: DeviceInfo::new(64, 512),
            limits,
            kind: QueueKind::Interrupt { sources },
            dispatch_mode: DispatchMode::Direct,
        }
    }

    fn flush_request(flags: RequestFlags) -> OwnedRequest {
        OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags,
        }
    }

    #[test]
    fn inline_request_identity_is_a_reserved_non_generation_sentinel() {
        assert_eq!(usize::from(RequestId::INLINE), usize::MAX);
        assert_eq!(RequestId::try_new(usize::MAX), None);
        assert_eq!(RequestId::try_new(7), Some(RequestId::new(7)));
        assert!(RequestId::INLINE.is_inline());
        assert!(!RequestId::new(7).is_inline());
    }

    #[test]
    fn request_validation_rejects_unsupported_flags() {
        let info = queue_info_with(QueueLimits::simple(512, u64::MAX));

        assert_eq!(
            validate_owned_request(info, &flush_request(RequestFlags::FUA)),
            Err(BlkError::NotSupported)
        );
    }

    #[test]
    fn request_validation_rejects_unknown_flags() {
        let mut limits = QueueLimits::simple(512, u64::MAX);
        limits.supports_flush = true;
        let info = queue_info_with(limits);

        assert_eq!(
            validate_owned_request(info, &flush_request(RequestFlags(1 << 24))),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn flush_validation_accepts_cpu_owned_request_without_data() {
        let mut limits = QueueLimits::simple(512, u64::MAX);
        limits.supports_flush = true;
        let info = queue_info_with(limits);

        assert_eq!(
            validate_owned_request(info, &flush_request(RequestFlags::NONE)),
            Ok(())
        );
    }

    #[test]
    fn request_length_does_not_truncate_a_large_logical_block_size() {
        let logical_block_size = u32::MAX as usize + 513;
        let mut limits = QueueLimits::simple(logical_block_size, u64::MAX);
        limits.max_segment_size = logical_block_size;
        let request = OwnedRequest {
            op: RequestOp::Read,
            lba: 0,
            block_count: 1,
            data: Some(dma_buffer(512)),
            flags: RequestFlags::NONE,
        };

        assert_eq!(
            validate_owned_request_shape(DeviceInfo::new(1, logical_block_size), limits, &request,),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn submit_error_returns_the_same_request_identity_fields() {
        let request_id = RequestId::new(33);
        let error = SubmitError::new(
            request_id,
            BlkError::Retry,
            flush_request(RequestFlags::SYNC),
        );

        assert_eq!(error.id(), request_id);
        assert_eq!(error.error(), BlkError::Retry);
        assert_eq!(error.request().flags, RequestFlags::SYNC);
        let (returned_id, returned_error, request) = error.into_parts();
        assert_eq!(returned_id, request_id);
        assert_eq!(returned_error, BlkError::Retry);
        assert_eq!(request.op, RequestOp::Flush);
        assert_eq!(request.lba, 0);
    }

    #[test]
    fn submit_error_returns_the_exact_dma_backing_without_reallocation() {
        let data = dma_buffer(512);
        let original_cpu_pointer = data.cpu_ptr();
        let original_dma_address = data.dma_addr();
        let request = OwnedRequest {
            op: RequestOp::Read,
            lba: 5,
            block_count: 1,
            data: Some(data),
            flags: RequestFlags::SYNC,
        };

        let (_, error, returned) =
            SubmitError::new(RequestId::new(37), BlkError::Retry, request).into_parts();
        let returned_data = returned
            .data
            .expect("rejected data request must return its DMA backing");

        assert_eq!(error, BlkError::Retry);
        assert_eq!(returned_data.cpu_ptr(), original_cpu_pointer);
        assert_eq!(returned_data.dma_addr(), original_dma_address);
    }

    #[test]
    fn request_validation_rejects_dma_direction_mismatch() {
        let mut limits = QueueLimits::simple(512, u64::MAX);
        limits.dma_alignment = 1;
        let request = OwnedRequest {
            op: RequestOp::Read,
            lba: 0,
            block_count: 1,
            data: Some(dma_buffer_for_direction(512, DmaDirection::ToDevice)),
            flags: RequestFlags::NONE,
        };

        assert_eq!(
            validate_owned_request(queue_info_with(limits), &request),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn request_validation_rejects_dma_domain_mismatch() {
        let mut limits = QueueLimits::simple(512, u64::MAX);
        limits.dma_alignment = 1;
        limits.dma_domain = crate::dma_api::DmaDomainId::from_raw(2);
        let request = OwnedRequest {
            op: RequestOp::Read,
            lba: 0,
            block_count: 1,
            data: Some(dma_buffer(512)),
            flags: RequestFlags::NONE,
        };

        assert_eq!(
            validate_owned_request(queue_info_with(limits), &request),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn request_validation_rejects_dma_backing_outside_the_queue_mask() {
        let data = dma_buffer(512);
        let dma_start = data.dma_addr().as_u64();
        let mut limits = QueueLimits::simple(512, dma_start.saturating_sub(1));
        limits.dma_alignment = 1;
        let request = OwnedRequest {
            op: RequestOp::Read,
            lba: 0,
            block_count: 1,
            data: Some(data),
            flags: RequestFlags::NONE,
        };

        assert_eq!(
            validate_owned_request(queue_info_with(limits), &request),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn request_validation_rejects_write_operations_on_read_only_devices() {
        let mut limits = QueueLimits::simple(512, u64::MAX);
        limits.dma_alignment = 1;
        let mut info = queue_info_with(limits);
        info.device.read_only = true;
        let request = OwnedRequest {
            op: RequestOp::Write,
            lba: 0,
            block_count: 1,
            data: Some(dma_buffer_for_direction(512, DmaDirection::ToDevice)),
            flags: RequestFlags::NONE,
        };

        assert_eq!(
            validate_owned_request(info, &request),
            Err(BlkError::NotSupported)
        );
    }

    #[test]
    fn flush_request_cannot_smuggle_a_logical_block_address() {
        let mut limits = QueueLimits::simple(512, u64::MAX);
        limits.supports_flush = true;
        let mut request = flush_request(RequestFlags::NONE);
        request.lba = 5;

        assert_eq!(
            validate_owned_request(queue_info_with(limits), &request),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn request_validation_accounts_for_the_full_segment_budget() {
        let mut limits = QueueLimits::simple(512, u64::MAX);
        limits.dma_alignment = 1;
        limits.max_blocks_per_request = 2;
        limits.max_segments = 2;
        limits.max_segment_size = 512;
        let request = OwnedRequest {
            op: RequestOp::Read,
            lba: 0,
            block_count: 2,
            data: Some(dma_buffer(1024)),
            flags: RequestFlags::NONE,
        };

        assert_eq!(
            validate_owned_request(queue_info_with(limits), &request),
            Ok(())
        );
    }
}
