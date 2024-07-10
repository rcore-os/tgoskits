// use super::vmexit;
use axvm::{AxVMHal, HostPhysAddr, HostVirtAddr};
use memory_addr::PAGE_SIZE_4K as PAGE_SIZE;

pub struct AxVMHalImpl;

impl AxVMHal for AxVMHalImpl {
    fn alloc_page() -> Option<HostPhysAddr> {
        axalloc::global_allocator()
            .alloc_pages(1, PAGE_SIZE)
            .map(|vaddr| axhal::mem::virt_to_phys(vaddr.into()))
            .ok()
    }

    fn dealloc_page(paddr: HostPhysAddr) {
        axalloc::global_allocator().dealloc_pages(axhal::mem::phys_to_virt(paddr).as_usize(), 1)
    }

    fn phys_to_virt(paddr: HostPhysAddr) -> HostVirtAddr {
        axhal::mem::phys_to_virt(paddr)
    }

    fn virt_to_phys(vaddr: HostVirtAddr) -> HostPhysAddr {
        axhal::mem::virt_to_phys(vaddr)
    }

    // fn vmexit_handler(vcpu: &mut AxVMVcpu<Self>) {
    //     vmexit::vmexit_handler(vcpu).unwrap()
    // }

    fn current_time_nanos() -> u64 {
        axhal::time::current_time_nanos()
    }
}
