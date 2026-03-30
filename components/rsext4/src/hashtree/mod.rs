//! Hash tree helpers for indexed directory lookup.

mod error;
mod facade;
mod inode;
mod lookup;
mod manager;
mod parse;
mod types;

pub use error::HashTreeError;
pub use facade::{create_hash_tree_manager, lookup_directory_entry};
pub use inode::Ext4InodeHashTreeExt;
pub use manager::HashTreeManager;
pub use types::{HashTreeNode, HashTreeSearchResult};

#[cfg(test)]
mod tests;
