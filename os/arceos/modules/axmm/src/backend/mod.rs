//! Memory mapping backends.

use ax_hal::paging::{MappingFlags, PageTable};
use ax_memory_addr::VirtAddr;
use ax_memory_set::MappingBackend;

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
#[derive(Clone)]
pub enum Backend {
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
    },
}

impl MappingBackend for Backend {
    type Addr = VirtAddr;
    type Flags = MappingFlags;
    type PageTable = PageTable;
    fn map(&self, start: VirtAddr, size: usize, flags: MappingFlags, pt: &mut PageTable) -> bool {
        match *self {
            Self::Linear { pa_va_offset } => self.map_linear(start, size, flags, pt, pa_va_offset),
            Self::Alloc { populate } => self.map_alloc(start, size, flags, pt, populate),
        }
    }

    fn unmap(&self, start: VirtAddr, size: usize, pt: &mut PageTable) -> bool {
        match *self {
            Self::Linear { pa_va_offset } => self.unmap_linear(start, size, pt, pa_va_offset),
            Self::Alloc { populate } => self.unmap_alloc(start, size, pt, populate),
        }
    }

    fn protect(
        &self,
        start: Self::Addr,
        size: usize,
        new_flags: Self::Flags,
        page_table: &mut Self::PageTable,
    ) -> bool {
        page_table
            .cursor()
            .protect_region(start, size, new_flags)
            .is_ok()
    }

    fn split(&mut self, _align_diff: usize) -> Option<Self> {
        // backend can be trivially split since it does not have any state.
        Some(self.clone())
    }

    fn shrink_left(&mut self, _shrink_size: usize) {
        // backend can be trivially shrunk since it does not have any state.
    }

    fn shrink_right(&mut self, _shrink_size: usize) {
        // backend can be trivially shrunk since it does not have any state.
    }
}

impl Backend {
    pub(crate) fn handle_page_fault(
        &self,
        vaddr: VirtAddr,
        orig_flags: MappingFlags,
        page_table: &mut PageTable,
    ) -> bool {
        match *self {
            Self::Linear { .. } => false, // Linear mappings should not trigger page faults.
            Self::Alloc { populate } => {
                self.handle_page_fault_alloc(vaddr, orig_flags, page_table, populate)
            }
        }
    }
}
