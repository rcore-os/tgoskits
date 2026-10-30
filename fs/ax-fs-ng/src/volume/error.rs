/// Result of volume discovery, validation or partition-table mutation.
pub type Result<T> = core::result::Result<T, Error>;

/// Distinguishes malformed or conflicting metadata from backend I/O failures
/// and unsupported growth layouts so callers can decide whether to skip or stop.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    /// The backend reported a block size unsuitable for the requested access.
    #[error("invalid block size")]
    InvalidBlockSize,
    /// The supplied byte buffer does not match the requested block count.
    #[error("buffer size does not match block count")]
    BufferSizeMismatch,
    /// The requested range exceeds device capacity or address arithmetic.
    #[error("block access is out of range")]
    OutOfRange,
    /// A metadata read failed; another valid copy may still support discovery.
    #[error("block reader failed")]
    Reader,
    /// No usable table exists, or a required metadata/partition invariant failed.
    #[error("invalid partition table")]
    InvalidPartitionTable,
    /// Eligible GPT copies cannot be ordered as identity-preserving tail growth.
    #[error("conflicting partition tables")]
    ConflictingPartitionTables,
    /// The selected partition or disk layout cannot use this growth procedure.
    #[error("partition growth is unsupported")]
    Unsupported,
    /// Memory for a validated metadata read could not be reserved.
    #[error("partition metadata allocation failed")]
    NoMemory,
    /// A metadata write or durability flush failed, possibly after changing disk.
    #[error("partition metadata write or flush failed")]
    Writer,
}
