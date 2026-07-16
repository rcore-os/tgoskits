//! Host callbacks required by ARM virtual interrupt-controller devices.

use alloc::boxed::Box;
use core::time::Duration;

use ax_memory_addr::{PhysAddr, VirtAddr};

/// Host operations required by ARM VGIC and virtual timer components.
#[ax_crate_interface::def_interface]
pub trait ArmVgicHostIf {
    /// Allocate contiguous host frames.
    fn alloc_contiguous_frames(frame_count: usize, frame_align: usize) -> Option<PhysAddr>;

    /// Deallocate contiguous host frames.
    fn dealloc_contiguous_frames(start_paddr: PhysAddr, frame_count: usize);

    /// Convert host physical address to host virtual address.
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr;

    /// Return host CPU count.
    fn host_cpu_num() -> usize;

    /// Return current VM ID.
    fn current_vm_id() -> usize;

    /// Return current vCPU ID.
    fn current_vcpu_id() -> usize;

    /// Current monotonic host time in nanoseconds.
    fn current_time_nanos() -> u64;

    /// Register a timer callback and return its cancellation token.
    fn register_timer(
        deadline: Duration,
        callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> Option<usize>;

    /// Cancel a timer registration.
    fn cancel_timer(token: usize);

    /// Queue a virtual interrupt for one specific vCPU.
    fn queue_virtual_interrupt(vm_id: usize, vcpu_id: usize, vector: usize);

    /// Read VGICD IIDR from host GIC.
    fn read_vgicd_iidr() -> u32;

    /// Read VGICD TYPER from host GIC.
    fn read_vgicd_typer() -> u32;

    /// Return host GICD base.
    fn get_host_gicd_base() -> PhysAddr;

    /// Return host GICR base.
    fn get_host_gicr_base() -> PhysAddr;
}

#[cfg(feature = "vgicv3")]
pub(crate) fn alloc_contiguous_frames(frame_count: usize, frame_align: usize) -> Option<PhysAddr> {
    ax_crate_interface::call_interface!(ArmVgicHostIf::alloc_contiguous_frames(
        frame_count,
        frame_align
    ))
}

#[cfg(feature = "vgicv3")]
pub(crate) fn dealloc_contiguous_frames(start_paddr: PhysAddr, frame_count: usize) {
    ax_crate_interface::call_interface!(ArmVgicHostIf::dealloc_contiguous_frames(
        start_paddr,
        frame_count
    ));
}

#[cfg(feature = "vgicv3")]
pub(crate) fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
    ax_crate_interface::call_interface!(ArmVgicHostIf::phys_to_virt(paddr))
}

#[cfg(feature = "vgicv3")]
pub(crate) fn host_cpu_num() -> usize {
    ax_crate_interface::call_interface!(ArmVgicHostIf::host_cpu_num())
}

pub(crate) fn current_vm_id() -> usize {
    ax_crate_interface::call_interface!(ArmVgicHostIf::current_vm_id())
}

pub(crate) fn current_vcpu_id() -> usize {
    ax_crate_interface::call_interface!(ArmVgicHostIf::current_vcpu_id())
}

pub(crate) fn current_time_nanos() -> u64 {
    ax_crate_interface::call_interface!(ArmVgicHostIf::current_time_nanos())
}

pub(crate) fn register_timer(
    deadline: Duration,
    callback: Box<dyn FnOnce(Duration) + Send + 'static>,
) -> Option<usize> {
    ax_crate_interface::call_interface!(ArmVgicHostIf::register_timer(deadline, callback))
}

pub(crate) fn cancel_timer(token: usize) {
    ax_crate_interface::call_interface!(ArmVgicHostIf::cancel_timer(token));
}

pub(crate) fn queue_virtual_interrupt(vm_id: usize, vcpu_id: usize, vector: usize) {
    ax_crate_interface::call_interface!(ArmVgicHostIf::queue_virtual_interrupt(
        vm_id, vcpu_id, vector
    ));
}

pub fn read_vgicd_iidr() -> u32 {
    ax_crate_interface::call_interface!(ArmVgicHostIf::read_vgicd_iidr())
}

pub fn read_vgicd_typer() -> u32 {
    ax_crate_interface::call_interface!(ArmVgicHostIf::read_vgicd_typer())
}

pub fn get_host_gicd_base() -> PhysAddr {
    ax_crate_interface::call_interface!(ArmVgicHostIf::get_host_gicd_base())
}

pub fn get_host_gicr_base() -> PhysAddr {
    ax_crate_interface::call_interface!(ArmVgicHostIf::get_host_gicr_base())
}
