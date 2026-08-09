//! Helpers for classic linear directory blocks.

use alloc::vec::Vec;

use super::{DirEntryIterator, Ext4DirEntryInfo};

/// Finds an entry by name in a linear directory block.
pub fn find_entry<'a>(block_data: &'a [u8], target_name: &[u8]) -> Option<Ext4DirEntryInfo<'a>> {
    let iter = DirEntryIterator::new(block_data);
    iter.map(|(entry, _)| entry)
        .find(|entry| entry.name == target_name)
}

/// Returns all valid entries from a linear directory block.
pub fn list_entries<'a>(block_data: &'a [u8]) -> Vec<Ext4DirEntryInfo<'a>> {
    let iter = DirEntryIterator::new(block_data);
    iter.map(|(entry, _)| entry).collect()
}
