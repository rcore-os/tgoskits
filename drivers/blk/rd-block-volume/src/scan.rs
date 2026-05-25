use alloc::vec::Vec;

use crate::{
    BlockReader, BlockRegion, BlockVolume, DiskId, Error, PartitionId, PartitionTableKind, Result,
    gpt, mbr,
};

pub fn scan_volumes<R: BlockReader>(reader: &mut R, disk_id: DiskId) -> Result<Vec<BlockVolume>> {
    if reader.block_size() == 0 || reader.num_blocks() == 0 {
        return Err(Error::InvalidBlockSize);
    }

    if let Some(volumes) = gpt::scan_gpt(reader, disk_id)? {
        return Ok(volumes);
    }

    if let Some(volumes) = mbr::scan_mbr(reader, disk_id)? {
        return Ok(volumes);
    }

    Ok(Vec::from([BlockVolume {
        disk_id,
        partition_id: PartitionId(0),
        region: BlockRegion::new(0, reader.num_blocks()),
        table_kind: PartitionTableKind::Raw,
        bootable: false,
        partuuid: None,
        partlabel: None,
    }]))
}
