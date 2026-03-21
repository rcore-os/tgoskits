use core::{alloc::Layout, ptr::NonNull};

use std::os::arceos;

use axaddrspace::{AxMmHal, HostPhysAddr, HostVirtAddr};
use axvisor_api::memory::MemoryIf;
use memory_addr::PAGE_SIZE_4K;

use crate::hal::AxMmHalImpl;

struct MemoryImpl;

#[axvisor_api::api_impl]
impl MemoryIf for MemoryImpl {
    fn alloc_frame() -> Option<HostPhysAddr> {
        <AxMmHalImpl as AxMmHal>::alloc_frame()
    }

    fn alloc_contiguous_frames(num_frames: usize, frame_align_pow2: usize) -> Option<HostPhysAddr> {
        arceos::modules::axalloc::global_allocator()
            .alloc(
                Layout::from_size_align(
                    num_frames * PAGE_SIZE_4K,
                    PAGE_SIZE_4K << frame_align_pow2,
                )
                .unwrap(),
            )
            // .alloc_pages(num_frames, PAGE_SIZE_4K << frame_align_pow2)
            // .map(|vaddr| <AxMmHalImpl as AxMmHal>::virt_to_phys(vaddr.into()))
            .map(|vaddr| HostPhysAddr::from(vaddr.as_ptr() as usize))
            .ok()
    }

    fn dealloc_frame(paddr: HostPhysAddr) {
        <AxMmHalImpl as AxMmHal>::dealloc_frame(paddr)
    }

    fn dealloc_contiguous_frames(paddr: HostPhysAddr, num_frames: usize) {
        // arceos::modules::axalloc::global_allocator().dealloc_pages(paddr.as_usize(), num_frames);
        arceos::modules::axalloc::global_allocator().dealloc(
            unsafe { NonNull::new_unchecked(paddr.as_usize() as _) },
            Layout::from_size_align(num_frames * PAGE_SIZE_4K, PAGE_SIZE_4K).unwrap(),
        );
    }

    fn phys_to_virt(paddr: HostPhysAddr) -> HostVirtAddr {
        <AxMmHalImpl as AxMmHal>::phys_to_virt(paddr)
    }

    fn virt_to_phys(vaddr: HostVirtAddr) -> HostPhysAddr {
        <AxMmHalImpl as AxMmHal>::virt_to_phys(vaddr)
    }
}
