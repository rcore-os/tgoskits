use axaddrspace::{HostPhysAddr, HostVirtAddr};
use axvisor_api::memory::MemoryIf;

struct MemoryImpl;

#[axvisor_api::api_impl]
impl MemoryIf for MemoryImpl {
    fn alloc_frame() -> Option<HostPhysAddr> {
        crate::host::memory::alloc_frame()
    }

    fn alloc_contiguous_frames(num_frames: usize, frame_align: usize) -> Option<HostPhysAddr> {
        crate::host::memory::alloc_contiguous_frames(num_frames, frame_align)
    }

    fn dealloc_frame(paddr: HostPhysAddr) {
        crate::host::memory::dealloc_frame(paddr);
    }

    fn dealloc_contiguous_frames(paddr: HostPhysAddr, num_frames: usize) {
        crate::host::memory::dealloc_contiguous_frames(paddr, num_frames);
    }

    fn phys_to_virt(paddr: HostPhysAddr) -> HostVirtAddr {
        crate::host::memory::phys_to_virt(paddr)
    }

    fn virt_to_phys(vaddr: HostVirtAddr) -> HostPhysAddr {
        crate::host::memory::virt_to_phys(vaddr)
    }
}
