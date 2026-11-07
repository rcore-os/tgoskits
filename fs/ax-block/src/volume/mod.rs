mod device;
mod error;
mod gpt;
mod grow;
mod mbr;
mod mbr_growth;
mod reader;
mod scan;
#[cfg(test)]
mod tests;
mod types;

pub use device::DeviceReader;
pub use error::{Error, Result};
pub use grow::{PartitionGrowth, grow_partition, partition_growth};
pub use reader::{BlockReader, BlockWriter};
pub use scan::{VolumeScan, scan_volumes};
pub use types::{
    BlockRegion, BlockVolume, DiskId, PartitionId, PartitionLabel, PartitionTableKind,
    PartitionUuid,
};
