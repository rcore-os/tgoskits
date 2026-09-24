use core::{alloc::Layout, ptr::NonNull};

use rdif_iommu::IommuError;

/// Direct physical, coherent memory used by the SMMU itself.
///
/// # Safety
/// Implementations must return a stable, physically contiguous region, at
/// least `layout.size()` bytes long and aligned in both CPU and physical
/// address spaces to `layout.align()`. The region must be coherent for SMMU
/// table walks and queue accesses. It must remain allocated until `deallocate`.
pub unsafe trait PhysicalMemory: Send + Sync {
    fn allocate(&'static self, layout: Layout) -> Result<PhysicalRegion, IommuError>;

    /// # Safety
    /// `ptr`, `physical`, and `layout` must be the exact values from one live
    /// allocation of this provider, passed once after SMMU access has stopped.
    unsafe fn deallocate(&self, ptr: NonNull<u8>, physical: u64, layout: Layout);
}

/// Owned controller memory. Translation domains keep page tables alive for
/// their entire binding lifetime; a failed IOTLB sync must retain them.
pub struct PhysicalRegion {
    ptr: NonNull<u8>,
    physical: u64,
    layout: Layout,
    owner: &'static dyn PhysicalMemory,
}

impl PhysicalRegion {
    /// # Safety
    /// The pointer and physical range satisfy `PhysicalMemory`'s contract and
    /// identify one live allocation owned by `owner`. No other owner may free
    /// it. The constructor zeroes the CPU mapping before hardware publication.
    pub unsafe fn new(
        ptr: NonNull<u8>,
        physical: u64,
        layout: Layout,
        owner: &'static dyn PhysicalMemory,
    ) -> Self {
        // SAFETY: The caller promises write access to layout.size() live bytes.
        unsafe { core::ptr::write_bytes(ptr.as_ptr(), 0, layout.size()) };
        Self {
            ptr,
            physical,
            layout,
            owner,
        }
    }

    pub const fn physical(&self) -> u64 {
        self.physical
    }

    pub const fn len(&self) -> usize {
        self.layout.size()
    }

    pub const fn is_empty(&self) -> bool {
        self.layout.size() == 0
    }

    pub(crate) fn read_u64(&self, index: usize) -> u64 {
        assert!(index < self.layout.size() / 8);
        // SAFETY: The checked index is within the aligned live allocation.
        u64::from_le(unsafe {
            core::ptr::read_volatile(self.ptr.as_ptr().cast::<u64>().add(index))
        })
    }

    pub(crate) fn write_u64(&self, index: usize, value: u64) {
        assert!(index < self.layout.size() / 8);
        // SAFETY: The checked index is within the aligned live allocation.
        unsafe {
            core::ptr::write_volatile(self.ptr.as_ptr().cast::<u64>().add(index), value.to_le())
        };
    }
}

// SAFETY: The allocation is stable and movable between CPUs; all mutable CPU
// accesses are serialized by the controller lock, while SMMU access uses the
// coherent hardware protocol and explicit command sync.
unsafe impl Send for PhysicalRegion {}

impl Drop for PhysicalRegion {
    fn drop(&mut self) {
        // SAFETY: This region is the unique owner of the allocation.
        unsafe { self.owner.deallocate(self.ptr, self.physical, self.layout) };
    }
}
