//! Memory mapping backends.

use ax_hal::paging::{MappingFlags, PageTable};
use ax_memory_addr::VirtAddr;
use ax_memory_set::MappingBackend;

pub(crate) mod alloc;
mod linear;

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
    type PageTable = PageTable;
    fn map(&self, start: VirtAddr, size: usize, flags: MappingFlags, pt: &mut PageTable) -> bool {
        match *self {
            Self::Linear { pa_va_offset } => {
                self.map_linear(start, size, flags, pt, pa_va_offset, false)
            }
            Self::BootLinear { pa_va_offset } => {
                self.map_linear(start, size, flags, pt, pa_va_offset, true)
            }
            Self::Alloc { populate } => self.map_alloc(start, size, flags, pt, populate),
            Self::KernelVirtualAllocation(_) => {
                self.map_kernel_virtual_allocation(start, size, flags, pt)
            }
        }
    }

    fn unmap(&self, start: VirtAddr, size: usize, pt: &mut PageTable) -> bool {
        match *self {
            Self::Linear { pa_va_offset } | Self::BootLinear { pa_va_offset } => {
                self.unmap_linear(start, size, pt, pa_va_offset)
            }
            Self::Alloc { populate } => self.unmap_alloc(start, size, pt, populate),
            Self::KernelVirtualAllocation(_) => {
                self.unmap_kernel_virtual_allocation(start, size, pt)
            }
        }
    }

    fn validate_unmap(&self, start: VirtAddr, size: usize, pt: &PageTable) -> bool {
        match self {
            Self::KernelVirtualAllocation(_) => {
                self.validate_kernel_virtual_allocation(start, size, pt)
            }
            _ => true,
        }
    }

    fn protect(
        &self,
        start: Self::Addr,
        size: usize,
        new_flags: Self::Flags,
        page_table: &mut Self::PageTable,
    ) -> bool {
        page_table.protect_region(start, size, new_flags).is_ok()
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
        page_table: &mut PageTable,
    ) -> bool {
        match *self {
            Self::Linear { .. } | Self::BootLinear { .. } => false,
            Self::Alloc { populate } => {
                self.handle_page_fault_alloc(vaddr, orig_flags, page_table, populate)
            }
            Self::KernelVirtualAllocation(_) => false,
        }
    }

    pub(crate) fn new_kernel_virtual_allocation(
        usage: ax_alloc::UsageKind,
        leading_guard_pages: usize,
        page_count: usize,
    ) -> Option<Self> {
        KernelVirtualAllocationBackend::allocate(usage, leading_guard_pages, page_count)
            .map(Self::KernelVirtualAllocation)
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
