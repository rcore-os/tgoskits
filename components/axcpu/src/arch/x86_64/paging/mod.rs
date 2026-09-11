//! Native and nested x86 paging formats.

mod ept;
mod native;

pub use ept::{EptEntry, EptFlags, EptMemoryType};
pub use native::{DescriptorFlags, Pte};
pub(crate) use native::{LEVEL_BITS, MAX_BLOCK_LEVEL, PAGE_SIZE};

/// AMD nested paging uses the native long-mode descriptor bit layout.
/// Its root, ASID ownership and invalidation belong to SVM, not the host CR3.
pub type NptEntry = Pte;
