//! Internal host capability traits used by the AxVM runtime.

use core::time::Duration;

use axvm_types::{HostPhysAddr, HostVirtAddr};

use crate::AxVmResult;

/// Host memory allocation and address translation.
pub trait HostMemory {
    /// Allocate one 4 KiB host frame.
    fn alloc_frame(&self) -> Option<HostPhysAddr>;

    /// Free one frame returned by [`HostMemory::alloc_frame`].
    fn dealloc_frame(&self, paddr: HostPhysAddr);

    /// Allocate contiguous host frames.
    fn alloc_contiguous_frames(
        &self,
        num_frames: usize,
        frame_align: usize,
    ) -> Option<HostPhysAddr>;

    /// Free contiguous host frames.
    fn dealloc_contiguous_frames(&self, paddr: HostPhysAddr, num_frames: usize);

    /// Convert a host physical address to a host virtual address.
    fn phys_to_virt(&self, paddr: HostPhysAddr) -> HostVirtAddr;

    /// Convert a host virtual address to a host physical address.
    fn virt_to_phys(&self, vaddr: HostVirtAddr) -> HostPhysAddr;
}

/// Host time and timer operations.
pub trait HostTime {
    /// Read monotonic host time.
    fn monotonic_time(&self) -> Duration;
}

/// Host CPU topology and affinity operations.
pub trait HostCpu {
    /// CPU affinity mask type.
    type CpuMask: Send + Sync + 'static;

    /// Number of usable host CPUs.
    fn cpu_count(&self) -> usize;
}

/// Host platform lifecycle and virtualization controls.
pub trait HostPlatform {
    /// Check whether hardware virtualization is available.
    fn has_hardware_support(&self) -> bool;

    /// Enable virtualization on every usable host CPU.
    fn enable_virtualization_on_all_cpus(&self) -> AxVmResult;
}
