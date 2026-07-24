use core::fmt::Debug;

pub mod frame;
mod map;
mod table;
mod walk;

pub use frame::Frame;
pub use map::*;
pub use table::*;
pub use walk::*;

pub use crate::common::*;

pub type PagingResult<T = ()> = Result<T, PagingError>;

pub trait TableMeta: Sync + Send + Clone + Copy + 'static {
    type P: PageTableEntry;

    /// Base page size used by this page-table geometry.
    const PAGE_SIZE: usize;

    /// Index width of each level, ordered from root to leaf.
    const LEVEL_BITS: &[usize];

    /// Highest level that may contain a block mapping.
    const MAX_BLOCK_LEVEL: usize;

    /// Whether addresses must fit the address width described by [`LEVEL_BITS`].
    const STRICT_ADDRESS_WIDTH: bool = false;

    /// Invalidates translations affected by a page-table update.
    fn flush(vaddr: Option<VirtAddr>);
}

/// Returns the mapping size represented by `level` for a table geometry.
///
/// Level 1 is the leaf page size. Returns `None` for an invalid level or when
/// the computed shift cannot be represented by `usize`.
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

pub trait PageTableEntry: Debug + Sync + Send + Clone + Copy + Sized + 'static {
    /// Encodes a generic page-table entry configuration.
    fn from_config(config: PteConfig) -> Self;

    /// Decodes the entry, using `is_dir` to select directory-entry semantics.
    fn to_config(&self, is_dir: bool) -> PteConfig;

    /// Returns whether the hardware entry is valid.
    fn valid(&self) -> bool;
}
