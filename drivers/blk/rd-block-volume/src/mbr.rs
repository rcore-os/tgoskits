use alloc::{collections::BTreeSet, format, vec::Vec};

use crate::{
    BlockReader, BlockRegion, BlockVolume, DiskId, Error, PartitionId, PartitionTableKind,
    PartitionUuid, Result,
};

pub(crate) const MBR_SIGNATURE_OFFSET: usize = 510;
pub(crate) const PARTITION_TABLE_OFFSET: usize = 446;
pub(crate) const PARTITION_ENTRY_SIZE: usize = 16;
pub(crate) const PARTITION_ENTRY_COUNT: usize = 4;
pub(crate) const GPT_PROTECTIVE_TYPE: u8 = 0xee;
const DISK_SIGNATURE_OFFSET: usize = 440;
const EXTENDED_TYPES: &[u8] = &[0x05, 0x0f, 0x85];
const FIRST_LOGICAL_PARTITION_ID: u32 = 5;

pub(crate) fn scan_mbr<R: BlockReader>(
    reader: &mut R,
    disk_id: DiskId,
) -> Result<Option<Vec<BlockVolume>>> {
    let Some(mbr) = read_sector0(reader)? else {
        return Ok(None);
    };
    if has_protective_mbr(&mbr) {
        return Ok(None);
    }

    let mut volumes = Vec::new();
    let disk_signature = le_u32(&mbr[DISK_SIGNATURE_OFFSET..DISK_SIGNATURE_OFFSET + 4]);
    let mut extended_root = None;
    for index in 0..PARTITION_ENTRY_COUNT {
        let entry = entry_at(&mbr, index);
        let bootable = entry[0] == 0x80;
        let partition_type = entry[4];
        let start_block = le_u32(&entry[8..12]) as u64;
        let num_blocks = le_u32(&entry[12..16]) as u64;
        if partition_type == 0 || num_blocks == 0 {
            continue;
        }
        if !region_within_disk(reader.num_blocks(), start_block, num_blocks) {
            return Err(Error::InvalidPartitionTable);
        }
        if is_extended_type(partition_type) {
            if extended_root.is_some() {
                return Err(Error::InvalidPartitionTable);
            }
            extended_root = Some(BlockRegion::new(start_block, num_blocks));
            continue;
        }

        volumes.push(BlockVolume {
            disk_id,
            partition_id: PartitionId(index as u32 + 1),
            region: BlockRegion::new(start_block, num_blocks),
            table_kind: PartitionTableKind::Mbr,
            bootable,
            partuuid: mbr_partuuid(disk_signature, index),
            partlabel: None,
        });
    }

    if let Some(extended) = extended_root {
        scan_ebr_chain(reader, disk_id, disk_signature, extended, &mut volumes)?;
    }

    Ok((!volumes.is_empty()).then_some(volumes))
}

pub(crate) fn has_protective_mbr(mbr: &[u8]) -> bool {
    if !has_signature(mbr) {
        return false;
    }

    (0..PARTITION_ENTRY_COUNT).any(|index| {
        let entry = entry_at(mbr, index);
        entry[4] == GPT_PROTECTIVE_TYPE && le_u32(&entry[8..12]) == 1
    })
}

pub(crate) fn read_sector0<R: BlockReader>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    let block_size = reader.block_size();
    if block_size < 512 {
        return Err(Error::InvalidBlockSize);
    }
    let mut block = alloc::vec![0; block_size];
    reader.read_block(0, &mut block)?;
    Ok(has_signature(&block).then_some(block))
}

fn has_signature(block: &[u8]) -> bool {
    block
        .get(MBR_SIGNATURE_OFFSET..MBR_SIGNATURE_OFFSET + 2)
        .is_some_and(|sig| sig == [0x55, 0xaa])
}

fn entry_at(mbr: &[u8], index: usize) -> &[u8] {
    let start = PARTITION_TABLE_OFFSET + index * PARTITION_ENTRY_SIZE;
    &mbr[start..start + PARTITION_ENTRY_SIZE]
}

fn region_within_disk(disk_blocks: u64, start_block: u64, num_blocks: u64) -> bool {
    start_block
        .checked_add(num_blocks)
        .is_some_and(|end| end <= disk_blocks)
}

fn scan_ebr_chain<R: BlockReader>(
    reader: &mut R,
    disk_id: DiskId,
    disk_signature: u32,
    extended: BlockRegion,
    volumes: &mut Vec<BlockVolume>,
) -> Result<()> {
    let mut next_ebr = extended.start_block;
    let mut partition_id = FIRST_LOGICAL_PARTITION_ID;
    let mut visited = BTreeSet::new();

    while next_ebr != 0 {
        if !visited.insert(next_ebr) {
            return Err(Error::InvalidPartitionTable);
        }
        if !region_within_region(extended, next_ebr, 1)
            || !region_within_disk(reader.num_blocks(), next_ebr, 1)
        {
            return Err(Error::InvalidPartitionTable);
        }

        let Some(ebr) = read_sector(reader, next_ebr)? else {
            return Err(Error::InvalidPartitionTable);
        };
        let data_entry = entry_at(&ebr, 0);
        let data_type = data_entry[4];
        let data_start = le_u32(&data_entry[8..12]) as u64;
        let data_blocks = le_u32(&data_entry[12..16]) as u64;
        if data_type != 0 && data_blocks != 0 {
            if is_extended_type(data_type) {
                return Err(Error::InvalidPartitionTable);
            }

            let absolute_start = next_ebr
                .checked_add(data_start)
                .ok_or(Error::InvalidPartitionTable)?;
            if !region_within_region(extended, absolute_start, data_blocks)
                || !region_within_disk(reader.num_blocks(), absolute_start, data_blocks)
            {
                return Err(Error::InvalidPartitionTable);
            }

            volumes.push(BlockVolume {
                disk_id,
                partition_id: PartitionId(partition_id),
                region: BlockRegion::new(absolute_start, data_blocks),
                table_kind: PartitionTableKind::Mbr,
                bootable: data_entry[0] == 0x80,
                partuuid: mbr_partuuid(disk_signature, partition_id as usize - 1),
                partlabel: None,
            });
            partition_id += 1;
        }

        let link_entry = entry_at(&ebr, 1);
        let link_type = link_entry[4];
        let link_start = le_u32(&link_entry[8..12]) as u64;
        let link_blocks = le_u32(&link_entry[12..16]) as u64;
        if link_type == 0 || link_blocks == 0 {
            break;
        }
        if !is_extended_type(link_type) {
            return Err(Error::InvalidPartitionTable);
        }

        let next = extended
            .start_block
            .checked_add(link_start)
            .ok_or(Error::InvalidPartitionTable)?;
        if !region_within_region(extended, next, link_blocks) {
            return Err(Error::InvalidPartitionTable);
        }
        next_ebr = next;
    }

    Ok(())
}

fn read_sector<R: BlockReader>(reader: &mut R, block_id: u64) -> Result<Option<Vec<u8>>> {
    let block_size = reader.block_size();
    if block_size < 512 {
        return Err(Error::InvalidBlockSize);
    }
    let mut block = alloc::vec![0; block_size];
    reader.read_block(block_id, &mut block)?;
    Ok(has_signature(&block).then_some(block))
}

fn is_extended_type(partition_type: u8) -> bool {
    EXTENDED_TYPES.contains(&partition_type)
}

fn region_within_region(region: BlockRegion, start_block: u64, num_blocks: u64) -> bool {
    let Some(end) = start_block.checked_add(num_blocks) else {
        return false;
    };
    let Some(region_end) = region.start_block.checked_add(region.num_blocks) else {
        return false;
    };

    start_block >= region.start_block && end <= region_end
}

fn mbr_partuuid(disk_signature: u32, index: usize) -> Option<PartitionUuid> {
    (disk_signature != 0).then(|| PartitionUuid(format!("{disk_signature:08x}-{:02}", index + 1)))
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("slice length is fixed"))
}
