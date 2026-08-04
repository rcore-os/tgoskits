mod error;
mod gpt;
mod mbr;
mod reader;
mod scan;
#[cfg(test)]
mod tests;
mod types;

pub use error::{Error, Result};
pub use reader::BlockReader;
pub use scan::scan_volumes;
pub use types::{
    BlockRegion, BlockVolume, DiskId, PartitionId, PartitionLabel, PartitionTableKind,
    PartitionUuid,
};
