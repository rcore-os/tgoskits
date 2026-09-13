//! Inode mappings: allocation, range changes, resize recovery and reads.

use super::*;
use crate::blockdev::TransactionCredits;

mod allocation;
mod legacy_removal;
mod preallocate;
mod ranges;
mod read;
mod removal;
mod resize;
mod shift;
mod transaction;
mod zero;

pub(super) const MAX_RUN_IO_BYTES: usize = 1024 * 1024;

pub use preallocate::{PreallocationOptions, preallocate_inode};
pub use ranges::{
    RangeOperation, ZeroRangeOptions, operate_inode_range, punch_hole_inode, zero_range_inode,
};
pub use read::{read_file, read_inode_data_into};
pub use resize::{InodeResize, truncate, truncate_inode};
pub(crate) use resize::{recover_linked_truncate_inode, truncate_inode_for_reap};
pub use shift::{collapse_range_inode, insert_range_inode};
