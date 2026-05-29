use ax_memory_addr::PAGE_SIZE_4K;
use ax_std::os::arceos::modules;
use axaddrspace::{AxMmHal, HostPhysAddr, HostVirtAddr};

use crate::host::paging::HostPaging;

pub fn alloc_frame() -> Option<HostPhysAddr> {
    <HostPaging as AxMmHal>::alloc_frame()
}

pub fn dealloc_frame(paddr: HostPhysAddr) {
    <HostPaging as AxMmHal>::dealloc_frame(paddr);
}

pub fn alloc_contiguous_frames(num_frames: usize, frame_align: usize) -> Option<HostPhysAddr> {
    modules::ax_alloc::global_allocator()
        .alloc_pages(
            num_frames,
            frame_align.max(PAGE_SIZE_4K),
            modules::ax_alloc::UsageKind::Dma,
        )
        .map(|vaddr| virt_to_phys(vaddr.into()))
        .ok()
}

pub fn dealloc_contiguous_frames(paddr: HostPhysAddr, num_frames: usize) {
    let vaddr = phys_to_virt(paddr).as_usize();
    modules::ax_alloc::global_allocator().dealloc_pages(
        vaddr,
        num_frames,
        modules::ax_alloc::UsageKind::Dma,
    );
}

pub fn phys_to_virt(paddr: HostPhysAddr) -> HostVirtAddr {
    <HostPaging as AxMmHal>::phys_to_virt(paddr)
}

pub fn virt_to_phys(vaddr: HostVirtAddr) -> HostPhysAddr {
    <HostPaging as AxMmHal>::virt_to_phys(vaddr)
}
