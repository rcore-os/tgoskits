#![cfg_attr(not(test), no_std)]

//! Architecture-neutral page-table traversal and frame-provider contracts.

mod common;
mod frame;
mod map;
mod table;
mod walk;

pub use common::*;
pub use frame::Frame;
pub use map::*;
pub use table::*;
pub use walk::*;

/// Result type returned by page-table operations.
pub type PagingResult<T = ()> = Result<T, PagingError>;

/// Describes the geometry and entry representation of a page table.
pub trait TableMeta: Sync + Send + Clone + Copy + 'static {
    /// Hardware page-table entry type.
    type P: PageTableEntry;

    /// Base page size used by this page-table geometry.
    const PAGE_SIZE: usize;

    /// Index width of each level, ordered from root to leaf.
    const LEVEL_BITS: &[usize];

    /// Highest level that may contain a block mapping.
    const MAX_BLOCK_LEVEL: usize;

    /// Whether addresses must fit the width described by [`Self::LEVEL_BITS`].
    const STRICT_ADDRESS_WIDTH: bool = false;

    /// Invalidates translations affected by a page-table update.
    fn flush(vaddr: Option<VirtAddr>);
}

/// Returns the mapping size represented by `level` for a table geometry.
pub fn level_size<T: TableMeta>(level: usize) -> Option<usize> {
    if level == 0 || level > T::LEVEL_BITS.len() {
        return None;
    }
    let shift = T::LEVEL_BITS
        .iter()
        .skip(T::LEVEL_BITS.len() - level + 1)
        .try_fold(0usize, |sum, &bits| sum.checked_add(bits))?;
    T::PAGE_SIZE.checked_shl(u32::try_from(shift).ok()?)
}

/// Hardware entry operations required by the generic walker.
pub trait PageTableEntry: core::fmt::Debug + Sync + Send + Clone + Copy + Sized + 'static {
    /// Encodes a generic page-table entry configuration.
    fn from_config(config: PteConfig) -> Self;

    /// Decodes this entry using `is_dir` to select directory semantics.
    fn to_config(&self, is_dir: bool) -> PteConfig;

    /// Returns whether the hardware entry is valid.
    fn valid(&self) -> bool;
}
