use axvm::AxVMHal;
// Todo: should we know about HostPhysAddr and HostVirtAddr here???
use axaddrspace::{HostPhysAddr, HostVirtAddr};

/// Implementation for `AxVMHal` trait.
pub struct AxVMHalImpl;

impl AxVMHal for AxVMHalImpl {
    type PagingHandler = axhal::paging::PagingHandlerImpl;

    fn virt_to_phys(vaddr: HostVirtAddr) -> HostPhysAddr {
        axhal::mem::virt_to_phys(vaddr)
    }

    fn current_time_nanos() -> u64 {
        axhal::time::monotonic_time_nanos()
    }
}

/// This design might seem strange,
/// but the underlying reason is that the vCPU implementations for ARM and RISC-V architectures
/// **DO NOT** require dependency on OS-related resource management interfaces.
///
/// However, the vCPU implementation for the x86_64 architecture relies on OS-provided physical memory management interfaces to allocate memory for VMX-related control regions.
/// To avoid unnecessary Rust generic type applications, we decided to introduce `crate_interface` in the [`x86_vcpu`](https://github.com/arceos-hypervisor/x86_vcpu) crate
/// and use it to call OS-related resource allocation interfaces to implement `PhysFrameIf`.
#[cfg(target_arch = "x86_64")]
mod frame_x86 {
    use axaddrspace::{HostPhysAddr, HostVirtAddr};
    use page_table_multiarch::PagingHandler;

    use axvm::AxVMHal;

    use crate::hal::AxVMHalImpl;

    /// Implementation for `PhysFrameIf` trait provided by [x86_vcpu](https://github.com/arceos-hypervisor/x86_vcpu) crate.
    struct PhysFrameIfImpl;

    #[crate_interface::impl_interface]
    impl x86_vcpu::PhysFrameIf for PhysFrameIfImpl {
        fn alloc_frame() -> Option<HostPhysAddr> {
            <AxVMHalImpl as AxVMHal>::PagingHandler::alloc_frame()
        }

        fn dealloc_frame(paddr: HostPhysAddr) {
            <AxVMHalImpl as AxVMHal>::PagingHandler::dealloc_frame(paddr)
        }

        #[inline]
        fn phys_to_virt(paddr: HostPhysAddr) -> HostVirtAddr {
            <AxVMHalImpl as AxVMHal>::PagingHandler::phys_to_virt(paddr)
        }
    }
}
