#![no_std]

extern crate alloc;

mod error;
mod gpt;
mod mbr;
mod reader;
mod scan;
mod types;

pub use error::{Error, Result};
pub use reader::BlockReader;
pub use scan::scan_volumes;
pub use types::{
    BlockRegion, BlockVolume, DiskId, PartitionId, PartitionLabel, PartitionTableKind,
    PartitionUuid,
};
