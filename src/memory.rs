//! Memory allocation and address translation APIs.

pub use memory_addr::{PhysAddr, VirtAddr};

/// The API trait for memory allocation and address translation functionalities.
#[crate::api_def]
pub trait MemoryIf {
    /// Allocate a frame.
    fn alloc_frame() -> Option<PhysAddr>;
    /// Allocate a number of contiguous frames, with a specified alignment.
    fn alloc_contiguous_frames(num_frames: usize, frame_align_pow2: usize) -> Option<PhysAddr>;
    /// Deallocate a frame allocated previously by [`alloc_frame`].
    fn dealloc_frame(addr: PhysAddr);
    /// Deallocate a number of contiguous frames allocated previously by
    /// [`alloc_contiguous_frames`].
    fn dealloc_contiguous_frames(first_addr: PhysAddr, num_frames: usize);
    /// Convert a physical address to a virtual address.
    fn phys_to_virt(addr: PhysAddr) -> VirtAddr;
    /// Convert a virtual address to a physical address.
    fn virt_to_phys(addr: VirtAddr) -> PhysAddr;
}

/// [`AxMmHal`](axaddrspace::AxMmHal) implementation by axvisor_api.
#[doc(hidden)]
pub struct AxMmHalApiImpl;

impl axaddrspace::AxMmHal for AxMmHalApiImpl {
    fn alloc_frame() -> Option<PhysAddr> {
        alloc_frame()
    }

    fn dealloc_frame(addr: PhysAddr) {
        dealloc_frame(addr)
    }

    fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
        phys_to_virt(addr)
    }

    fn virt_to_phys(addr: VirtAddr) -> PhysAddr {
        virt_to_phys(addr)
    }
}

/// A physical frame which will be automatically deallocated when dropped.
pub type PhysFrame = axaddrspace::PhysFrame<AxMmHalApiImpl>;
