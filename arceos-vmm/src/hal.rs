use axvm::AxVMHal;
use memory_addr::PAGE_SIZE_4K as PAGE_SIZE;
// Todo: should we know about HostPhysAddr and HostVirtAddr here???
use memory_addr::{PhysAddr, VirtAddr};

pub struct AxVMHalImpl;

impl AxVMHal for AxVMHalImpl {
    type PagingHandler = axhal::paging::PagingHandlerImpl;

    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        axhal::mem::virt_to_phys(vaddr)
    }

    // fn vmexit_handler(vcpu: &mut AxVMVcpu<Self>) {
    //     vmexit::vmexit_handler(vcpu).unwrap()
    // }

    fn current_time_nanos() -> u64 {
        axhal::time::monotonic_time_nanos()
    }
}
