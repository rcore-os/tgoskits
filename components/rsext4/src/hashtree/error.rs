//! Hash tree error types.

/// Errors returned by hash tree parsing and lookup helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashTreeError {
    /// The directory does not contain a valid hash tree layout.
    InvalidHashTree,
    /// The on-disk hash version is not supported.
    UnsupportedHashVersion,
    /// The hash tree metadata is corrupted.
    CorruptedHashTree,
    /// A referenced data block is out of range.
    BlockOutOfRange,
    /// The provided buffer is too small to contain the expected structure.
    BufferTooSmall,
    /// The requested entry does not exist.
    EntryNotFound,
}

impl core::fmt::Display for HashTreeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HashTreeError::InvalidHashTree => write!(f, "Invalid hash tree format"),
            HashTreeError::UnsupportedHashVersion => write!(f, "Unsupported hash version"),
            HashTreeError::CorruptedHashTree => write!(f, "Corrupted hash tree"),
            HashTreeError::BlockOutOfRange => write!(f, "Block number out of range"),
            HashTreeError::BufferTooSmall => write!(f, "Buffer too small"),
            HashTreeError::EntryNotFound => write!(f, "Entry not found"),
        }
    }
}
