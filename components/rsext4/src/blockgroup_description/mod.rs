//! Block group descriptor types, tables, and checksum helpers.

mod desc;
mod disk;
mod stats;
mod table;

pub use desc::Ext4GroupDesc;
pub use stats::BlockGroupStats;
pub use table::{BlockGroupDescIter, BlockGroupDescTable, BlockGroupDescTableMut};

#[cfg(test)]
mod tests;
