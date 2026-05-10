use std::os::arceos;

use ax_memory_addr::PAGE_SIZE_4K;
use axaddrspace::{AxMmHal, HostPhysAddr, HostVirtAddr};
use axvisor_api::memory::MemoryIf;

use crate::hal::AxMmHalImpl;

struct MemoryImpl;

#[axvisor_api::api_impl]
impl MemoryIf for MemoryImpl {
    fn alloc_frame() -> Option<HostPhysAddr> {
        <AxMmHalImpl as AxMmHal>::alloc_frame()
    }

    fn alloc_contiguous_frames(num_frames: usize, frame_align: usize) -> Option<HostPhysAddr> {
        arceos::modules::ax_alloc::global_allocator()
            .alloc_pages(
                num_frames,
                frame_align.max(PAGE_SIZE_4K),
                arceos::modules::ax_alloc::UsageKind::Dma,
            )
            .map(|vaddr| <AxMmHalImpl as AxMmHal>::virt_to_phys(vaddr.into()))
            .ok()
    }

    fn dealloc_frame(paddr: HostPhysAddr) {
        <AxMmHalImpl as AxMmHal>::dealloc_frame(paddr)
    }

    fn dealloc_contiguous_frames(paddr: HostPhysAddr, num_frames: usize) {
        let vaddr = <AxMmHalImpl as AxMmHal>::phys_to_virt(paddr).as_usize();
        arceos::modules::ax_alloc::global_allocator().dealloc_pages(
            vaddr,
            num_frames,
            arceos::modules::ax_alloc::UsageKind::Dma,
        );
    }

    fn phys_to_virt(paddr: HostPhysAddr) -> HostVirtAddr {
        <AxMmHalImpl as AxMmHal>::phys_to_virt(paddr)
    }

    fn virt_to_phys(vaddr: HostVirtAddr) -> HostPhysAddr {
        <AxMmHalImpl as AxMmHal>::virt_to_phys(vaddr)
    }
}
