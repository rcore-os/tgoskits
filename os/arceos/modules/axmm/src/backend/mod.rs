//! Memory mapping backends.

use ax_hal::paging::{MappingFlags, PageTable, PagingError};
use ax_memory_addr::{PageIter4K, VirtAddr};
use ax_memory_set::MappingBackend;

use crate::tlb::TlbGather;
pub(crate) mod alloc;
mod linear;

pub(crate) use alloc::dealloc_frame;
pub use alloc::{KernelVirtualAllocationBackend, KernelVirtualAllocationId};

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
/// - **Kernel virtual allocation**: reserves one virtual interval, optionally
///   leaves leading guard pages unmapped, and backs the usable part with
///   individually allocated frames. Its explicit
///   Live -> Retiring -> Quarantined state keeps frame ownership attached to
///   the mapping until a TLB acknowledgement.
#[derive(Clone)]
pub(crate) enum Backend {
    /// Linear mapping backend.
    ///
    /// The signed delta from physical to virtual addresses is constant. The
    /// physical address for `vaddr` is `vaddr - pa_to_va_delta`.
    Linear {
        /// `vaddr as i128 - paddr as i128`.
        pa_to_va_delta: i128,
    },
    /// Immutable linear mapping backend for the boot-time kernel direct map.
    BootLinear {
        /// `vaddr as i128 - paddr as i128`.
        pa_to_va_delta: i128,
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
    /// Virtually contiguous kernel allocation with non-contiguous frames.
    KernelVirtualAllocation(KernelVirtualAllocationBackend),
}

/// Whether a kernel virtual allocation may still be used by its owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KernelVirtualAllocationState {
    Live,
    Retiring,
    Quarantined,
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
            Self::Linear { pa_to_va_delta } => {
                self.map_linear(start, size, flags, pt, pa_to_va_delta, false)
            }
            Self::BootLinear { pa_to_va_delta } => {
                self.map_linear(start, size, flags, pt, pa_to_va_delta, true)
            }
            Self::Alloc { populate } => self.map_alloc(start, size, flags, context, pt, populate),
            // Virtual allocations reserve metadata first and install prepared
            // page-table deposits outside the generic map path.
            Self::KernelVirtualAllocation(_) => false,
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
            Self::Linear { pa_to_va_delta } | Self::BootLinear { pa_to_va_delta } => {
                self.unmap_linear(start, size, context, pt, pa_to_va_delta)
            }
            Self::Alloc { populate } => self.unmap_alloc(start, size, context, pt, populate),
            Self::KernelVirtualAllocation(_) => {
                self.unmap_kernel_virtual_allocation(start, size, pt)
            }
        }
    }

    fn validate_unmap(&self, start: VirtAddr, size: usize, pt: &PageTable) -> bool {
        match self {
            Self::Linear { .. } | Self::BootLinear { .. } => {
                self.validate_linear_unmap(start, size, pt)
            }
            Self::Alloc { .. } => {
                for addr in PageIter4K::new(start, start + size).unwrap() {
                    match pt.query_occupied(addr) {
                        Ok((_, 1)) | Err(PagingError::NotMapped) => {}
                        Ok(_) | Err(_) => return false,
                    }
                }
                true
            }
            Self::KernelVirtualAllocation(_) => {
                self.validate_kernel_virtual_allocation(start, size, pt)
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
        match self {
            Self::KernelVirtualAllocation(_) => None,
            // These backends do not carry range-relative ownership.
            _ => Some(self.clone()),
        }
    }

    fn shrink_left(&mut self, _shrink_size: usize) -> bool {
        !matches!(self, Self::KernelVirtualAllocation(_))
    }

    fn shrink_right(&mut self, _shrink_size: usize) -> bool {
        !matches!(self, Self::KernelVirtualAllocation(_))
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
            Self::KernelVirtualAllocation(_) => false,
        }
    }

    pub(crate) const fn kernel_virtual_allocation(
        &self,
    ) -> Option<&KernelVirtualAllocationBackend> {
        match self {
            Self::KernelVirtualAllocation(allocation) => Some(allocation),
            _ => None,
        }
    }
}
