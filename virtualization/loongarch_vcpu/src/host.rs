//! Host callbacks required by the OS-neutral LoongArch vCPU core.

extern crate alloc;

use alloc::boxed::Box;
use core::time::Duration;

use crate::{LoongArchHostPhysAddr, LoongArchHostVirtAddr};

/// Host operations required by LoongArch virtualization code.
pub trait LoongArchHostOps {
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
    ) -> usize;

    /// Cancel a guest timer callback.
    fn cancel_timer(token: usize);

    /// Queue an interrupt for a vCPU.
    fn inject_interrupt(vm_id: usize, vcpu_id: usize, vector: usize);
}
