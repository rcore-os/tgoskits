use ax_errno::AxError;

/// Result returned by partition-table readers.
pub type Result<T> = core::result::Result<T, Error>;

/// Failure while validating or reading block-volume metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    /// The reader reported an unusable logical block size.
    #[error("invalid block size")]
    InvalidBlockSize,
    /// A caller-provided buffer cannot hold the requested block count.
    #[error("buffer size does not match block count")]
    BufferSizeMismatch,
    /// The requested block range exceeds the reader's published capacity.
    #[error("block access is out of range")]
    OutOfRange,
    /// The block service returned a terminal request error.
    #[error("block reader failed: {0}")]
    Reader(AxError),
    /// On-disk metadata is not a supported partition-table representation.
    #[error("invalid partition table")]
    InvalidPartitionTable,
}
