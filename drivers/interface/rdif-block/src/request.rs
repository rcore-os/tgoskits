use core::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

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
        let limits = QueueLimits {
            max_blocks_per_request: 8,
            max_segment_size: 1024,
            ..QueueLimits::simple(512, u64::MAX)
        };
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

    fn queue_info_with(limits: QueueLimits) -> QueueInfo {
        QueueInfo {
            id: 0,
            device: DeviceInfo::new(64, 512),
            limits,
        }
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
    fn request_validation_rejects_transfer_larger_than_hard_block_limit() {
        let info = queue_info_with(QueueLimits {
            dma_mask: u64::MAX,
            dma_alignment: 512,
            max_blocks_per_request: 2,
            max_segments: 1,
            max_segment_size: 4096,
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
