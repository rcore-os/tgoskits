//! Host callbacks required by x86 virtual interrupt-controller devices.

use core::marker::PhantomData;

use crate::{
    X86HostPhysAddr, X86HostVirtAddr, X86InterruptVector, X86TimerCallback, X86VcpuId,
    X86VlapicResult, X86VmId,
};

/// Size of a 4 KiB host frame.
pub const X86_PAGE_SIZE_4K: usize = 0x1000;

/// Run-scoped operations injected into one vCPU-owned interrupt device.
///
/// A runtime port is created before guest entry and retains only pre-bound
/// lower capabilities. It must not search a global VM registry, acquire a
/// sleeping lock, or reach another vCPU's backend.
pub trait X86VlapicRuntimeOps: Send + Sync + 'static {
    /// Stable handle type returned by this host's timer service.
    type TimerHandle: Copy + Send + 'static;

    /// Returns the VM identity fixed when this port was created.
    fn vm_id(&self) -> X86VmId;

    /// Returns the source vCPU identity fixed when this port was created.
    fn vcpu_id(&self) -> X86VcpuId;

    /// Returns the number of vCPUs in this run.
    fn vcpu_count(&self) -> usize;

    /// Returns this run's active-vCPU mask.
    fn active_vcpu_mask(&self) -> usize;

    /// Publishes a virtual interrupt to a target vCPU and wakes its owner.
    fn inject_interrupt(
        &self,
        target_vcpu_id: X86VcpuId,
        vector: X86InterruptVector,
    ) -> X86VlapicResult;

    /// Fans one PIT IRQ0 edge through this run's legacy-PIC and IOAPIC paths.
    fn inject_pit_irq(&self) -> X86VlapicResult;

    /// Registers a task-context timer callback for an absolute deadline.
    fn register_timer(
        &self,
        deadline_nanos: u64,
        callback: X86TimerCallback,
    ) -> X86VlapicResult<Self::TimerHandle>;

    /// Registers a bounded hard-IRQ timer callback.
    ///
    /// # Safety
    ///
    /// The callback and all state transitions around it must be non-sleeping,
    /// allocation-free, and use only capabilities pre-bound to this run port.
    unsafe fn register_hard_timer(
        &self,
        deadline_nanos: u64,
        callback: X86TimerCallback,
    ) -> X86VlapicResult<Self::TimerHandle>;

    /// Yields the calling task while a timer callback or its host payload
    /// reclamation is still in flight.
    ///
    /// Task-side cancellation uses this instead of spinning so a callback that
    /// was preempted on the same CPU can run and retire its arm. It must not
    /// busy-wait, and it must not require any lock the callback acquires.
    fn wait_timer_progress(&self);

    /// Cancels a timer returned by this port and waits for its completion.
    ///
    /// Returns only once the callback has left the host timer queue and its
    /// payload is reclaimed (an already-completed registration reports
    /// success). A merely accepted but still-in-flight cancellation is not
    /// quiescence and must not be reported as success.
    fn cancel_timer(&self, handle: Self::TimerHandle) -> X86VlapicResult;
}

/// Host operations required by x86 vLAPIC and PIT emulation.
pub trait X86VlapicHostOps: 'static {
    /// Stable handle for one host timer registration.
    type TimerHandle: Copy + Send + 'static;

    /// Pre-bound run port handed to each vCPU-owned interrupt device.
    type Runtime: X86VlapicRuntimeOps<TimerHandle = Self::TimerHandle> + Clone;

    /// Allocate one host frame.
    fn alloc_frame() -> Option<X86HostPhysAddr>;

    /// Deallocate one host frame.
    fn dealloc_frame(paddr: X86HostPhysAddr);

    /// Convert host physical address to host virtual address.
    fn phys_to_virt(paddr: X86HostPhysAddr) -> X86HostVirtAddr;

    /// Convert host virtual address to host physical address.
    fn virt_to_phys(vaddr: X86HostVirtAddr) -> X86HostPhysAddr;

    /// Creates an inactive port for host-side adapters that are not attached
    /// to a guest run. Real vLAPIC and PIT instances receive a run port
    /// explicitly instead of relying on this constructor.
    fn unbound_runtime(vm_id: X86VmId, vcpu_id: X86VcpuId) -> Self::Runtime;

    /// Current monotonic host time in nanoseconds.
    fn current_time_nanos() -> u64;
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
        // SAFETY: the host allocator returned one writable 4-KiB frame and no
        // Rust reference to it exists yet.
        unsafe { core::ptr::write_bytes(frame.as_mut_ptr(), 0, X86_PAGE_SIZE_4K) };
        Ok(frame)
    }

    fn alloc() -> X86VlapicResult<Self> {
        let start_paddr = H::alloc_frame().ok_or(crate::X86VlapicError::NoMemory)?;
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
