//! Hash tree error types.

use crate::error::Ext4Error;

/// Errors returned by hash tree parsing and lookup helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HashTreeError {
    /// The directory does not contain a valid hash tree layout.
    #[error("Invalid hash tree format")]
    InvalidHashTree,
    /// The on-disk hash version is not supported.
    #[error("Unsupported hash version")]
    UnsupportedHashVersion,
    /// The hash tree metadata is corrupted.
    #[error("Corrupted hash tree")]
    CorruptedHashTree,
    /// A referenced data block is out of range.
    #[error("Block number out of range")]
    BlockOutOfRange,
    /// The provided buffer is too small to contain the expected structure.
    #[error("Buffer too small")]
    BufferTooSmall,
    /// The requested entry does not exist.
    #[error("Entry not found")]
    EntryNotFound,
    /// An extent, cache, or block-device operation failed during lookup.
    #[error(transparent)]
    Filesystem(#[from] Ext4Error),
}

impl HashTreeError {
    /// Returns whether Linux would treat this as `ERR_BAD_DX_DIR` and retry
    /// through the ordinary directory scan.
    pub(crate) const fn allows_linear_fallback(&self) -> bool {
        matches!(
            self,
            Self::InvalidHashTree
                | Self::UnsupportedHashVersion
                | Self::CorruptedHashTree
                | Self::BlockOutOfRange
                | Self::BufferTooSmall
        )
    }

    /// Converts an HTree boundary error into the filesystem domain.
    pub(crate) fn into_ext4(self, operation: &'static str) -> Ext4Error {
        match self {
            Self::Filesystem(error) => error,
            Self::UnsupportedHashVersion => Ext4Error::unsupported().with_operation(operation),
            Self::EntryNotFound => Ext4Error::not_found().with_operation(operation),
            Self::InvalidHashTree
            | Self::CorruptedHashTree
            | Self::BlockOutOfRange
            | Self::BufferTooSmall => Ext4Error::corrupted().with_operation(operation),
        }
    }
}
