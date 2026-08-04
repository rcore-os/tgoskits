//! Block group descriptor types, tables, and checksum helpers.

mod desc;
mod disk;
#[cfg(axtest)]
pub(crate) use self::disk::block_group_desc_disk_format_rules_hold_for_test;
mod stats;
mod table;

pub use desc::Ext4GroupDesc;
pub use stats::BlockGroupStats;
pub use table::{BlockGroupDescIter, BlockGroupDescTable, BlockGroupDescTableMut};

#[cfg(test)]
mod tests;
