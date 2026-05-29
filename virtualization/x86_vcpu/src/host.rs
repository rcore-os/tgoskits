//! Host callbacks required by the x86 vCPU implementation.

use ax_memory_addr::{PhysAddr, VirtAddr};

/// Host memory and time operations required by x86 virtualization backends.
#[ax_crate_interface::def_interface]
pub trait X86VcpuHostIf {
    /// Allocate one host frame.
    fn alloc_frame() -> Option<PhysAddr>;

    /// Deallocate one host frame.
    fn dealloc_frame(paddr: PhysAddr);

    /// Allocate contiguous host frames.
    fn alloc_contiguous_frames(frame_count: usize, frame_align: usize) -> Option<PhysAddr>;

    /// Deallocate contiguous host frames.
    fn dealloc_contiguous_frames(start_paddr: PhysAddr, frame_count: usize);

    /// Convert host physical address to host virtual address.
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr;

    /// Convert nanoseconds to host ticks.
    fn nanos_to_ticks(nanos: u64) -> u64;
}

/// RAII host frame used by x86 VMX/SVM structures.
pub type PhysFrame = axaddrspace::PhysFrame<X86VcpuMmHal>;

/// Memory HAL backed by [`X86VcpuHostIf`].
#[derive(Debug)]
pub struct X86VcpuMmHal;

impl axaddrspace::AxMmHal for X86VcpuMmHal {
    fn alloc_frame() -> Option<PhysAddr> {
        ax_crate_interface::call_interface!(X86VcpuHostIf::alloc_frame())
    }

    fn dealloc_frame(paddr: PhysAddr) {
        ax_crate_interface::call_interface!(X86VcpuHostIf::dealloc_frame(paddr));
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        ax_crate_interface::call_interface!(X86VcpuHostIf::phys_to_virt(paddr))
    }

    fn virt_to_phys(_vaddr: VirtAddr) -> PhysAddr {
        unreachable!("x86_vcpu does not require host virtual-to-physical translation")
    }
}

#[cfg(feature = "svm")]
pub(crate) fn alloc_contiguous_frames(frame_count: usize, frame_align: usize) -> Option<PhysAddr> {
    ax_crate_interface::call_interface!(X86VcpuHostIf::alloc_contiguous_frames(
        frame_count,
        frame_align
    ))
}

#[cfg(feature = "svm")]
pub(crate) fn dealloc_contiguous_frames(start_paddr: PhysAddr, frame_count: usize) {
    ax_crate_interface::call_interface!(X86VcpuHostIf::dealloc_contiguous_frames(
        start_paddr,
        frame_count
    ));
}

#[cfg(any(feature = "vmx", feature = "svm"))]
pub(crate) fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
    ax_crate_interface::call_interface!(X86VcpuHostIf::phys_to_virt(paddr))
}

#[cfg(any(feature = "vmx", feature = "svm"))]
pub(crate) fn nanos_to_ticks(nanos: u64) -> u64 {
    ax_crate_interface::call_interface!(X86VcpuHostIf::nanos_to_ticks(nanos))
}
