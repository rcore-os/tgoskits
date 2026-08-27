//! Memory mapping backends.

use ax_hal::paging::{MappingFlags, PageTable, PagingError};
use ax_memory_addr::{PAGE_SIZE_4K, PageIter4K, VirtAddr};
use ax_memory_set::MappingBackend;

use crate::tlb::TlbGather;

mod alloc;
mod linear;

pub(crate) use alloc::dealloc_frame;

/// A unified enum type for different memory mapping backends.
///
/// Currently, two backends are implemented:
///
/// - **Linear**: used for linear mappings. The target physical frames are
///   contiguous and their addresses should be known when creating the mapping.
/// - **BootLinear**: used only for immutable boot-time kernel direct mappings,
///   which may use huge pages and must not be partially unmapped.
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
    /// Immutable linear mapping backend for the boot-time kernel direct map.
    BootLinear {
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
    type MutationContext = TlbGather;
    type PageTable = PageTable;
    fn map(
        &self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        context: &mut TlbGather,
        pt: &mut PageTable,
    ) -> bool {
        match *self {
            Self::Linear { pa_va_offset } => {
                self.map_linear(start, size, flags, pt, pa_va_offset, false)
            }
            Self::BootLinear { pa_va_offset } => {
                self.map_linear(start, size, flags, pt, pa_va_offset, true)
            }
            Self::Alloc { populate } => self.map_alloc(start, size, flags, context, pt, populate),
        }
    }

    fn unmap(
        &self,
        start: VirtAddr,
        size: usize,
        context: &mut TlbGather,
        pt: &mut PageTable,
    ) -> bool {
        match *self {
            Self::Linear { pa_va_offset } | Self::BootLinear { pa_va_offset } => {
                self.unmap_linear(start, size, context, pt, pa_va_offset)
            }
            Self::Alloc { populate } => self.unmap_alloc(start, size, context, pt, populate),
        }
    }

    fn validate_unmap(&self, start: VirtAddr, size: usize, pt: &PageTable) -> bool {
        match self {
            Self::Linear { .. } | Self::BootLinear { .. } => {
                self.validate_linear_unmap(start, size, pt)
            }
            Self::Alloc { .. } => {
                for addr in PageIter4K::new(start, start + size).unwrap() {
                    match pt.query(addr) {
                        Ok((_, _, PAGE_SIZE_4K)) | Err(PagingError::NotMapped) => {}
                        Ok(_) | Err(_) => return false,
                    }
                }
                true
            }
        }
    }

    fn protect(
        &self,
        start: Self::Addr,
        size: usize,
        new_flags: Self::Flags,
        context: &mut TlbGather,
        page_table: &mut Self::PageTable,
    ) -> bool {
        if page_table.protect_region(start, size, new_flags).is_err() {
            return false;
        }
        context.invalidate(start, size);
        true
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
        gather: &mut TlbGather,
        page_table: &mut PageTable,
    ) -> bool {
        match *self {
            Self::Linear { .. } | Self::BootLinear { .. } => false,
            Self::Alloc { populate } => {
                self.handle_page_fault_alloc(vaddr, orig_flags, gather, page_table, populate)
            }
        }
    }
}
