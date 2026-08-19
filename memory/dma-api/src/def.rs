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

/// Identity of the address domain used by one DMA device.
///
/// Drivers use this to reject already-prepared DMA buffers that were prepared
/// for a different device/IOMMU domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DmaDomainId {
    /// Device addresses are physical addresses shared by direct-mapped devices.
    Direct,
    /// Device addresses are translated in the identified IOMMU domain.
    Translated(NonZeroU64),
}

/// Device-visible DMA constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaConstraints {
    pub addr_mask: u64,
    pub align: usize,
    pub boundary: Option<usize>,
    pub max_segment_size: Option<usize>,
}

/// Cache-coherency relationship between one DMA device and the CPU.
///
/// This is a device property supplied by firmware or the platform bus. It is
/// independent from address-mask and segment-layout constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DmaCoherency {
    /// CPU and device observe the same cacheable mapping without explicit
    /// cache maintenance.
    Coherent,
    /// CPU ownership transitions require cache maintenance or a coherent CPU
    /// mapping supplied by the DMA backend.
    NonCoherent,
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

    pub const fn with_align(mut self, align: usize) -> Self {
        self.align = if align == 0 { 1 } else { align };
        self
    }

    pub const fn with_boundary(mut self, boundary: usize) -> Self {
        self.boundary = Some(if boundary == 0 { 1 } else { boundary });
        self
    }

    pub const fn with_max_segment_size(mut self, max_segment_size: usize) -> Self {
        self.max_segment_size = Some(max_segment_size);
        self
    }
}

/// Complete device-scoped DMA capability metadata.
///
/// This value deliberately contains no OS backend. It can cross portable
/// driver boundaries without exposing platform implementation details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaDeviceInfo {
    domain: DmaDomainId,
    coherency: DmaCoherency,
    constraints: DmaConstraints,
}

impl DmaDeviceInfo {
    pub const fn new(
        domain: DmaDomainId,
        coherency: DmaCoherency,
        constraints: DmaConstraints,
    ) -> Self {
        Self {
            domain,
            coherency,
            constraints,
        }
    }

    pub const fn domain(self) -> DmaDomainId {
        self.domain
    }

    pub const fn coherency(self) -> DmaCoherency {
        self.coherency
    }

    pub const fn constraints(self) -> DmaConstraints {
        self.constraints
    }

    pub const fn with_constraints(self, constraints: DmaConstraints) -> Self {
        Self {
            constraints,
            ..self
        }
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
/// # Safety
///
/// Implementors must be `Copy`, have no invalid all-zero bit pattern, and must
/// not own resources or references whose validity can be broken by raw device
/// writes.
pub unsafe trait DmaPod: Copy {}

unsafe impl<T: Copy> DmaPod for T {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmaAllocHandle {
    pub(crate) cpu_addr: NonNull<u8>,
    pub(crate) allocation_addr: NonNull<u8>,
    pub(crate) dma_addr: DmaAddr,
    pub(crate) layout: Layout,
}

impl DmaAllocHandle {
    /// Creates a handle from its CPU-visible and allocator-owned addresses.
    ///
    /// # Safety
    ///
    /// `cpu_addr` and `allocation_addr` must refer to the same live physical
    /// allocation described by `layout`. `cpu_addr` must remain the only CPU
    /// mapping exposed to the allocation owner until deallocation, and
    /// `dma_addr` must be the device-visible address for those pages.
    pub unsafe fn new(
        cpu_addr: NonNull<u8>,
        allocation_addr: NonNull<u8>,
        dma_addr: DmaAddr,
        layout: Layout,
    ) -> Self {
        Self {
            cpu_addr,
            allocation_addr,
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

    /// Returns the allocator-owned address required by the DMA backend when
    /// releasing this handle.
    pub fn allocation_ptr(&self) -> NonNull<u8> {
        self.allocation_addr
    }

    pub fn dma_addr(&self) -> DmaAddr {
        self.dma_addr
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coherent_handle_keeps_cpu_alias_and_allocator_address_distinct() {
        let alias = NonNull::new(0x8000_usize as *mut u8).unwrap();
        let allocation = NonNull::new(0x4000_usize as *mut u8).unwrap();
        let layout = Layout::from_size_align(0x1000, 0x1000).unwrap();
        let handle = unsafe { DmaAllocHandle::new(alias, allocation, 0x2000_u64.into(), layout) };

        assert_eq!(handle.as_ptr(), alias);
        assert_eq!(handle.allocation_ptr(), allocation);
        assert_eq!(handle.dma_addr(), DmaAddr::from(0x2000_u64));
    }
}
