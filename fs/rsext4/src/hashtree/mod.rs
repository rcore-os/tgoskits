//! Hash tree helpers for indexed directory lookup.

mod error;
mod facade;
mod hash;
mod inode;
mod lookup;
mod manager;
mod mutation;
mod parse;
mod readdir;
mod types;

pub use error::HashTreeError;
pub(crate) use facade::lookup_directory_entry;
pub(crate) use hash::calculate_hash;
pub use inode::Ext4InodeHashTreeExt;
pub use manager::HashTreeManager;
pub(crate) use mutation::{insert_indexed_directory_entry, make_indexed_directory};
pub(crate) use readdir::{
    IndexedDirectoryRange, IndexedDirectoryRecord, read_indexed_directory_range,
};
pub use types::{HashTreeNode, HashTreeSearchResult};

#[cfg(test)]
mod tests;
