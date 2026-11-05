//! MBR growth bounded by neighboring data, table sectors and extended containers.

use alloc::vec::Vec;

use super::{
    BlockReader, BlockRegion, BlockWriter, DiskId, Error, PartitionGrowth, PartitionId, Result, mbr,
};

/// One validated size-field update. Boot code, identity and other entries stay intact.
struct Growth {
    /// Physical sector containing the selected primary or logical entry.
    sector: u64,
    /// Byte offset of that entry's sector-count field.
    size_offset: usize,
    /// Original complete logical sector, including bytes beyond the MBR trailer.
    bytes: Vec<u8>,
    /// Validated old region and largest non-overlapping new region.
    bounds: PartitionGrowth,
}

impl Growth {
    /// Validates the full EBR chain before locating the selected entry's size field.
    fn read<R: BlockReader>(reader: &mut R, partition: PartitionId) -> Result<Self> {
        let scan = mbr::scan_mbr(reader, DiskId(0))?.ok_or(Error::InvalidPartitionTable)?;
        let source = scan
            .volumes
            .iter()
            .find(|volume| volume.partition_id == partition)
            .ok_or(Error::InvalidPartitionTable)?;
        let start = source.region.start_block;
        let end = start
            .checked_add(source.region.num_blocks)
            .ok_or(Error::OutOfRange)?;
        // Discovery tolerates several legacy MBR quirks. Mutation must additionally
        // prove that no data region already overlaps another volume or an EBR.
        for (index, volume) in scan.volumes.iter().enumerate() {
            let first = volume.region.start_block;
            let last = first
                .checked_add(volume.region.num_blocks)
                .ok_or(Error::OutOfRange)?;
            if scan.volumes[index + 1..].iter().any(|other| {
                first < other.region.start_block + other.region.num_blocks
                    && other.region.start_block < last
            }) || scan.table_metadata.iter().any(|table| {
                first < table.start_block + table.num_blocks && table.start_block < last
            }) {
                return Err(Error::InvalidPartitionTable);
            }
        }
        let primary = mbr::read_sector0(reader)?.ok_or(Error::InvalidPartitionTable)?;
        let mut limit = reader.num_blocks().min(u64::from(u32::MAX) + 1).min(
            start
                .checked_add(u64::from(u32::MAX))
                .ok_or(Error::OutOfRange)?,
        );
        for volume in &scan.volumes {
            if volume.region.start_block >= end {
                limit = limit.min(volume.region.start_block);
            }
        }
        for table in &scan.table_metadata {
            if table.start_block >= end {
                limit = limit.min(table.start_block);
            }
        }
        for entry in primary[mbr::PARTITION_TABLE_OFFSET..mbr::MBR_SIGNATURE_OFFSET]
            .as_chunks::<{ mbr::PARTITION_ENTRY_SIZE }>()
            .0
        {
            if mbr::is_extended_type(entry[4]) {
                let first = u64::from(le_u32(&entry[8..12]));
                let last = first + u64::from(le_u32(&entry[12..16]));
                if partition.0 >= 5 && first <= start && end <= last {
                    limit = limit.min(last);
                } else if first >= end {
                    limit = limit.min(first);
                }
            }
        }
        if limit < end {
            return Err(Error::InvalidPartitionTable);
        }
        let bounds = PartitionGrowth {
            current: source.region,
            target: BlockRegion::new(start, limit - start),
        };
        if (1..=4).contains(&partition.0) {
            return Ok(Self {
                sector: 0,
                size_offset: mbr::PARTITION_TABLE_OFFSET
                    + (partition.0 as usize - 1) * mbr::PARTITION_ENTRY_SIZE
                    + 12,
                bytes: primary,
                bounds,
            });
        }
        for table in &scan.table_metadata {
            if table.start_block == 0 {
                continue;
            }
            let mut bytes = alloc::vec![0; reader.block_size()];
            reader.read_block(table.start_block, &mut bytes)?;
            let entry = &bytes[mbr::PARTITION_TABLE_OFFSET..mbr::PARTITION_TABLE_OFFSET + 16];
            if table.start_block + u64::from(le_u32(&entry[8..12])) == start
                && u64::from(le_u32(&entry[12..16])) == source.region.num_blocks
            {
                return Ok(Self {
                    sector: table.start_block,
                    size_offset: mbr::PARTITION_TABLE_OFFSET + 12,
                    bytes,
                    bounds,
                });
            }
        }
        Err(Error::InvalidPartitionTable)
    }
}

/// Plans one primary or logical data partition without writing table metadata.
pub(super) fn partition_growth<R: BlockReader>(
    reader: &mut R,
    partition: PartitionId,
) -> Result<PartitionGrowth> {
    Ok(Growth::read(reader, partition)?.bounds)
}

/// Updates only the sector-count field and verifies the resulting valid layout.
/// MBR has no redundant table; a failed write must be reported and revalidated.
pub(super) fn grow_partition<W: BlockWriter>(
    writer: &mut W,
    partition: PartitionId,
) -> Result<BlockRegion> {
    let mut growth = Growth::read(writer, partition)?;
    if growth.bounds.current == growth.bounds.target {
        return Ok(growth.bounds.current);
    }
    let size = u32::try_from(growth.bounds.target.num_blocks).map_err(|_| Error::OutOfRange)?;
    growth.bytes[growth.size_offset..growth.size_offset + 4].copy_from_slice(&size.to_le_bytes());
    writer.write_block(growth.sector, &growth.bytes)?;
    writer.flush()?;
    let mut verified = alloc::vec![0; writer.block_size()];
    writer.read_block(growth.sector, &mut verified)?;
    if verified != growth.bytes
        || Growth::read(writer, partition)?.bounds.current != growth.bounds.target
    {
        return Err(Error::InvalidPartitionTable);
    }
    Ok(growth.bounds.target)
}

/// Decodes a validated four-byte little-endian MBR field.
fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("fixed-width MBR field"))
}
