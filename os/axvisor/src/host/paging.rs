use ax_page_table_multiarch::PagingHandler;
use ax_std::os::arceos::modules;
use axaddrspace::{AxMmHal, HostPhysAddr, HostVirtAddr};

pub struct HostPaging;

impl AxMmHal for HostPaging {
    fn alloc_frame() -> Option<HostPhysAddr> {
        <modules::ax_hal::paging::PagingHandlerImpl as PagingHandler>::alloc_frame()
    }

    fn dealloc_frame(paddr: HostPhysAddr) {
        <modules::ax_hal::paging::PagingHandlerImpl as PagingHandler>::dealloc_frame(paddr)
    }

    #[inline]
    fn phys_to_virt(paddr: HostPhysAddr) -> HostVirtAddr {
        <modules::ax_hal::paging::PagingHandlerImpl as PagingHandler>::phys_to_virt(paddr)
    }

    fn virt_to_phys(vaddr: HostVirtAddr) -> HostPhysAddr {
        modules::ax_hal::mem::virt_to_phys(vaddr)
    }
}

pub type HostPagingHandler = modules::ax_hal::paging::PagingHandlerImpl;
