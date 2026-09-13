#[macro_use]
mod macros;

pub(crate) mod context;
pub(crate) mod entry;
mod irq;
pub(crate) mod trap;
pub(crate) mod unaligned;

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
pub(crate) const TLB_RANGE_PAGE_LIMIT: usize = 32;

#[cfg(feature = "virtualization")]
pub(crate) mod virtualization;

pub(crate) mod capability;

mod fp;

// Legacy RISC-V fixups use a section base; the other formats use each field.
#[cfg(feature = "exception-table")]
pub(crate) const EX_TABLE_RELATIVE_TO_START: bool = false;
