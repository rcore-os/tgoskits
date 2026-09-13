#[macro_use]
mod macros;

pub(crate) mod context;
mod local_state;
pub(crate) mod trap;

pub mod asm;
pub(crate) mod boot;

pub(crate) mod paging;

#[cfg(feature = "uspace")]
pub mod uspace;

pub(crate) mod barrier;
pub(crate) mod cache;
pub(crate) mod interrupt;
pub(crate) mod registers;

pub(crate) mod timer;

// Preserve the native local-invalidation cost threshold.
pub(crate) const TLB_RANGE_PAGE_LIMIT: usize = 64;

pub(crate) mod capability;

#[cfg(feature = "virtualization")]
pub(crate) mod virtualization;

mod entry;

// Legacy RISC-V fixups use a section base; the other formats use each field.
#[cfg(feature = "exception-table")]
pub(crate) const EX_TABLE_RELATIVE_TO_START: bool = true;

pub(crate) mod mmu;
