//! Memory mapping backends.

use memory_set::MappingBackend;
use page_table_multiarch::{MappingFlags, PagingHandler};

use crate::{npt::NestedPageTable as PageTable, GuestPhysAddr};

mod alloc;
mod linear;

/// A unified enum type for different memory mapping backends.
///
/// Currently, two backends are implemented:
///
/// - **Linear**: used for linear mappings. The target physical frames are
///   contiguous and their addresses should be known when creating the mapping.
/// - **Allocation**: used in general, or for lazy mappings. The target physical
///   frames are obtained from the global allocator.
pub enum Backend<H: PagingHandler> {
    /// Linear mapping backend.
    ///
    /// The offset between the virtual address and the physical address is
    /// constant, which is specified by `pa_va_offset`. For example, the virtual
    /// address `vaddr` is mapped to the physical address `vaddr - pa_va_offset`.
    Linear {
        /// `vaddr - paddr`.
        pa_va_offset: usize,
    },
    /// Allocation mapping backend.
    ///
    /// If `populate` is `true`, all physical frames are allocated when the
    /// mapping is created, and no page faults are triggered during the memory
    /// access. Otherwise, the physical frames are allocated on demand (by
    /// handling page faults).
    Alloc {
        /// Whether to populate the physical frames when creating the mapping.
        populate: bool,
        /// A phantom data for the paging handler.
        _phantom: core::marker::PhantomData<H>,
    },
}

impl<H: PagingHandler> Clone for Backend<H> {
    fn clone(&self) -> Self {
        match self {
            &Self::Linear { pa_va_offset } => Self::Linear { pa_va_offset },
            &Self::Alloc { populate, .. } => Self::Alloc {
                populate: populate,
                _phantom: core::marker::PhantomData,
            },
        }
    }
}

impl<H: PagingHandler> MappingBackend for Backend<H> {
    type Addr = GuestPhysAddr;
    type Flags = MappingFlags;
    type PageTable = PageTable<H>;

    fn map(
        &self,
        start: GuestPhysAddr,
        size: usize,
        flags: MappingFlags,
        pt: &mut PageTable<H>,
    ) -> bool {
        match *self {
            Self::Linear { pa_va_offset } => self.map_linear(start, size, flags, pt, pa_va_offset),
            Self::Alloc { populate, .. } => self.map_alloc(start, size, flags, pt, populate),
        }
    }

    fn unmap(&self, start: GuestPhysAddr, size: usize, pt: &mut PageTable<H>) -> bool {
        match *self {
            Self::Linear { pa_va_offset } => self.unmap_linear(start, size, pt, pa_va_offset),
            Self::Alloc { populate, .. } => self.unmap_alloc(start, size, pt, populate),
        }
    }

    fn protect(
        &self,
        _start: GuestPhysAddr,
        _size: usize,
        _new_flags: MappingFlags,
        _page_table: &mut PageTable<H>,
    ) -> bool {
        // a stub here
        true
    }
}

impl<H: PagingHandler> Backend<H> {
    pub(crate) fn handle_page_fault(
        &self,
        vaddr: GuestPhysAddr,
        orig_flags: MappingFlags,
        page_table: &mut PageTable<H>,
    ) -> bool {
        match *self {
            Self::Linear { .. } => false, // Linear mappings should not trigger page faults.
            Self::Alloc { populate, .. } => {
                self.handle_page_fault_alloc(vaddr, orig_flags, page_table, populate)
            }
        }
    }
}
