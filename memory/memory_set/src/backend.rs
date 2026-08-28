use ax_memory_addr::MemoryAddr;

/// Underlying operations to do when manipulating mappings within the specific
/// [`MemoryArea`](crate::MemoryArea).
///
/// The backend can be different for different memory areas. e.g., for linear
/// mappings, the target physical address is known when it is added to the page
/// table. For lazy mappings, an empty mapping needs to be added to the page
/// table to trigger a page fault.
pub trait MappingBackend: Clone {
    /// The address type used in the memory area.
    type Addr: MemoryAddr;
    /// The flags type used in the memory area.
    type Flags: Copy;
    /// Per-mutation state that must be shared by all page-table operations in
    /// one logical mapping transaction.
    type MutationContext;
    /// The page table type used in the memory area.
    type PageTable;

    /// What to do when mapping a region within the area with the given flags.
    fn map(
        &self,
        start: Self::Addr,
        size: usize,
        flags: Self::Flags,
        context: &mut Self::MutationContext,
        page_table: &mut Self::PageTable,
    ) -> bool;

    /// What to do when unmaping a memory region within the area.
    fn unmap(
        &self,
        start: Self::Addr,
        size: usize,
        context: &mut Self::MutationContext,
        page_table: &mut Self::PageTable,
    ) -> bool;

    /// Preflights mapping shape and resource ownership for [`Self::unmap`].
    ///
    /// The page table is not mutated between this preflight and commit. A
    /// backend that can predict rejection from mapping shape or owned
    /// resources must override this method. The commit can still fail because
    /// of concurrent external state or resource pressure; in that case earlier
    /// disjoint subranges may already be unmapped, while `MemorySet` retains
    /// all area metadata and backend owners so the caller can quarantine and
    /// retry the published mutation.
    fn validate_unmap(
        &self,
        _start: Self::Addr,
        _size: usize,
        _page_table: &Self::PageTable,
    ) -> bool {
        true
    }

    /// What to do when changing access flags.
    fn protect(
        &self,
        start: Self::Addr,
        size: usize,
        new_flags: Self::Flags,
        context: &mut Self::MutationContext,
        page_table: &mut Self::PageTable,
    ) -> bool;

    /// Splits the backend into two backends at the given alignment difference.
    fn split(&mut self, align_diff: usize) -> Option<Self>;

    /// Shrinks the backend from the left by the given size.
    ///
    /// The backend start address is increased by `shrink_size`.
    fn shrink_left(&mut self, _shrink_size: usize) {}

    /// Shrinks the backend from the right by the given size.
    ///
    /// The backend end address is decreased by `shrink_size`.
    fn shrink_right(&mut self, _shrink_size: usize) {}
}
