//! Internal host capability traits used by the AxVM runtime.

use std::time::Duration;

use axvm_types::{HostPhysAddr, HostVirtAddr};

use crate::AxVmResult;

/// Action returned by a restartable task-context host timer.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostTimerAction {
    Complete,
    Rearm(Duration),
}

/// Action returned by an explicitly hard-IRQ-safe host timer.
#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostHardTimerAction {
    Complete,
    Disarm,
    Rearm(Duration),
}

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

/// Typed host deadline capability used by AxVM architectural and device timers.
pub trait HostTimer {
    type TimerHandle: Copy + Send + Sync + 'static;

    fn register_timer(
        &self,
        deadline: Duration,
        callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> AxVmResult<Self::TimerHandle>;

    #[cfg(target_arch = "x86_64")]
    fn register_restartable_timer(
        &self,
        deadline: Duration,
        callback: Box<dyn FnMut(Duration) -> HostTimerAction + Send + 'static>,
    ) -> AxVmResult<Self::TimerHandle>;

    /// Registers one stable callback that may run in hard IRQ context.
    ///
    /// # Safety
    ///
    /// The callback must be bounded, allocation-free, non-sleeping, and use
    /// only IRQ-safe pre-bound capabilities. It may not perform destruction or
    /// registry lookup.
    #[cfg(target_arch = "aarch64")]
    unsafe fn register_hard_restartable_timer(
        &self,
        deadline: Duration,
        callback: Box<dyn FnMut(Duration) -> HostHardTimerAction + Send + 'static>,
    ) -> AxVmResult<Self::TimerHandle>;

    #[cfg(target_arch = "aarch64")]
    fn arm_hard_timer(&self, handle: Self::TimerHandle, deadline: Duration) -> AxVmResult;

    #[cfg(target_arch = "aarch64")]
    fn disarm_hard_timer(&self, handle: Self::TimerHandle) -> AxVmResult;

    fn cancel_timer(&self, handle: Self::TimerHandle) -> AxVmResult<bool>;
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

/// Host platform lifecycle and virtualization controls.
pub trait HostPlatform {
    /// Check whether hardware virtualization is available.
    fn has_hardware_support(&self) -> bool;

    /// Enable virtualization on the current host CPU.
    fn enable_virtualization_on_current_cpu(&self) -> AxVmResult;

    /// Enable virtualization on every usable host CPU.
    fn enable_virtualization_on_all_cpus(&self) -> AxVmResult;
}
