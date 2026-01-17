#![cfg_attr(not(test), no_std)]
#![cfg_attr(doc, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate log;

mod arch;
mod bits64;

use core::fmt::Debug;

use memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr};
#[doc(no_inline)]
pub use page_table_entry::{GenericPTE, MappingFlags};

pub use self::{
    arch::*,
    bits64::{PageTable64, PageTable64Cursor},
};

/// The error type for page table operation failures.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum PagingError {
    /// Cannot allocate memory.
    NoMemory,
    /// The address is not aligned to the page size.
    NotAligned,
    /// The mapping is not present.
    NotMapped,
    /// The mapping is already present.
    AlreadyMapped,
    /// The page table entry represents a huge page, but the target physical
    /// frame is 4K in size.
    MappedToHugePage,
}

#[cfg(feature = "axerrno")]
impl From<PagingError> for axerrno::AxError {
    fn from(value: PagingError) -> Self {
        match value {
            PagingError::NoMemory => axerrno::AxError::NoMemory,
            _ => axerrno::AxError::InvalidInput,
        }
    }
}

/// The specialized `Result` type for page table operations.
pub type PagingResult<T = ()> = Result<T, PagingError>;

/// The **architecture-dependent** metadata that must be provided for
/// [`PageTable64`].
pub trait PagingMetaData: Sync + Send {
    /// The number of levels of the hardware page table.
    const LEVELS: usize;
    /// The maximum number of bits of physical address.
    const PA_MAX_BITS: usize;
    /// The maximum number of bits of virtual address.
    const VA_MAX_BITS: usize;

    /// The maximum physical address.
    const PA_MAX_ADDR: usize = (1 << Self::PA_MAX_BITS) - 1;

    /// The virtual address to be translated in this page table.
    ///
    /// This associated type allows more flexible use of page tables structs
    /// like [`PageTable64`], for example, to implement EPTs.
    type VirtAddr: MemoryAddr;
    // (^)it can be converted from/to usize and it's trivially copyable

    /// Whether a given physical address is valid.
    #[inline]
    fn paddr_is_valid(paddr: usize) -> bool {
        paddr <= Self::PA_MAX_ADDR // default
    }

    /// Whether a given virtual address is valid.
    #[inline]
    fn vaddr_is_valid(vaddr: usize) -> bool {
        // default: top bits sign extended
        let top_mask = usize::MAX << (Self::VA_MAX_BITS - 1);
        (vaddr & top_mask) == 0 || (vaddr & top_mask) == top_mask
    }

    /// Flushes the TLB.
    ///
    /// If `vaddr` is [`None`], flushes the entire TLB. Otherwise, flushes the
    /// TLB entry at the given virtual address.
    fn flush_tlb(vaddr: Option<Self::VirtAddr>);
}

/// The low-level **OS-dependent** helpers that must be provided for
/// [`PageTable64`].
pub trait PagingHandler: Sized {
    /// Request to allocate a 4K-sized physical frame.
    fn alloc_frame() -> Option<PhysAddr> {
        Self::alloc_frames(1, PAGE_SIZE_4K)
    }
    /// Allocate `num` contiguous physical frames, with the starting physical
    /// address aligned to `align` bytes (must be a power of two, and must be
    /// a multiple of 4K).
    fn alloc_frames(num: usize, align: usize) -> Option<PhysAddr>;
    /// Request to free a allocated physical frame.
    fn dealloc_frame(paddr: PhysAddr) {
        Self::dealloc_frames(paddr, 1)
    }
    /// Free `num` contiguous physical frames starting from the given physical
    /// address. The `num` must be the same as that used in allocation.
    fn dealloc_frames(paddr: PhysAddr, num: usize);
    /// Returns a virtual address that maps to the given physical address.
    ///
    /// Used to access the physical memory directly in page table
    /// implementation.
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr;
}

/// The page sizes supported by the hardware page table.
#[repr(usize)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PageSize {
    /// Size of 4 kilobytes (2<sup>12</sup> bytes).
    Size4K = 0x1000,
    /// Size of 2 megabytes (2<sup>21</sup> bytes).
    Size2M = 0x20_0000,
    /// Size of 1 gigabytes (2<sup>30</sup> bytes).
    Size1G = 0x4000_0000,
}

impl PageSize {
    /// Whether this page size is considered huge (larger than 4K).
    pub const fn is_huge(self) -> bool {
        matches!(self, Self::Size1G | Self::Size2M)
    }

    /// Checks whether a given address or size is aligned to the page size.
    pub const fn is_aligned(self, addr_or_size: usize) -> bool {
        memory_addr::is_aligned(addr_or_size, self as usize)
    }

    /// Returns the offset of the address within the page size.
    pub const fn align_offset(self, addr: usize) -> usize {
        memory_addr::align_offset(addr, self as usize)
    }
}

impl From<PageSize> for usize {
    #[inline]
    fn from(size: PageSize) -> usize {
        size as usize
    }
}
