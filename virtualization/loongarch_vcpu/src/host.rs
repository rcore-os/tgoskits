//! Host callbacks required by LoongArch vCPU implementation.

extern crate alloc;

use alloc::boxed::Box;
use core::time::Duration;

use ax_memory_addr::{PhysAddr, VirtAddr};

/// Host memory operations required by LoongArch virtualization code.
#[ax_crate_interface::def_interface]
pub trait LoongArchVcpuHostIf {
    /// Convert a host virtual address to host physical address.
    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr;

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

    /// Queue a routed external interrupt for a vCPU.
    fn inject_external_interrupt(vm_id: usize, vcpu_id: usize, vector: usize, physical_irq: usize);
}

#[cfg(target_arch = "loongarch64")]
pub(crate) fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
    ax_crate_interface::call_interface!(LoongArchVcpuHostIf::virt_to_phys(vaddr))
}

#[cfg(target_arch = "loongarch64")]
pub(crate) fn current_time_nanos() -> u64 {
    ax_crate_interface::call_interface!(LoongArchVcpuHostIf::current_time_nanos())
}

#[cfg(target_arch = "loongarch64")]
pub(crate) fn ticks_to_nanos(ticks: u64) -> u64 {
    ax_crate_interface::call_interface!(LoongArchVcpuHostIf::ticks_to_nanos(ticks))
}

#[cfg(target_arch = "loongarch64")]
pub(crate) fn register_timer(
    deadline: Duration,
    callback: Box<dyn FnOnce(Duration) + Send + 'static>,
) -> usize {
    ax_crate_interface::call_interface!(LoongArchVcpuHostIf::register_timer(deadline, callback))
}

#[cfg(target_arch = "loongarch64")]
pub(crate) fn cancel_timer(token: usize) {
    ax_crate_interface::call_interface!(LoongArchVcpuHostIf::cancel_timer(token))
}

#[cfg(target_arch = "loongarch64")]
pub(crate) fn inject_interrupt(vm_id: usize, vcpu_id: usize, vector: usize) {
    ax_crate_interface::call_interface!(LoongArchVcpuHostIf::inject_interrupt(
        vm_id, vcpu_id, vector
    ))
}
