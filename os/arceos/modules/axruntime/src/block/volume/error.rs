use core::fmt;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidBlockSize,
    BufferSizeMismatch,
    OutOfRange,
    Reader,
    InvalidPartitionTable,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBlockSize => f.write_str("invalid block size"),
            Self::BufferSizeMismatch => f.write_str("buffer size does not match block count"),
            Self::OutOfRange => f.write_str("block access is out of range"),
            Self::Reader => f.write_str("block reader failed"),
            Self::InvalidPartitionTable => f.write_str("invalid partition table"),
        }
    }
}
