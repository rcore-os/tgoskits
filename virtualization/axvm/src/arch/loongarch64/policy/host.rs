//! Host callbacks required by the OS-neutral LoongArch vCPU core.

use core::time::Duration;
use std::boxed::Box;

use super::{LoongArchHostPhysAddr, LoongArchHostVirtAddr, LoongArchVcpuResult};

/// Host operations required by LoongArch virtualization code.
pub trait LoongArchHostOps {
    /// Opaque ownership token for one host timer registration.
    type TimerHandle: Copy;

    /// Convert a host virtual address to a host physical address.
    fn virt_to_phys(vaddr: LoongArchHostVirtAddr) -> LoongArchHostPhysAddr;

    /// Read monotonic host time in nanoseconds.
    fn current_time_nanos() -> u64;

    /// Convert LoongArch timer ticks to nanoseconds.
    fn ticks_to_nanos(ticks: u64) -> u64;

    /// Register a guest timer callback at an absolute host deadline.
    fn register_timer(
        deadline: Duration,
        callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> LoongArchVcpuResult<Self::TimerHandle>;

    /// Cancel a guest timer callback and wait for its retirement.
    ///
    /// Task context only, on the timer's vCPU owner. The underlying hard-IRQ
    /// host cancellation is not a completion barrier: it may report that the
    /// callback is still executing or its payload is still being reclaimed, so
    /// this port must not return until the stable registration is fully retired.
    /// A returned error means the registration is still live and must be
    /// retried rather than dropped.
    fn cancel_timer(handle: Self::TimerHandle) -> LoongArchVcpuResult;
}
