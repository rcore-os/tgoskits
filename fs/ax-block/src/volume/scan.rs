use alloc::vec::Vec;

use super::{
    BlockReader, BlockRegion, BlockVolume, DiskId, Error, PartitionId, PartitionTableKind, Result,
    gpt, mbr,
};

/// One disk's volume scan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolumeScan {
    /// Every volume found on the disk: identified partitions, or one raw
    /// entry covering the whole disk when no partition table or data
    /// partition exists.
    pub volumes: Vec<BlockVolume>,
    /// Partition-table metadata sectors destructive raw writes must avoid:
    /// the MBR sector and every EBR of the extended chain, or the GPT
    /// header/entry areas at both ends of the disk. Empty without a table.
    pub table_metadata: Vec<BlockRegion>,
}

pub fn scan_volumes<R: BlockReader>(reader: &mut R, disk_id: DiskId) -> Result<VolumeScan> {
    if reader.block_size() == 0 || reader.num_blocks() == 0 {
        return Err(Error::InvalidBlockSize);
    }

    if let Some(scan) = gpt::scan_gpt(reader, disk_id)? {
        return Ok(scan);
    }

    if let Some(mut scan) = mbr::scan_mbr(reader, disk_id)? {
        // Preserve the historical raw-device fallback for an otherwise empty
        // MBR, while retaining the MBR/EBR metadata so destructive callers do
        // not mistake the table for unprotected disk space.
        if scan.volumes.is_empty() {
            scan.volumes.push(BlockVolume {
                disk_id,
                partition_id: PartitionId(0),
                region: BlockRegion::new(0, reader.num_blocks()),
                table_kind: PartitionTableKind::Raw,
                bootable: false,
                partuuid: None,
                partlabel: None,
            });
        }
        return Ok(scan);
    }

    Ok(VolumeScan {
        volumes: Vec::from([BlockVolume {
            disk_id,
            partition_id: PartitionId(0),
            region: BlockRegion::new(0, reader.num_blocks()),
            table_kind: PartitionTableKind::Raw,
            bootable: false,
            partuuid: None,
            partlabel: None,
        }]),
        table_metadata: Vec::new(),
    })
}
