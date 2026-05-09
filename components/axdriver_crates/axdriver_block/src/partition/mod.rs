mod device;
mod gpt;

use alloc::{string::String, vec::Vec};

pub use self::device::PartitionBlockDevice;
use crate::{BlockDriverOps, DevResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PartitionTableKind {
    Gpt,
    Mbr,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PartitionRegion {
    pub start_lba: u64,
    pub end_lba: u64,
}

impl PartitionRegion {
    pub const fn from_num_blocks(num_blocks: u64) -> Self {
        Self {
            start_lba: 0,
            end_lba: num_blocks,
        }
    }

    pub const fn num_blocks(self) -> u64 {
        self.end_lba.saturating_sub(self.start_lba)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartitionInfo {
    pub index: usize,
    pub table_kind: PartitionTableKind,
    pub region: PartitionRegion,
    pub name: Option<String>,
    pub part_uuid: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartitionTable {
    pub kind: PartitionTableKind,
    pub partitions: Vec<PartitionInfo>,
}

impl PartitionTable {
    pub const fn empty() -> Self {
        Self {
            kind: PartitionTableKind::None,
            partitions: Vec::new(),
        }
    }
}

pub fn scan_partitions<T: BlockDriverOps + ?Sized>(inner: &mut T) -> DevResult<PartitionTable> {
    if let Some(table) = gpt::scan_gpt_partitions(inner)? {
        return Ok(table);
    }

    Ok(PartitionTable::empty())
}
