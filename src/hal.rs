use axaddrspace::{HostPhysAddr, HostVirtAddr};

/// The interfaces which the underlying software (kernel or hypervisor) must implement.
pub trait AxVCpuHal {
    /// Allocates a frame and returns its host physical address.
    ///
    /// # Returns
    ///
    /// * `Option<HostPhysAddr>` - Some containing the physical address of the allocated frame, or None if allocation fails.
    fn alloc_frame() -> Option<HostPhysAddr>;

    /// Deallocates a frame given its physical address.
    ///
    /// # Parameters
    ///
    /// * `paddr` - The physical address of the frame to deallocate.
    fn dealloc_frame(paddr: HostPhysAddr);

    /// Converts a host physical address to a host virtual address.
    ///
    /// # Parameters
    ///
    /// * `paddr` - The physical address to convert.
    ///
    /// # Returns
    ///
    /// * `HostVirtAddr` - The corresponding virtual address.
    fn phys_to_virt(paddr: HostPhysAddr) -> HostVirtAddr;

    /// Converts a host virtual address to a host physical address.
    ///
    /// # Parameters
    ///
    /// * `vaddr` - The virtual address to convert.
    ///
    /// # Returns
    ///
    /// * `HostPhysAddr` - The corresponding physical address.
    fn virt_to_phys(vaddr: HostVirtAddr) -> HostPhysAddr;

    /// Fetches current interrupt (IRQ) number.
    ///
    /// # Returns
    ///
    /// * `usize` - The current IRQ number.
    fn irq_fetch() -> usize {
        0
    }

    /// Dispatch an interrupt request (IRQ) to the underlying host OS.
    fn irq_hanlder() {
        unimplemented!("irq_handler is not implemented");
    }
}
