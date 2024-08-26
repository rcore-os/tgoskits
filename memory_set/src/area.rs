use core::fmt;

use memory_addr::{AddrRange, MemoryAddr};

use crate::{MappingBackend, MappingError, MappingResult};

/// A memory area represents a continuous range of virtual memory with the same
/// flags.
///
/// The target physical memory frames are determined by [`MappingBackend`] and
/// may not be contiguous.
pub struct MemoryArea<B: MappingBackend> {
    va_range: AddrRange<B::Addr>,
    flags: B::Flags,
    backend: B,
}

impl<B: MappingBackend> MemoryArea<B> {
    /// Creates a new memory area.
    ///
    /// # Panics
    ///
    /// Panics if `start + size` overflows.
    pub fn new(start: B::Addr, size: usize, flags: B::Flags, backend: B) -> Self {
        Self {
            va_range: AddrRange::from_start_size(start, size),
            flags,
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
    /// Changes the flags.
    pub(crate) fn set_flags(&mut self, new_flags: B::Flags) {
        self.flags = new_flags;
    }

    /// Changes the end address of the memory area.
    pub(crate) fn set_end(&mut self, new_end: B::Addr) {
        self.va_range.end = new_end;
    }

    /// Maps the whole memory area in the page table.
    pub(crate) fn map_area(&self, page_table: &mut B::PageTable) -> MappingResult {
        self.backend
            .map(self.start(), self.size(), self.flags, page_table)
            .then_some(())
            .ok_or(MappingError::BadState)
    }

    /// Unmaps the whole memory area in the page table.
    pub(crate) fn unmap_area(&self, page_table: &mut B::PageTable) -> MappingResult {
        self.backend
            .unmap(self.start(), self.size(), page_table)
            .then_some(())
            .ok_or(MappingError::BadState)
    }

    /// Changes the flags in the page table.
    pub(crate) fn protect_area(
        &mut self,
        new_flags: B::Flags,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        self.backend
            .protect(self.start(), self.size(), new_flags, page_table);
        Ok(())
    }

    /// Shrinks the memory area at the left side.
    ///
    /// The start address of the memory area is increased by `new_size`. The
    /// shrunk part is unmapped.
    ///
    /// `new_size` must be greater than 0 and less than the current size.
    pub(crate) fn shrink_left(
        &mut self,
        new_size: usize,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        assert!(new_size > 0 && new_size < self.size());

        let old_size = self.size();
        let unmap_size = old_size - new_size;

        if !self.backend.unmap(self.start(), unmap_size, page_table) {
            return Err(MappingError::BadState);
        }
        // Use wrapping_add to avoid overflow check.
        // Safety: `unmap_size` is less than the current size, so it will never
        // overflow.
        self.va_range.start = self.va_range.start.wrapping_add(unmap_size);
        Ok(())
    }

    /// Shrinks the memory area at the right side.
    ///
    /// The end address of the memory area is decreased by `new_size`. The
    /// shrunk part is unmapped.
    ///
    /// `new_size` must be greater than 0 and less than the current size.
    pub(crate) fn shrink_right(
        &mut self,
        new_size: usize,
        page_table: &mut B::PageTable,
    ) -> MappingResult {
        assert!(new_size > 0 && new_size < self.size());
        let old_size = self.size();
        let unmap_size = old_size - new_size;

        // Use wrapping_add to avoid overflow check.
        // Safety: `new_size` is less than the current size, so it will never overflow.
        let unmap_start = self.start().wrapping_add(new_size);

        if !self.backend.unmap(unmap_start, unmap_size, page_table) {
            return Err(MappingError::BadState);
        }

        // Use wrapping_sub to avoid overflow check, same as above.
        self.va_range.end = self.va_range.end.wrapping_sub(unmap_size);
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
            let new_area = Self::new(
                pos,
                // Use wrapping_sub_addr to avoid overflow check. It is safe because
                // `pos` is within the memory area.
                self.end().wrapping_sub_addr(pos),
                self.flags,
                self.backend.clone(),
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
            .finish()
    }
}
