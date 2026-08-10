//! Structured context carried alongside an ext4 domain error.

use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorContext {
    BlockRange { block_id: u32, max_blocks: u64 },
    BlockSize { size: usize, expected: usize },
    BufferSize { provided: usize, required: usize },
    Alignment { offset: u64, alignment: u32 },
    Operation { op: &'static str },
    Feature { set: FeatureSet, bits: u32 },
    Capability { name: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureSet {
    Incompatible,
    ReadOnlyCompatible,
}

impl fmt::Display for ErrorContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorContext::BlockRange {
                block_id,
                max_blocks,
            } => write!(f, "block_id={block_id}, max_blocks={max_blocks}"),
            ErrorContext::BlockSize { size, expected } => {
                write!(f, "size={size}, expected={expected}")
            }
            ErrorContext::BufferSize { provided, required } => {
                write!(f, "provided={provided}, required={required}")
            }
            ErrorContext::Alignment { offset, alignment } => {
                write!(f, "offset={offset}, alignment={alignment}")
            }
            ErrorContext::Operation { op } => write!(f, "op={op}"),
            ErrorContext::Feature { set, bits } => write!(f, "set={set:?}, bits={bits:#x}"),
            ErrorContext::Capability { name } => write!(f, "capability={name}"),
        }
    }
}
