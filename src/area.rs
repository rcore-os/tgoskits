use core::fmt;
use core::marker::PhantomData;

use memory_addr::{VirtAddr, VirtAddrRange};

use crate::{MappingError, MappingResult};

/// Underlying operations to do when manipulating mappings within the specific
/// [`MemoryArea`].
///
/// The backend can be different for different memory areas. e.g., for linear
/// mappings, the target physical address is known when it is added to the page
/// table. For lazy mappings, an empty mapping needs to be added to the page table
/// to trigger a page fault.
pub trait MappingBackend<F: Copy, P>: Clone {
    /// What to do when mapping a region within the area with the given flags.
    fn map(&self, start: VirtAddr, size: usize, flags: F, page_table: &mut P) -> bool;
    /// What to do when unmaping a memory region within the area.
    fn unmap(&self, start: VirtAddr, size: usize, page_table: &mut P) -> bool;
    /// What to do when changing access flags.
    fn protect(&self, start: VirtAddr, size: usize, new_flags: F, page_table: &mut P) -> bool;
}

/// A memory area represents a continuous range of virtual memory with the same
/// flags.
///
/// The target physical memory frames are determined by [`MappingBackend`] and
/// may not be contiguous.
pub struct MemoryArea<F: Copy, P, B: MappingBackend<F, P>> {
    va_range: VirtAddrRange,
    flags: F,
    backend: B,
    _phantom: PhantomData<(F, P)>,
}

impl<F: Copy, P, B: MappingBackend<F, P>> MemoryArea<F, P, B> {
    /// Creates a new memory area.
    pub const fn new(start: VirtAddr, size: usize, flags: F, backend: B) -> Self {
        Self {
            va_range: VirtAddrRange::from_start_size(start, size),
            flags,
            backend,
            _phantom: PhantomData,
        }
    }

    /// Returns the virtual address range.
    pub const fn va_range(&self) -> VirtAddrRange {
        self.va_range
    }

    /// Returns the memory flags, e.g., the permission bits.
    pub const fn flags(&self) -> F {
        self.flags
    }

    /// Returns the start address of the memory area.
    pub const fn start(&self) -> VirtAddr {
        self.va_range.start
    }

    /// Returns the end address of the memory area.
    pub const fn end(&self) -> VirtAddr {
        self.va_range.end
    }

    /// Returns the size of the memory area.
    pub const fn size(&self) -> usize {
        self.va_range.size()
    }

    /// Returns the mapping backend of the memory area.
    pub const fn backend(&self) -> &B {
        &self.backend
    }
}

impl<F: Copy, P, B: MappingBackend<F, P>> MemoryArea<F, P, B> {
    /// Changes the flags.
    pub(crate) fn set_flags(&mut self, new_flags: F) {
        self.flags = new_flags;
    }

    /// Changes the end address of the memory area.
    pub(crate) fn set_end(&mut self, new_end: VirtAddr) {
        self.va_range.end = new_end;
    }

    /// Maps the whole memory area in the page table.
    pub(crate) fn map_area(&self, page_table: &mut P) -> MappingResult {
        self.backend
            .map(self.start(), self.size(), self.flags, page_table)
            .then_some(())
            .ok_or(MappingError::BadState)
    }

    /// Unmaps the whole memory area in the page table.
    pub(crate) fn unmap_area(&self, page_table: &mut P) -> MappingResult {
        self.backend
            .unmap(self.start(), self.size(), page_table)
            .then_some(())
            .ok_or(MappingError::BadState)
    }

    /// Changes the flags in the page table.
    pub(crate) fn protect_area(&mut self, new_flags: F, page_table: &mut P) -> MappingResult {
        self.backend
            .protect(self.start(), self.size(), new_flags, page_table);
        Ok(())
    }

    /// Shrinks the memory area at the left side.
    ///
    /// The start address of the memory area is increased by `new_size`. The
    /// shrunk part is unmapped.
    pub(crate) fn shrink_left(&mut self, new_size: usize, page_table: &mut P) -> MappingResult {
        let unmap_size = self.size() - new_size;
        if !self.backend.unmap(self.start(), unmap_size, page_table) {
            return Err(MappingError::BadState);
        }
        self.va_range.start += unmap_size;
        Ok(())
    }

    /// Shrinks the memory area at the right side.
    ///
    /// The end address of the memory area is decreased by `new_size`. The
    /// shrunk part is unmapped.
    pub(crate) fn shrink_right(&mut self, new_size: usize, page_table: &mut P) -> MappingResult {
        let unmap_size = self.size() - new_size;
        if !self
            .backend
            .unmap(self.start() + new_size, unmap_size, page_table)
        {
            return Err(MappingError::BadState);
        }
        self.va_range.end -= unmap_size;
        Ok(())
    }

    /// Splits the memory area at the given position.
    ///
    /// The original memory area is shrunk to the left part, and the right part
    /// is returned.
    ///
    /// Returns `None` if the given position is not in the memory area, or one
    /// of the parts is empty after splitting.
    pub(crate) fn split(&mut self, pos: VirtAddr) -> Option<Self> {
        let start = self.start();
        let end = self.end();
        if start < pos && pos < end {
            let new_area = Self::new(
                pos,
                end.as_usize() - pos.as_usize(),
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

impl<F, P, B: MappingBackend<F, P>> fmt::Debug for MemoryArea<F, P, B>
where
    F: fmt::Debug + Copy,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("MemoryArea")
            .field("va_range", &self.va_range)
            .field("flags", &self.flags)
            .finish()
    }
}
