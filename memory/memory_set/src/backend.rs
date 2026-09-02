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
    /// The page table type used in the memory area.
    type PageTable;

    /// What to do when mapping a region within the area with the given flags.
    fn map(
        &self,
        start: Self::Addr,
        size: usize,
        flags: Self::Flags,
        page_table: &mut Self::PageTable,
    ) -> bool;

    /// Read-only validation for a mapping operation.  Backends that can
    /// inspect conflicts or allocation requirements should override this
    /// hook.  `MemorySet` invokes it before an overlapping `MAP_FIXED`
    /// operation removes the old mapping, so a rejected request has no
    /// externally visible side effect.
    fn validate_map(
        &self,
        _start: Self::Addr,
        _size: usize,
        _flags: Self::Flags,
        _page_table: &Self::PageTable,
    ) -> bool {
        true
    }

    /// What to do when unmaping a memory region within the area.
    fn unmap(&self, start: Self::Addr, size: usize, page_table: &mut Self::PageTable) -> bool;

    /// Validate an unmap before any page-table state is changed.
    ///
    /// Implementations that can observe structural errors (for example a
    /// multi-level page table) should override this hook and perform a
    /// read-only walk.  `MemorySet` calls it for every affected fragment before
    /// invoking [`Self::unmap`], which gives callers a prepare phase and avoids
    /// the common "first VMA detached, second VMA failed" outcome.  The
    /// default is deliberately permissive for small backends whose `unmap`
    /// operation is intrinsically atomic.
    fn validate_unmap(
        &self,
        _start: Self::Addr,
        _size: usize,
        _page_table: &Self::PageTable,
    ) -> bool {
        true
    }

    /// Validate a protection update before applying it.  This mirrors
    /// [`Self::validate_unmap`] and is optional for legacy backends.
    fn validate_protect(
        &self,
        _start: Self::Addr,
        _size: usize,
        _new_flags: Self::Flags,
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
        page_table: &mut Self::PageTable,
    ) -> bool;

    /// Splits the backend into two backends at the given alignment difference.
    fn split(&mut self, align_diff: usize) -> Option<Self>;

    /// Shrinks the backend from the left by the given size.
    ///
    /// The backend start address is increased by `shrink_size`.
    fn shrink_left(&mut self, _shrink_size: usize) -> bool {
        true
    }

    /// Shrinks the backend from the right by the given size.
    ///
    /// The backend end address is decreased by `shrink_size`.
    fn shrink_right(&mut self, _shrink_size: usize) -> bool {
        true
    }
}
