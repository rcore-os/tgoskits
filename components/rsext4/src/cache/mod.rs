//! Shared cache managers for bitmaps, inode tables, and data blocks.

pub mod bitmap;
pub mod data_block;
pub mod inode_table;

pub use bitmap::{BitmapCache, BitmapType};
pub use data_block::{DataBlockCache, DataBlockCacheStats};
pub use inode_table::{InodeCache, InodeCacheStats, InodeHandle};
