//! Internal host capability traits used by the AxVM runtime.

extern crate alloc;

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "loongarch64"
))]
use alloc::boxed::Box;
use core::time::Duration;

use ax_errno::AxResult;
use axvm_types::{HostPhysAddr, HostVirtAddr};

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
    /// Timer cancellation token.
    type CancelToken: Copy + Send + Sync + 'static;

    /// Convert nanoseconds to hardware ticks.
    #[cfg(target_arch = "x86_64")]
    fn nanos_to_ticks(&self, nanos: u64) -> u64;

    /// Read monotonic host time.
    fn monotonic_time(&self) -> Duration;

    /// Program the host one-shot timer.
    fn set_oneshot_timer(&self, deadline_ns: u64);

    /// Register a VM timer callback.
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "loongarch64"
    ))]
    fn register_timer(
        &self,
        deadline_ns: u64,
        callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> Self::CancelToken;

    /// Cancel a VM timer callback.
    #[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))]
    fn cancel_timer(&self, token: Self::CancelToken);
}

/// Host CPU topology and affinity operations.
pub trait HostCpu {
    /// CPU affinity mask type.
    type CpuMask: Send + Sync + 'static;

    /// Number of usable host CPUs.
    fn cpu_count(&self) -> usize;

    /// Current host CPU ID.
    fn this_cpu_id(&self) -> usize;
}

/// Host console operations.
#[cfg(target_arch = "x86_64")]
pub trait HostConsole {
    /// Write raw bytes to host console.
    fn write_bytes(&self, bytes: &[u8]);

    /// Read raw bytes from host console.
    fn read_bytes(&self, bytes: &mut [u8]) -> usize;
}

/// Host platform lifecycle and virtualization controls.
pub trait HostPlatform {
    /// Check whether hardware virtualization is available.
    fn has_hardware_support(&self) -> bool;

    /// Enable virtualization on the current host CPU.
    fn enable_virtualization_on_current_cpu(&self) -> AxResult;

    /// Enable virtualization on every usable host CPU.
    fn enable_virtualization_on_all_cpus(&self) -> AxResult;
}
