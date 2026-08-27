use core::fmt;

use ax_memory_addr::{AddrRange, MemoryAddr};

use crate::{MappingBackend, MappingError, MappingResult};

/// A memory area represents a continuous range of virtual memory with the same
/// flags.
///
/// The target physical memory frames are determined by [`MappingBackend`] and
/// may not be contiguous.
pub struct MemoryArea<B: MappingBackend> {
    va_range: AddrRange<B::Addr>,
    flags: B::Flags,
    reported_flags: B::Flags,
    backend: B,
}

impl<B: MappingBackend> MemoryArea<B> {
    /// Creates a new memory area.
    ///
    /// # Panics
    ///
    /// Panics if `start + size` overflows.
    pub fn new(start: B::Addr, size: usize, flags: B::Flags, backend: B) -> Self {
        Self::new_with_reported_flags(start, size, flags, flags, backend)
    }

    /// Creates a new memory area with separate backend and reported flags.
    ///
    /// `flags` are used for page-table/backend operations. `reported_flags`
    /// are metadata exposed through introspection interfaces such as procfs.
    ///
    /// # Panics
    ///
    /// Panics if `start + size` overflows.
    pub fn new_with_reported_flags(
        start: B::Addr,
        size: usize,
        flags: B::Flags,
        reported_flags: B::Flags,
        backend: B,
    ) -> Self {
        Self {
            va_range: AddrRange::from_start_size(start, size),
            flags,
            reported_flags,
            backend,
        }
    }

    /// Returns the virtual address range.
    pub const fn va_range(&self) -> AddrRange<B::Addr> {
        self.va_range
    }

    /// Returns the memory flags, e.g., the permission bits.
    pub const fn flags(&self) -> B::Flags {
        self.flags
    }

    /// Returns the permission flags reported to user-visible introspection.
    pub const fn reported_flags(&self) -> B::Flags {
        self.reported_flags
    }

    /// Returns the start address of the memory area.
    pub const fn start(&self) -> B::Addr {
        self.va_range.start
    }

    /// Returns the end address of the memory area.
    pub const fn end(&self) -> B::Addr {
        self.va_range.end
    }

    /// Returns the size of the memory area.
    pub fn size(&self) -> usize {
        self.va_range.size()
    }

    /// Returns the mapping backend of the memory area.
    pub const fn backend(&self) -> &B {
        &self.backend
    }
}

impl<B: MappingBackend> MemoryArea<B> {
    /// Changes backend/page-table flags and reported flags together.
    pub(crate) fn set_flags_with_reported_flags(
        &mut self,
        new_flags: B::Flags,
        new_reported_flags: B::Flags,
    ) {
        self.flags = new_flags;
        self.reported_flags = new_reported_flags;
    }

    /// Maps the whole memory area in the page table.
    pub(crate) fn map_area(
        &self,
        context: &mut B::MutationContext,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        self.backend
            .map(self.start(), self.size(), self.flags, context, page_table)
            .then_some(())
            .ok_or(MappingError::BadState)
    }

    /// Unmaps the whole memory area in the page table.
    pub(crate) fn unmap_area(
        &self,
        context: &mut B::MutationContext,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        self.unmap_range(self.start(), self.size(), context, page_table)
    }

    /// Unmaps a sub-range without changing this area's metadata.
    ///
    /// Callers use this to complete the fallible backend transition before
    /// committing a split or key change in the containing memory set.
    pub(crate) fn unmap_range(
        &self,
        start: B::Addr,
        size: usize,
        context: &mut B::MutationContext,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        debug_assert!(
            self.va_range
                .contains_range(AddrRange::from_start_size(start, size))
        );
        self.backend
            .unmap(start, size, context, page_table)
            .then_some(())
            .ok_or(MappingError::BadState)
    }

    /// Preflights an unmap sub-range without changing page-table or metadata.
    pub(crate) fn validate_unmap_range(
        &self,
        start: B::Addr,
        size: usize,
        page_table: &B::PageTable,
    ) -> MappingResult {
        debug_assert!(
            self.va_range
                .contains_range(AddrRange::from_start_size(start, size))
        );
        self.backend
            .validate_unmap(start, size, page_table)
            .then_some(())
            .ok_or(MappingError::BadState)
    }

    /// Changes page-table flags for a sub-range without changing metadata.
    pub(crate) fn protect_range(
        &self,
        start: B::Addr,
        size: usize,
        new_flags: B::Flags,
        context: &mut B::MutationContext,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        debug_assert!(
            self.va_range
                .contains_range(AddrRange::from_start_size(start, size))
        );
        self.backend
            .protect(start, size, new_flags, context, page_table)
            .then_some(())
            .ok_or(MappingError::BadState)
    }

    /// Shrinks the memory area at the left side without touching the page
    /// table.
    pub(crate) fn shrink_left_metadata(&mut self, new_size: usize) {
        assert!(new_size > 0 && new_size < self.size());

        let old_size = self.size();
        let unmap_size = old_size - new_size;
        self.va_range.start = self.va_range.start.wrapping_add(unmap_size);
        self.backend.shrink_left(unmap_size);
    }

    /// Shrinks the memory area at the right side without touching the page
    /// table.
    pub(crate) fn shrink_right_metadata(&mut self, new_size: usize) {
        assert!(new_size > 0 && new_size < self.size());
        let old_size = self.size();
        let unmap_size = old_size - new_size;

        self.va_range.end = self.va_range.end.wrapping_sub(unmap_size);
        self.backend.shrink_right(unmap_size);
    }

    /// Inverse of [`shrink_right`]: extends the end by `additional_size`
    /// and maps the new region via the backend.
    pub(crate) fn grow_right(
        &mut self,
        additional_size: usize,
        context: &mut B::MutationContext,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        assert!(additional_size > 0);
        assert!(
            self.end().is_aligned_4k()
                && additional_size.is_multiple_of(ax_memory_addr::PAGE_SIZE_4K),
            "grow_right: end and additional_size must be page-aligned"
        );
        let map_start = self.end();
        let new_end = self
            .va_range
            .end
            .checked_add(additional_size)
            .ok_or(MappingError::InvalidParam)?;
        if !self
            .backend
            .map(map_start, additional_size, self.flags, context, page_table)
        {
            return Err(MappingError::BadState);
        }
        self.va_range.end = new_end;
        Ok(())
    }

    /// Splits the memory area at the given position.
    ///
    /// The original memory area is shrunk to the left part, and the right part
    /// is returned.
    ///
    /// Returns `None` if the given position is not in the memory area, or one
    /// of the parts is empty after splitting.
    pub(crate) fn split(&mut self, pos: B::Addr) -> Option<Self> {
        if self.start() < pos && pos < self.end() {
            let align_diff = pos.sub_addr(self.start());

            let right = self
                .backend
                .split(align_diff)
                .expect("backend should be splittable");

            let new_area = Self::new_with_reported_flags(
                pos,
                // Use wrapping_sub_addr to avoid overflow check. It is safe because
                // `pos` is within the memory area.
                self.end().wrapping_sub_addr(pos),
                self.flags,
                self.reported_flags,
                right,
            );
            self.va_range.end = pos;
            Some(new_area)
        } else {
            None
        }
    }
}

impl<B: MappingBackend> fmt::Debug for MemoryArea<B>
where
    B::Addr: fmt::Debug,
    B::Flags: fmt::Debug + Copy,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("MemoryArea")
            .field("va_range", &self.va_range)
            .field("flags", &self.flags)
            .field("reported_flags", &self.reported_flags)
            .finish()
    }
}
