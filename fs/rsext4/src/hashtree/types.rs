//! Core hash tree types.

use alloc::vec::Vec;

use crate::{
    bmalloc::AbsoluteBN,
    entries::{Ext4DirEntryInfo, Ext4DxEntry},
};

/// Result returned by hash tree lookups.
#[derive(Debug)]
pub struct HashTreeSearchResult {
    /// Matched directory entry.
    pub entry: Ext4DirEntryInfo<'static>,
    /// Physical block that contains the entry.
    pub block_num: AbsoluteBN,
    /// Offset inside the containing block.
    pub offset: usize,
}

/// Parsed hash tree node variants.
#[derive(Debug)]
pub enum HashTreeNode {
    /// Root node with root-specific metadata.
    Root {
        hash_version: u8,
        indirect_levels: u8,
        entries: Vec<Ext4DxEntry>,
    },
    /// Internal index node.
    Internal {
        entries: Vec<Ext4DxEntry>,
        level: u32,
    },
    /// Leaf node carrying directory entries.
    Leaf {
        block_num: AbsoluteBN,
        entries: Vec<Ext4DirEntryInfo<'static>>,
    },
}
