//! Block and inode bitmap allocation helpers.

use core::fmt;

use crate::{
    bitmap::*,
    blockgroup_description::*,
    error::{Errno, Ext4Error, Ext4Result},
    superblock::*,
};

mod block;
mod error;
mod inode;

pub use block::{BlockAlloc, BlockAllocator};
pub(crate) use error::map_bitmap_error;
pub use inode::{InodeAlloc, InodeAllocator};

fn overflow_error() -> Ext4Error {
    Ext4Error::from(Errno::EOVERFLOW)
}

/// Zero-based block-group index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BGIndex(u32);

impl BGIndex {
    /// Creates a new block-group index.
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying raw value.
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Converts this index into `usize`.
    pub fn as_usize(self) -> Ext4Result<usize> {
        usize::try_from(self.0).map_err(|_| overflow_error())
    }

    /// Converts a group-local block index into an absolute physical block number.
    pub fn absolute_block(
        self,
        block_in_group: RelativeBN,
        first_data_block: u32,
        blocks_per_group: u32,
    ) -> AbsoluteBN {
        AbsoluteBN(
            u64::from(self.0) * u64::from(blocks_per_group)
                + u64::from(block_in_group.raw())
                + u64::from(first_data_block),
        )
    }

    /// Converts a group-local inode index into a global inode number.
    pub fn inode_number(
        self,
        inode_in_group: RelativeInodeIndex,
        inodes_per_group: u32,
    ) -> Ext4Result<InodeNumber> {
        let raw =
            u64::from(self.0) * u64::from(inodes_per_group) + u64::from(inode_in_group.raw()) + 1;
        InodeNumber::from_u64(raw)
    }
}

impl fmt::Display for BGIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Absolute physical block number in the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AbsoluteBN(u64);

impl AbsoluteBN {
    /// Creates a new absolute block number.
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the underlying raw value.
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Converts this block number into `usize`.
    pub fn as_usize(self) -> Ext4Result<usize> {
        usize::try_from(self.0).map_err(|_| overflow_error())
    }

    /// Converts this block number into `u32`, failing on overflow.
    pub fn to_u32(self) -> Ext4Result<u32> {
        u32::try_from(self.0).map_err(|_| overflow_error())
    }

    /// Returns a block number offset by `delta` blocks.
    pub fn checked_add(self, delta: u32) -> Ext4Result<Self> {
        self.0
            .checked_add(u64::from(delta))
            .map(Self)
            .ok_or_else(overflow_error)
    }

    /// Returns a block number offset by `delta` blocks.
    pub fn checked_add_usize(self, delta: usize) -> Ext4Result<Self> {
        let delta = u64::try_from(delta).map_err(|_| overflow_error())?;
        self.0
            .checked_add(delta)
            .map(Self)
            .ok_or_else(overflow_error)
    }

    /// Converts an absolute block number into `(group, block-in-group)`.
    pub fn to_group(
        self,
        first_data_block: u32,
        blocks_per_group: u32,
    ) -> Ext4Result<(BGIndex, RelativeBN)> {
        if blocks_per_group == 0 || self.0 < u64::from(first_data_block) {
            return Err(Ext4Error::invalid_input());
        }

        let rel = self.0 - u64::from(first_data_block);
        let group_idx =
            u32::try_from(rel / u64::from(blocks_per_group)).map_err(|_| overflow_error())?;
        let block_in_group =
            u32::try_from(rel % u64::from(blocks_per_group)).map_err(|_| overflow_error())?;
        Ok((BGIndex(group_idx), RelativeBN(block_in_group)))
    }
}

impl From<u32> for AbsoluteBN {
    fn from(value: u32) -> Self {
        Self(u64::from(value))
    }
}

impl fmt::Display for AbsoluteBN {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Zero-based block index inside one block group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelativeBN(u32);

impl RelativeBN {
    /// Creates a new block-in-group index.
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying raw value.
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Converts this index into `usize`.
    pub fn as_usize(self) -> Ext4Result<usize> {
        usize::try_from(self.0).map_err(|_| overflow_error())
    }
}

impl fmt::Display for RelativeBN {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// One-based global inode number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InodeNumber(u32);

impl InodeNumber {
    /// Creates a validated inode number.
    pub fn new(raw: u32) -> Ext4Result<Self> {
        if raw == 0 {
            return Err(Ext4Error::invalid_input());
        }
        Ok(Self(raw))
    }

    /// Creates a validated inode number from `u64`.
    pub fn from_u64(raw: u64) -> Ext4Result<Self> {
        let raw = u32::try_from(raw).map_err(|_| overflow_error())?;
        Self::new(raw)
    }

    /// Returns the underlying raw value.
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Converts this inode number into `u64`.
    pub const fn as_u64(self) -> u64 {
        self.0 as u64
    }

    /// Converts this inode number into `usize`.
    pub fn as_usize(self) -> Ext4Result<usize> {
        usize::try_from(self.0).map_err(|_| overflow_error())
    }

    /// Converts a global inode number into `(group, inode-in-group)`.
    pub fn to_group(self, inodes_per_group: u32) -> Ext4Result<(BGIndex, RelativeInodeIndex)> {
        if inodes_per_group == 0 {
            return Err(Ext4Error::invalid_input());
        }

        let inode_idx = self.0 - 1;
        Ok((
            BGIndex(inode_idx / inodes_per_group),
            RelativeInodeIndex(inode_idx % inodes_per_group),
        ))
    }
}

impl fmt::Display for InodeNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Zero-based inode index inside one block group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelativeInodeIndex(u32);

impl RelativeInodeIndex {
    /// Creates a new inode-in-group index.
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying raw value.
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Converts this index into `usize`.
    pub fn as_usize(self) -> Ext4Result<usize> {
        usize::try_from(self.0).map_err(|_| overflow_error())
    }
}

impl fmt::Display for RelativeInodeIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inode_number_rejects_zero() {
        let err = InodeNumber::new(0).unwrap_err();
        assert_eq!(err.code, Errno::EINVAL);
    }

    #[test]
    fn block_group_and_absolute_block_round_trip() {
        let group = BGIndex::new(3);
        let block_in_group = RelativeBN::new(17);
        let absolute = group.absolute_block(block_in_group, 1, 8192);
        let (decoded_group, decoded_block) = absolute.to_group(1, 8192).unwrap();

        assert_eq!(decoded_group, group);
        assert_eq!(decoded_block, block_in_group);
    }

    #[test]
    fn inode_group_round_trip() {
        let group = BGIndex::new(5);
        let inode_in_group = RelativeInodeIndex::new(123);
        let inode = group.inode_number(inode_in_group, 2048).unwrap();
        let (decoded_group, decoded_inode) = inode.to_group(2048).unwrap();

        assert_eq!(decoded_group, group);
        assert_eq!(decoded_inode, inode_in_group);
    }

    #[test]
    fn absolute_block_to_u32_checks_overflow() {
        let err = AbsoluteBN::new(u64::from(u32::MAX) + 1)
            .to_u32()
            .unwrap_err();
        assert_eq!(err.code, Errno::EOVERFLOW);
    }
}
