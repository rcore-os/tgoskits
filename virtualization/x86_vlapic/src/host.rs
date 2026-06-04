//! Host callbacks required by x86 virtual interrupt-controller devices.

use alloc::boxed::Box;
use core::time::Duration;

use ax_errno::{AxResult, ax_err_type};
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, VirtAddr};
use axvm_types::{InterruptVector, VCpuId, VMId};

/// Host operations required by x86 vLAPIC, PIT, and serial emulation.
#[ax_crate_interface::def_interface]
pub trait X86VlapicHostIf {
    /// Allocate one host frame.
    fn alloc_frame() -> Option<PhysAddr>;

    /// Deallocate one host frame.
    fn dealloc_frame(paddr: PhysAddr);

    /// Convert host physical address to host virtual address.
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr;

    /// Convert host virtual address to host physical address.
    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr;

    /// Current monotonic host time.
    fn current_time() -> Duration;

    /// Current monotonic host time in nanoseconds.
    fn current_time_nanos() -> u64;

    /// Register a timer callback.
    fn register_timer(
        deadline: Duration,
        callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> usize;

    /// Cancel a timer callback.
    fn cancel_timer(token: usize);

    /// Write bytes to the host console.
    fn write_bytes(bytes: &[u8]);

    /// Read bytes from the host console.
    fn read_bytes(bytes: &mut [u8]) -> usize;

    /// Return the current VM ID.
    fn current_vm_id() -> VMId;

    /// Return the current VM vCPU count.
    fn current_vm_vcpu_num() -> usize;

    /// Return the active vCPU mask for the current VM.
    fn current_vm_active_vcpus() -> usize;

    /// Return the active vCPU mask for the given VM.
    fn active_vcpus(vm_id: VMId) -> Option<usize>;

    /// Inject a virtual interrupt into a vCPU.
    fn inject_interrupt(vm_id: VMId, vcpu_id: VCpuId, vector: InterruptVector) -> AxResult;
}

/// RAII host frame used by x86 virtual interrupt-controller structures.
#[derive(Debug)]
pub struct PhysFrame {
    start_paddr: PhysAddr,
}

impl PhysFrame {
    /// Allocate a host frame.
    pub fn alloc_zero() -> AxResult<Self> {
        let frame = Self::alloc()?;
        unsafe { core::ptr::write_bytes(frame.as_mut_ptr(), 0, PAGE_SIZE_4K) };
        Ok(frame)
    }

    fn alloc() -> AxResult<Self> {
        let start_paddr = ax_crate_interface::call_interface!(X86VlapicHostIf::alloc_frame())
            .ok_or_else(|| ax_err_type!(NoMemory, "allocate physical frame failed"))?;
        assert_ne!(start_paddr.as_usize(), 0);
        Ok(Self { start_paddr })
    }

    /// Get the starting physical address of the frame.
    pub fn start_paddr(&self) -> PhysAddr {
        self.start_paddr
    }

    /// Get a mutable pointer to the frame.
    pub fn as_mut_ptr(&self) -> *mut u8 {
        ax_crate_interface::call_interface!(X86VlapicHostIf::phys_to_virt(self.start_paddr))
            .as_mut_ptr()
    }
}

impl Drop for PhysFrame {
    fn drop(&mut self) {
        ax_crate_interface::call_interface!(X86VlapicHostIf::dealloc_frame(self.start_paddr));
        log::debug!(
            "[x86_vlapic] deallocated PhysFrame({:#x})",
            self.start_paddr
        );
    }
}

pub(crate) fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
    ax_crate_interface::call_interface!(X86VlapicHostIf::virt_to_phys(vaddr))
}

pub(crate) fn current_time() -> Duration {
    ax_crate_interface::call_interface!(X86VlapicHostIf::current_time())
}

pub(crate) fn current_time_nanos() -> u64 {
    ax_crate_interface::call_interface!(X86VlapicHostIf::current_time_nanos())
}

pub(crate) fn register_timer(
    deadline: Duration,
    callback: Box<dyn FnOnce(Duration) + Send + 'static>,
) -> usize {
    ax_crate_interface::call_interface!(X86VlapicHostIf::register_timer(deadline, callback))
}

pub(crate) fn cancel_timer(token: usize) {
    ax_crate_interface::call_interface!(X86VlapicHostIf::cancel_timer(token));
}

pub(crate) fn write_bytes(bytes: &[u8]) {
    ax_crate_interface::call_interface!(X86VlapicHostIf::write_bytes(bytes));
}

pub(crate) fn read_bytes(bytes: &mut [u8]) -> usize {
    ax_crate_interface::call_interface!(X86VlapicHostIf::read_bytes(bytes))
}

pub(crate) fn current_vm_id() -> VMId {
    ax_crate_interface::call_interface!(X86VlapicHostIf::current_vm_id())
}

pub(crate) fn current_vm_vcpu_num() -> usize {
    ax_crate_interface::call_interface!(X86VlapicHostIf::current_vm_vcpu_num())
}

pub(crate) fn current_vm_active_vcpus() -> usize {
    ax_crate_interface::call_interface!(X86VlapicHostIf::current_vm_active_vcpus())
}

pub(crate) fn active_vcpus(vm_id: VMId) -> Option<usize> {
    ax_crate_interface::call_interface!(X86VlapicHostIf::active_vcpus(vm_id))
}

pub(crate) fn inject_interrupt(vm_id: VMId, vcpu_id: VCpuId, vector: InterruptVector) -> AxResult {
    ax_crate_interface::call_interface!(X86VlapicHostIf::inject_interrupt(vm_id, vcpu_id, vector))
}
