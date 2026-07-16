//! Host callbacks required by x86 virtual interrupt-controller devices.

use core::marker::PhantomData;

use crate::{
    X86HostPhysAddr, X86HostVirtAddr, X86InterruptVector, X86TimerCallback, X86VcpuId,
    X86VlapicError, X86VlapicResult, X86VmId,
};

/// Size of a 4 KiB host frame.
pub const X86_PAGE_SIZE_4K: usize = 0x1000;

/// Host operations required by x86 vLAPIC, PIT, and serial emulation.
pub trait X86VlapicHostOps {
    /// Allocate one host frame.
    fn alloc_frame() -> Option<X86HostPhysAddr>;

    /// Deallocate one host frame.
    fn dealloc_frame(paddr: X86HostPhysAddr);

    /// Convert host physical address to host virtual address.
    fn phys_to_virt(paddr: X86HostPhysAddr) -> X86HostVirtAddr;

    /// Convert host virtual address to host physical address.
    fn virt_to_phys(vaddr: X86HostVirtAddr) -> X86HostPhysAddr;

    /// Current monotonic host time in nanoseconds.
    fn current_time_nanos() -> u64;

    /// Register a timer callback for an absolute host deadline in nanoseconds.
    fn register_timer(deadline_nanos: u64, callback: X86TimerCallback) -> Option<usize>;

    /// Cancel a timer callback.
    fn cancel_timer(token: usize);

    /// Return the current VM ID.
    fn current_vm_id() -> X86VmId;

    /// Return the current VM vCPU count.
    fn current_vm_vcpu_num() -> usize;

    /// Return the active vCPU mask for the current VM.
    fn current_vm_active_vcpus() -> usize;

    /// Return the active vCPU mask for the given VM.
    fn active_vcpus(vm_id: X86VmId) -> Option<usize>;

    /// Inject a virtual interrupt into a vCPU.
    fn inject_interrupt(
        vm_id: X86VmId,
        vcpu_id: X86VcpuId,
        vector: X86InterruptVector,
    ) -> X86VlapicResult;
}

/// RAII host frame used by x86 virtual interrupt-controller structures.
#[derive(Debug)]
pub struct PhysFrame<H: X86VlapicHostOps> {
    start_paddr: X86HostPhysAddr,
    _host: PhantomData<fn() -> H>,
}

impl<H: X86VlapicHostOps> PhysFrame<H> {
    /// Allocate a host frame.
    pub fn alloc_zero() -> X86VlapicResult<Self> {
        let frame = Self::alloc()?;
        unsafe { core::ptr::write_bytes(frame.as_mut_ptr(), 0, X86_PAGE_SIZE_4K) };
        Ok(frame)
    }

    fn alloc() -> X86VlapicResult<Self> {
        let start_paddr = H::alloc_frame().ok_or(X86VlapicError::NoMemory)?;
        assert_ne!(start_paddr.as_usize(), 0);
        Ok(Self {
            start_paddr,
            _host: PhantomData,
        })
    }

    /// Get the starting physical address of the frame.
    pub fn start_paddr(&self) -> X86HostPhysAddr {
        self.start_paddr
    }

    /// Get a mutable pointer to the frame.
    pub fn as_mut_ptr(&self) -> *mut u8 {
        H::phys_to_virt(self.start_paddr).as_mut_ptr()
    }
}

impl<H: X86VlapicHostOps> Drop for PhysFrame<H> {
    fn drop(&mut self) {
        H::dealloc_frame(self.start_paddr);
        log::debug!(
            "[x86_vlapic] deallocated PhysFrame({:#x})",
            self.start_paddr
        );
    }
}

pub(crate) fn virt_to_phys<H: X86VlapicHostOps>(vaddr: X86HostVirtAddr) -> X86HostPhysAddr {
    H::virt_to_phys(vaddr)
}

pub(crate) fn current_time_nanos<H: X86VlapicHostOps>() -> u64 {
    H::current_time_nanos()
}

pub(crate) fn register_timer<H: X86VlapicHostOps>(
    deadline_nanos: u64,
    callback: X86TimerCallback,
) -> Option<usize> {
    H::register_timer(deadline_nanos, callback)
}

pub(crate) fn cancel_timer<H: X86VlapicHostOps>(token: usize) {
    H::cancel_timer(token);
}

pub(crate) fn current_vm_vcpu_num<H: X86VlapicHostOps>() -> usize {
    H::current_vm_vcpu_num()
}

pub(crate) fn current_vm_active_vcpus<H: X86VlapicHostOps>() -> usize {
    H::current_vm_active_vcpus()
}

pub(crate) fn active_vcpus<H: X86VlapicHostOps>(vm_id: X86VmId) -> Option<usize> {
    H::active_vcpus(vm_id)
}

pub(crate) fn inject_interrupt<H: X86VlapicHostOps>(
    vm_id: X86VmId,
    vcpu_id: X86VcpuId,
    vector: X86InterruptVector,
) -> X86VlapicResult {
    H::inject_interrupt(vm_id, vcpu_id, vector)
}
