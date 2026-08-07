use alloc::{boxed::Box, collections::VecDeque};

use dma_api::{CompletedDma, PreparedDma};

use crate::{BlkError, DeviceInfo, QueueInfo, QueueLimits};

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
pub enum RequestOp {
    Read,
    Write,
    Flush,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestFlags(u32);

impl RequestFlags {
    pub const NONE: Self = Self(0);
    pub const FUA: Self = Self(1 << 0);
    pub const PREFLUSH: Self = Self(1 << 1);
    /// Runtime channel admission hint; never forwarded as a hardware bit.
    pub const NOWAIT: Self = Self(1 << 2);
    pub const ALL_KNOWN: Self =
        Self(Self::FUA.bits() | Self::PREFLUSH.bits() | Self::NOWAIT.bits());

    pub const fn bits(self) -> u32 {
        self.0
    }

    #[cfg(all(axtest, feature = "axtest"))]
    pub(crate) const fn from_bits_for_test(bits: u32) -> Self {
        Self(bits)
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

    pub const fn without(self, removed: Self) -> Self {
        Self(self.0 & !removed.0)
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

/// Block request that transfers its DMA backing to one hardware queue.
pub struct OwnedRequest {
    pub op: RequestOp,
    pub lba: u64,
    pub block_count: u32,
    pub data: Option<PreparedDma>,
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

/// Ordered requests whose DMA ownership may be transferred as one hardware
/// submission batch.
///
/// A driver removes requests from the front only after it has established
/// queue ownership for them. Requests left in the batch remain owned by the
/// runtime and may be retried without reconstruction.
pub struct OwnedRequestBatch {
    requests: VecDeque<OwnedRequest>,
}

impl OwnedRequestBatch {
    /// Creates an empty request batch with space for `capacity` requests.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            requests: VecDeque::with_capacity(capacity),
        }
    }

    /// Returns the number of requests still owned by this batch.
    pub fn len(&self) -> usize {
        self.requests.len()
    }

    /// Returns whether the batch owns no requests.
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    /// Borrows the next request without transferring ownership.
    pub fn front(&self) -> Option<&OwnedRequest> {
        self.requests.front()
    }

    /// Iterates over requests without changing ownership.
    pub fn iter(&self) -> impl Iterator<Item = &OwnedRequest> {
        self.requests.iter()
    }

    /// Iterates over requests while they remain owned by the batch.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut OwnedRequest> {
        self.requests.iter_mut()
    }

    /// Transfers the next request out of the batch.
    ///
    /// Drivers must call this only after all validation and queue-resource
    /// reservation needed to accept that request have succeeded.
    pub fn pop_front(&mut self) -> Option<OwnedRequest> {
        self.requests.pop_front()
    }

    /// Transfers the last runtime-owned request out of the batch.
    ///
    /// The block runtime uses this to restore an unaccepted suffix ahead of
    /// older retry work without allocating another batch container. Drivers
    /// should normally consume only from the front.
    pub fn pop_back(&mut self) -> Option<OwnedRequest> {
        self.requests.pop_back()
    }

    /// Restores a request before every request still owned by the batch.
    ///
    /// This is intended for driver rollback before hardware ownership has been
    /// established.
    pub fn push_front(&mut self, request: OwnedRequest) {
        self.requests.push_front(request);
    }

    /// Appends a runtime-owned request to the batch.
    pub fn push_back(&mut self, request: OwnedRequest) {
        self.requests.push_back(request);
    }
}

impl FromIterator<OwnedRequest> for OwnedRequestBatch {
    fn from_iter<T: IntoIterator<Item = OwnedRequest>>(iter: T) -> Self {
        Self {
            requests: iter.into_iter().collect(),
        }
    }
}

impl IntoIterator for OwnedRequestBatch {
    type Item = OwnedRequest;
    type IntoIter = alloc::collections::vec_deque::IntoIter<OwnedRequest>;

    fn into_iter(self) -> Self::IntoIter {
        self.requests.into_iter()
    }
}

/// Submit-side failure that returns request and DMA ownership.
pub struct SubmitError {
    pub error: BlkError,
    request: Box<OwnedRequest>,
}

impl SubmitError {
    pub fn new(error: BlkError, request: OwnedRequest) -> Self {
        Self {
            error,
            request: Box::new(request),
        }
    }

    pub fn into_request(self) -> OwnedRequest {
        *self.request
    }

    pub fn request(&self) -> &OwnedRequest {
        &self.request
    }
}

/// Batch admission failure that returns every unsubmitted request.
pub struct BatchSubmitError {
    pub error: BlkError,
    batch: OwnedRequestBatch,
}

impl BatchSubmitError {
    /// Creates an error while preserving request and DMA ownership.
    pub fn new(error: BlkError, batch: OwnedRequestBatch) -> Self {
        Self { error, batch }
    }

    /// Returns every request to the caller in its original order.
    pub fn into_batch(self) -> OwnedRequestBatch {
        self.batch
    }

    /// Borrows the requests returned to the caller.
    pub fn batch(&self) -> &OwnedRequestBatch {
        &self.batch
    }
}

/// One terminal result after hardware has relinquished DMA ownership.
pub struct CompletedRequest {
    pub id: RequestId,
    pub result: Result<(), BlkError>,
    pub data: Option<CompletedDma>,
}

impl CompletedRequest {
    pub const fn new(
        id: RequestId,
        result: Result<(), BlkError>,
        data: Option<CompletedDma>,
    ) -> Self {
        Self { id, result, data }
    }
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
    match request.op {
        RequestOp::Read | RequestOp::Write => {
            if request.op == RequestOp::Write && info.read_only {
                return Err(BlkError::NotSupported);
            }
            if request.block_count == 0
                || request.block_count > limits.max_blocks_per_request
                || request.lba >= info.num_blocks
                || request
                    .lba
                    .checked_add(u64::from(request.block_count))
                    .is_none_or(|end| end > info.num_blocks)
            {
                return Err(BlkError::InvalidBlockIndex(request.lba));
            }
            let expected = usize::try_from(request.block_count)
                .ok()
                .and_then(|blocks| blocks.checked_mul(info.logical_block_size))
                .ok_or(BlkError::InvalidRequest)?;
            if request.data_len() != expected {
                return Err(BlkError::InvalidRequest);
            }
            let data = request.data.as_ref().ok_or(BlkError::InvalidRequest)?;
            let segments = data.segments();
            if segments.is_empty()
                || segments.len() > limits.max_segments
                || data.domain_id() != limits.dma_domain
                || segments.iter().any(|segment| {
                    segment.len.get() > limits.max_segment_size
                        || !dma_segment_matches_limits(
                            segment.addr.as_u64(),
                            segment.len.get(),
                            limits,
                        )
                })
            {
                return Err(BlkError::InvalidRequest);
            }
        }
        RequestOp::Flush => {
            if request.block_count != 0 || request.data.is_some() {
                return Err(BlkError::InvalidRequest);
            }
            if !limits.supports_flush {
                return Err(BlkError::NotSupported);
            }
        }
    }
    Ok(())
}

fn dma_segment_matches_limits(bus: u64, len: usize, limits: QueueLimits) -> bool {
    let Some(last) = u64::try_from(len)
        .ok()
        .and_then(|len| len.checked_sub(1))
        .and_then(|last| bus.checked_add(last))
    else {
        return false;
    };
    if bus & !limits.dma_mask != 0 || last & !limits.dma_mask != 0 {
        return false;
    }

    if limits.dma_alignment == 0
        || limits.dma_length_alignment == 0
        || !bus.is_multiple_of(limits.dma_alignment as u64)
        || !len.is_multiple_of(limits.dma_length_alignment)
    {
        return false;
    }

    match limits.segment_boundary {
        None => true,
        Some(boundary) if boundary.is_power_of_two() => {
            let boundary_mask = !(boundary as u64 - 1);
            bus & boundary_mask == last & boundary_mask
        }
        Some(_) => false,
    }
}

fn validate_request_flags(info: QueueInfo, flags: RequestFlags) -> Result<(), BlkError> {
    if !flags.unsupported_by(RequestFlags::ALL_KNOWN).is_empty() {
        return Err(BlkError::InvalidRequest);
    }
    // NOWAIT is consumed by the bounded runtime channel.
    if !flags
        .unsupported_by(info.limits.supported_flags | RequestFlags::NOWAIT)
        .is_empty()
    {
        return Err(BlkError::NotSupported);
    }
    if flags.intersects(RequestFlags::PREFLUSH) && !info.limits.supports_flush {
        return Err(BlkError::NotSupported);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloc::alloc::{alloc_zeroed, dealloc};
    use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

    use dma_api::{
        DmaAddr, DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp,
        PreparedDma,
    };

    use super::*;

    struct TestDma {
        addr: u64,
    }

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
            Some(unsafe { DmaAllocHandle::new(ptr, DmaAddr::from(self.addr), layout) })
        }

        unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
            unsafe {
                dealloc(handle.as_ptr().as_ptr(), handle.layout());
            }
        }

        unsafe fn alloc_coherent(
            &self,
            _constraints: DmaConstraints,
            _layout: Layout,
        ) -> Option<DmaAllocHandle> {
            None
        }

        unsafe fn dealloc_coherent(&self, _handle: DmaAllocHandle) -> Result<(), DmaError> {
            Ok(())
        }

        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            _addr: NonNull<u8>,
            _size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, dma_api::DmaError> {
            Err(dma_api::DmaError::NoMemory)
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
    }

    fn prepared(addr: u64, len: usize) -> PreparedDma {
        let dma = Box::leak(Box::new(TestDma { addr }));
        let device = dma_api::DeviceDma::new_legacy(u64::MAX, dma);
        dma_api::CpuDmaBuffer::new_zero(
            &device,
            NonZeroUsize::new(len).unwrap(),
            1,
            DmaDirection::ToDevice,
        )
        .unwrap()
        .prepare_for_device()
    }

    fn request_with(addr: u64, len: usize) -> OwnedRequest {
        OwnedRequest {
            op: RequestOp::Write,
            lba: 0,
            block_count: (len / 512) as u32,
            data: Some(prepared(addr, len)),
            flags: RequestFlags::NONE,
        }
    }

    fn info_with(limits: QueueLimits) -> QueueInfo {
        QueueInfo {
            id: 0,
            device: DeviceInfo::new(128, 512),
            limits,
        }
    }

    #[test]
    fn request_shape_rejects_dma_address_outside_mask() {
        let limits = QueueLimits {
            dma_mask: 0xffff,
            ..QueueLimits::simple(512, u64::MAX)
        };
        assert_eq!(
            validate_owned_request_shape(
                info_with(limits).device,
                limits,
                &request_with(0x1_0000, 512),
            ),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn request_shape_rejects_dma_range_tail_outside_mask() {
        let limits = QueueLimits {
            dma_mask: 0xffff,
            dma_alignment: 1,
            ..QueueLimits::simple(512, u64::MAX)
        };
        assert_eq!(
            validate_owned_request_shape(
                info_with(limits).device,
                limits,
                &request_with(0xff00, 512),
            ),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn request_shape_rejects_unaligned_dma_address() {
        let limits = QueueLimits::simple(512, u64::MAX);
        assert_eq!(
            validate_owned_request_shape(
                info_with(limits).device,
                limits,
                &request_with(0x1100, 512),
            ),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn request_shape_rejects_unaligned_dma_length() {
        let limits = QueueLimits {
            dma_alignment: 1,
            dma_length_alignment: 1024,
            max_blocks_per_request: 2,
            max_segment_size: 1024,
            ..QueueLimits::simple(512, u64::MAX)
        };
        assert_eq!(
            validate_owned_request_shape(
                info_with(limits).device,
                limits,
                &request_with(0x1000, 512),
            ),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn request_shape_rejects_segment_boundary_crossing() {
        let limits = QueueLimits {
            dma_alignment: 1,
            segment_boundary: Some(4096),
            max_blocks_per_request: 2,
            max_segment_size: 1024,
            ..QueueLimits::simple(512, u64::MAX)
        };
        assert_eq!(
            validate_owned_request_shape(
                info_with(limits).device,
                limits,
                &request_with(0x0e00, 1024),
            ),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn request_shape_rejects_segment_larger_than_hardware_limit() {
        let limits = QueueLimits {
            max_blocks_per_request: 2,
            max_segment_size: 512,
            ..QueueLimits::simple(512, u64::MAX)
        };
        assert_eq!(
            validate_owned_request_shape(
                info_with(limits).device,
                limits,
                &request_with(0x1000, 1024),
            ),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn request_shape_rejects_write_to_read_only_device() {
        let limits = QueueLimits::simple(512, u64::MAX);
        let mut info = info_with(limits);
        info.device.read_only = true;

        assert_eq!(
            validate_owned_request_shape(info.device, limits, &request_with(0x1000, 512),),
            Err(BlkError::NotSupported)
        );
    }
}
