use alloc::string::String;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct DiskId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct PartitionId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockRegion {
    pub start_block: u64,
    pub num_blocks: u64,
}

impl BlockRegion {
    pub const fn new(start_block: u64, num_blocks: u64) -> Self {
        Self {
            start_block,
            num_blocks,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.num_blocks == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PartitionTableKind {
    Raw,
    Gpt,
    Mbr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartitionUuid(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartitionLabel(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockVolume {
    pub disk_id: DiskId,
    pub partition_id: PartitionId,
    pub region: BlockRegion,
    pub table_kind: PartitionTableKind,
    pub bootable: bool,
    pub partuuid: Option<PartitionUuid>,
    pub partlabel: Option<PartitionLabel>,
}
