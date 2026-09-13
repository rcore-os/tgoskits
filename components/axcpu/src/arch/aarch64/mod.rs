mod asid;
pub(crate) mod context;

pub mod asm;
pub(crate) mod boot;
#[cfg(feature = "pmu")]
pub(crate) mod pmu;

pub(crate) mod paging;

pub(crate) mod trap;

#[cfg(feature = "uspace")]
pub mod uspace;

pub(crate) mod barrier;
pub(crate) mod cache;
pub(crate) mod capability;
pub(crate) mod interrupt;
pub(crate) mod registers;
pub(crate) mod timer;

// Preserve the native local-invalidation cost threshold.
pub(crate) const TLB_RANGE_PAGE_LIMIT: usize = 32;

#[cfg(feature = "virtualization")]
pub(crate) mod virtualization;

mod fp;

mod entry;

pub(crate) mod mmu;

// Legacy RISC-V fixups use a section base; the other formats use each field.
#[cfg(feature = "exception-table")]
pub(crate) const EX_TABLE_RELATIVE_TO_START: bool = false;
