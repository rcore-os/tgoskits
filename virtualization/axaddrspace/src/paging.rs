use ax_memory_addr::{PageSize, PhysAddr, VirtAddr};
use axvm_types::{GuestPhysAddr, MappingFlags};

use crate::AddrSpaceResult;

/// Common nested page table operations required by the generic address-space
/// manager.
pub trait NestedPageTableOps {
    /// Returns the root physical address programmed into hardware.
    fn root_paddr(&self) -> PhysAddr;

    /// Returns the number of levels used by this table.
    fn levels(&self) -> usize;

    /// Allocates a host frame used by allocation-backed guest memory.
    fn alloc_frame(&self) -> Option<PhysAddr>;

    /// Releases a host frame allocated by [`Self::alloc_frame`].
    fn dealloc_frame(&self, paddr: PhysAddr);

    /// Converts a host physical address to a host virtual address.
    fn phys_to_virt(&self, paddr: PhysAddr) -> VirtAddr;

    /// Maps one page or block.
    fn map(
        &mut self,
        vaddr: GuestPhysAddr,
        paddr: PhysAddr,
        size: PageSize,
        flags: MappingFlags,
    ) -> AddrSpaceResult;

    /// Removes one page or block mapping.
    fn unmap(
        &mut self,
        vaddr: GuestPhysAddr,
    ) -> AddrSpaceResult<(PhysAddr, MappingFlags, PageSize)>;

    /// Maps a range, optionally using huge mappings.
    fn map_region(
        &mut self,
        vaddr: GuestPhysAddr,
        get_paddr: impl Fn(GuestPhysAddr) -> PhysAddr,
        size: usize,
        flags: MappingFlags,
        allow_huge: bool,
    ) -> AddrSpaceResult;

    /// Removes mappings from a range.
    fn unmap_region(&mut self, start: GuestPhysAddr, size: usize) -> AddrSpaceResult;

    /// Replaces the mapping at `start`.
    fn remap(&mut self, start: GuestPhysAddr, paddr: PhysAddr, flags: MappingFlags) -> bool;

    /// Updates protection flags for a range.
    fn protect_region(
        &mut self,
        start: GuestPhysAddr,
        size: usize,
        new_flags: MappingFlags,
    ) -> bool;

    /// Queries a mapped address.
    fn query(&self, vaddr: GuestPhysAddr) -> AddrSpaceResult<(PhysAddr, MappingFlags, PageSize)>;

    /// Translates a guest physical address.
    fn translate(&self, vaddr: GuestPhysAddr) -> Option<PhysAddr> {
        self.query(vaddr).ok().map(|(paddr, ..)| paddr)
    }
}
