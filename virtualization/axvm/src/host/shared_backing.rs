//! Platform implementation of the ivshmem shared-backing allocator.
//!
//! Link backing must be physically contiguous and guest-mappable, so the
//! allocator reserves contiguous DMA frames from the platform frame
//! allocator and hands out the host-physical base plus the permanent
//! hypervisor virtual view. The frames return to the platform allocator on
//! release.

use std::sync::Arc;

use axdevice::{BackingAllocation, IvshmemError, SharedBackingAllocator};
use axvm_types::HostPhysAddr;

use super::{HostMemory, default_host};

const PAGE_SIZE: u64 = 0x1000;

/// Platform backing allocator over the ArceOS frame allocator.
pub struct PlatformSharedBackingAllocator;

// SAFETY: the platform frame allocator returns one physically contiguous,
// page-aligned reservation and `phys_to_virt` provides its permanent writable
// hypervisor mapping. The move-only token transfers that reservation to the
// link backing.
unsafe impl SharedBackingAllocator for PlatformSharedBackingAllocator {
    fn allocate(&self, size: u64) -> Result<BackingAllocation, IvshmemError> {
        const OPERATION: &str = "allocate ivshmem link backing";
        if size == 0 || !size.is_multiple_of(PAGE_SIZE) {
            return Err(IvshmemError::AllocationFailed {
                operation: "allocate ivshmem link backing (size must be a non-zero 4 KiB multiple)",
            });
        }
        let num_frames = (size / PAGE_SIZE) as usize;
        // The platform allocator returns the host-physical base of the
        // contiguous reservation; phys_to_virt yields its permanent
        // hypervisor virtual view.
        let Some(hpa) = default_host().alloc_contiguous_frames(num_frames, PAGE_SIZE as usize)
        else {
            return Err(IvshmemError::AllocationFailed {
                operation: OPERATION,
            });
        };
        let virt = default_host().phys_to_virt(hpa);
        // SAFETY: `hpa` and `virt` identify the unique contiguous reservation
        // just obtained above, whose complete `size` is mapped by the host.
        Ok(unsafe {
            BackingAllocation::from_raw_parts(
                hpa.as_usize() as u64,
                size,
                virt.as_usize() as *mut u8,
            )
        })
    }

    unsafe fn release(&self, allocation: BackingAllocation) {
        // SAFETY: the caller passes the exact token returned by this allocator
        // after all accesses have ceased, as required by the allocator trait.
        let hpa = HostPhysAddr::from(allocation.hpa_base() as usize);
        let num_frames = (allocation.size() / PAGE_SIZE) as usize;
        default_host().dealloc_contiguous_frames(hpa, num_frames);
    }
}

/// Creates the process-wide shared-backing allocator for ivshmem links.
pub fn platform_shared_backing_allocator() -> Arc<dyn SharedBackingAllocator> {
    Arc::new(PlatformSharedBackingAllocator)
}
