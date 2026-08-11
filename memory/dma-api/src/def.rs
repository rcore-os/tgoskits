use core::{alloc::Layout, cmp::PartialOrd, num::NonZeroU64, ptr::NonNull};

use derive_more::{
    Add, AddAssign, Debug, Display, Div, From, Into, Mul, MulAssign, Sub, SubAssign,
};

#[derive(
    Debug,
    Display,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Hash,
    From,
    Into,
    Add,
    AddAssign,
    Mul,
    MulAssign,
    Sub,
    SubAssign,
    Div,
)]
#[debug("{}", format_args!("{_0:#X}"))]
#[display("{}", format_args!("{_0:#X}"))]
pub struct DmaAddr(u64);

impl DmaAddr {
    pub fn as_u64(&self) -> u64 {
        self.0
    }

    pub fn checked_add(&self, rhs: u64) -> Option<Self> {
        self.0.checked_add(rhs).map(DmaAddr)
    }
}

impl PartialEq<u64> for DmaAddr {
    fn eq(&self, other: &u64) -> bool {
        self.0 == *other
    }
}

impl PartialOrd<u64> for DmaAddr {
    fn partial_cmp(&self, other: &u64) -> Option<core::cmp::Ordering> {
        self.0.partial_cmp(other)
    }
}

/// Stable identity for one DMA translation domain.
///
/// Drivers use this to reject already-prepared DMA buffers that were prepared
/// for a different device/IOMMU domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DmaDomainId(NonZeroU64);

impl DmaDomainId {
    pub const fn new(id: NonZeroU64) -> Self {
        Self(id)
    }

    /// Compatibility domain for legacy callers that have not plumbed a
    /// device/IOMMU-specific identity yet.
    pub const fn legacy_global() -> Self {
        Self(NonZeroU64::MIN)
    }

    pub fn from_raw(id: u64) -> Self {
        Self(NonZeroU64::new(id).unwrap_or(NonZeroU64::MIN))
    }

    pub const fn get(self) -> NonZeroU64 {
        self.0
    }
}

/// Device-visible DMA constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaConstraints {
    pub addr_mask: u64,
    pub align: usize,
    pub boundary: Option<usize>,
    pub max_segment_size: Option<usize>,
}

impl DmaConstraints {
    pub const fn new(addr_mask: u64) -> Self {
        Self {
            addr_mask,
            align: 1,
            boundary: None,
            max_segment_size: None,
        }
    }

    pub fn with_align(mut self, align: usize) -> Self {
        self.align = align.max(1);
        self
    }

    pub fn with_boundary(mut self, boundary: usize) -> Self {
        self.boundary = Some(boundary.max(1));
        self
    }

    pub fn with_max_segment_size(mut self, max_segment_size: usize) -> Self {
        self.max_segment_size = Some(max_segment_size);
        self
    }
}

/// DMA transfer direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DmaDirection {
    /// CPU writes, device reads.
    ToDevice,
    /// Device writes, CPU reads.
    FromDevice,
    /// CPU and device may both read/write.
    Bidirectional,
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum DmaError {
    #[error("DMA allocation failed")]
    NoMemory,
    #[error("Invalid layout")]
    LayoutError(#[from] core::alloc::LayoutError),
    #[error("DMA address {addr} does not match device mask {mask:#X}")]
    DmaMaskNotMatch { addr: DmaAddr, mask: u64 },
    #[error("DMA align mismatch: required={required:#X}, but address={address}")]
    AlignMismatch { required: usize, address: DmaAddr },
    #[error("DMA segment size {size:#X} exceeds max segment size {max:#X}")]
    SegmentTooLarge { size: usize, max: usize },
    #[error("DMA address range crosses boundary {boundary:#X}: addr={addr}, size={size:#X}")]
    BoundaryCross {
        addr: DmaAddr,
        size: usize,
        boundary: usize,
    },
    #[error("Null pointer provided for DMA mapping")]
    NullPointer,
    #[error("Zero-sized buffer cannot be used for DMA")]
    ZeroSizedBuffer,
    #[error("DMA coherent allocation could not be released and was quarantined")]
    CoherentReleaseFailed,
}

/// Marker for plain data that can be safely stored in typed DMA buffers.
///
/// Types with restricted bit patterns, such as `bool`, must not implement this
/// trait because a device can write bytes that do not represent a valid value.
///
/// ```compile_fail
/// use dma_api::DmaPod;
///
/// fn require_dma_pod<T: DmaPod>() {}
/// require_dma_pod::<bool>();
/// ```
///
/// # Safety
///
/// Implementors must be `Copy`, permit every possible bit pattern, and contain
/// no references, pointers, or owned resources whose validity can be broken by
/// raw device writes. Padding bytes are allowed, but drivers must not rely on
/// their contents.
pub unsafe trait DmaPod: Copy {}

macro_rules! impl_dma_pod_for_scalars {
    ($($ty:ty),+ $(,)?) => {
        $(
            // SAFETY: Integer and floating-point scalars accept every bit pattern and
            // contain no references or owned resources.
            unsafe impl DmaPod for $ty {}
        )+
    };
}

impl_dma_pod_for_scalars!(
    u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize, f32, f64,
);

// SAFETY: An array has the same validity requirements as its elements and
// contains no additional references or owned resources.
unsafe impl<T: DmaPod, const N: usize> DmaPod for [T; N] {}

/// Backend-owned token for one DMA allocation.
///
/// The token is consumed by the matching deallocation operation and therefore
/// cannot be copied.
///
/// ```compile_fail
/// use dma_api::DmaAllocHandle;
///
/// fn require_copy<T: Copy>() {}
/// require_copy::<DmaAllocHandle>();
/// ```
///
/// ```compile_fail
/// use dma_api::DmaAllocHandle;
///
/// fn require_clone<T: Clone>() {}
/// require_clone::<DmaAllocHandle>();
/// ```
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct DmaAllocHandle {
    pub(crate) cpu_addr: NonNull<u8>,
    pub(crate) dma_addr: DmaAddr,
    pub(crate) layout: Layout,
}

impl DmaAllocHandle {
    /// # Safety
    ///
    /// `cpu_addr` must point to a live allocation described by `layout`, and
    /// `dma_addr` must be the device-visible address for that allocation.
    pub unsafe fn new(cpu_addr: NonNull<u8>, dma_addr: DmaAddr, layout: Layout) -> Self {
        Self {
            cpu_addr,
            dma_addr,
            layout,
        }
    }

    pub fn size(&self) -> usize {
        self.layout.size()
    }

    pub fn align(&self) -> usize {
        self.layout.align()
    }

    pub fn as_ptr(&self) -> NonNull<u8> {
        self.cpu_addr
    }

    pub fn dma_addr(&self) -> DmaAddr {
        self.dma_addr
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }
}

/// Backend-owned token for one streaming DMA mapping.
///
/// The token is consumed by the matching unmap operation and therefore cannot
/// be copied.
///
/// ```compile_fail
/// use dma_api::DmaMapHandle;
///
/// fn require_copy<T: Copy>() {}
/// require_copy::<DmaMapHandle>();
/// ```
///
/// ```compile_fail
/// use dma_api::DmaMapHandle;
///
/// fn require_clone<T: Clone>() {}
/// require_clone::<DmaMapHandle>();
/// ```
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct DmaMapHandle {
    pub(crate) cpu_addr: NonNull<u8>,
    pub(crate) dma_addr: DmaAddr,
    pub(crate) layout: Layout,
    pub(crate) bounce_ptr: Option<NonNull<u8>>,
}

impl DmaMapHandle {
    /// # Safety
    ///
    /// `cpu_addr` must point to the caller-owned mapped buffer for the mapping
    /// lifetime. `bounce_ptr`, when present, must point to a live bounce buffer
    /// described by `layout`.
    pub unsafe fn new(
        cpu_addr: NonNull<u8>,
        dma_addr: DmaAddr,
        layout: Layout,
        bounce_ptr: Option<NonNull<u8>>,
    ) -> Self {
        Self {
            cpu_addr,
            dma_addr,
            layout,
            bounce_ptr,
        }
    }

    pub fn size(&self) -> usize {
        self.layout.size()
    }

    pub fn align(&self) -> usize {
        self.layout.align()
    }

    pub fn as_ptr(&self) -> NonNull<u8> {
        self.cpu_addr
    }

    pub fn dma_addr(&self) -> DmaAddr {
        self.dma_addr
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn bounce_ptr(&self) -> Option<NonNull<u8>> {
        self.bounce_ptr
    }
}
