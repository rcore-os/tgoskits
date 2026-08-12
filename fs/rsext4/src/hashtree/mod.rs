//! Hash tree helpers for indexed directory lookup.

mod error;
mod facade;
mod hash;
mod inode;
mod lookup;
mod manager;
mod parse;
mod types;

pub use error::HashTreeError;
pub(crate) use facade::lookup_directory_entry;
pub(crate) use hash::calculate_hash;
pub use inode::Ext4InodeHashTreeExt;
pub use manager::HashTreeManager;
pub use types::{HashTreeNode, HashTreeSearchResult};

#[cfg(test)]
mod tests;
