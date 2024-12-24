use axaddrspace::{HostPhysAddr, HostVirtAddr};

/// The interfaces which the underlying software (kernel or hypervisor) must implement.
pub trait AxVMHal: Sized {
    /// The low-level **OS-dependent** helpers that must be provided for physical address management.
    type PagingHandler: page_table_multiarch::PagingHandler;

    /// Allocates a memory region at the specified physical address.
    ///
    /// Returns `true` if the memory region is successfully allocated.
    fn alloc_memory_region_at(base: HostPhysAddr, size: usize) -> bool;

    /// Deallocates a memory region at the specified physical address.
    fn dealloc_memory_region_at(base: HostPhysAddr, size: usize);

    /// Converts a virtual address to the corresponding physical address.
    fn virt_to_phys(vaddr: HostVirtAddr) -> HostPhysAddr;

    /// Current time in nanoseconds.
    fn current_time_nanos() -> u64;
}
