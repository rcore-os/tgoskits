use alloc::{format, string::String, vec::Vec};

use super::{
    BlockReader, BlockRegion, BlockVolume, DiskId, Error, PartitionId, PartitionLabel,
    PartitionTableKind, PartitionUuid, Result, VolumeScan, mbr,
};

const GPT_HEADER_BLOCK: u64 = 1;
const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";
const MIN_HEADER_SIZE: usize = 92;
const PARTITION_TYPE_GUID_OFFSET: usize = 0;
const UNIQUE_PARTITION_GUID_OFFSET: usize = 16;
const FIRST_LBA_OFFSET: usize = 32;
const LAST_LBA_OFFSET: usize = 40;
const PARTITION_NAME_OFFSET: usize = 56;
const CORE_ENTRY_SIZE: usize = 128;

pub(crate) fn scan_gpt<R: BlockReader>(
    reader: &mut R,
    disk_id: DiskId,
) -> Result<Option<VolumeScan>> {
    let Some(sector0) = mbr::read_sector0(reader)? else {
        return Ok(None);
    };
    if !mbr::has_protective_mbr(&sector0) {
        return Ok(None);
    }

    let block_size = reader.block_size();
    if block_size < 512 || reader.num_blocks() <= GPT_HEADER_BLOCK {
        return Err(Error::InvalidPartitionTable);
    }

    let mut header = alloc::vec![0; block_size];
    reader.read_block(GPT_HEADER_BLOCK, &mut header)?;
    let layout = parse_header(&header, reader.num_blocks())?;
    let entry_bytes = layout
        .entry_count
        .checked_mul(layout.entry_size)
        .ok_or(Error::InvalidPartitionTable)?;
    let entry_blocks = u64::try_from(entry_bytes.div_ceil(block_size))
        .map_err(|_| Error::InvalidPartitionTable)?;
    let primary_entries_end = layout
        .entries_start_lba
        .checked_add(entry_blocks)
        .ok_or(Error::InvalidPartitionTable)?;
    let backup_entries_start = layout
        .backup_lba
        .checked_sub(entry_blocks)
        .ok_or(Error::InvalidPartitionTable)?;
    if primary_entries_end > reader.num_blocks()
        || layout.entries_start_lba < GPT_HEADER_BLOCK + 1
        || primary_entries_end > layout.first_usable_lba
        || layout.backup_lba != reader.num_blocks() - 1
        || backup_entries_start <= layout.last_usable_lba
        || backup_entries_start >= layout.backup_lba
    {
        return Err(Error::InvalidPartitionTable);
    }

    let mut volumes = Vec::new();
    for index in 0..layout.entry_count {
        let entry = read_entry(reader, &layout, index)?;
        if is_unused_entry(&entry) {
            continue;
        }

        let first_lba = le_u64(&entry[FIRST_LBA_OFFSET..FIRST_LBA_OFFSET + 8]);
        let last_lba = le_u64(&entry[LAST_LBA_OFFSET..LAST_LBA_OFFSET + 8]);
        if first_lba < layout.first_usable_lba
            || last_lba > layout.last_usable_lba
            || first_lba > last_lba
        {
            return Err(Error::InvalidPartitionTable);
        }

        volumes.push(BlockVolume {
            disk_id,
            partition_id: PartitionId(index as u32 + 1),
            region: BlockRegion::new(first_lba, last_lba - first_lba + 1),
            table_kind: PartitionTableKind::Gpt,
            bootable: false,
            partuuid: Some(PartitionUuid(format_guid(
                &entry[UNIQUE_PARTITION_GUID_OFFSET..UNIQUE_PARTITION_GUID_OFFSET + 16],
            ))),
            partlabel: decode_partition_name(&entry[PARTITION_NAME_OFFSET..CORE_ENTRY_SIZE])
                .map(PartitionLabel),
        });
    }

    // Everything before the first usable LBA is GPT structure (protective
    // MBR, primary header, primary entries) and everything after the last
    // usable LBA holds the backup entries and backup header; the usable
    // range itself is reserved for partition data.
    let primary_metadata = BlockRegion::new(0, layout.first_usable_lba.max(GPT_HEADER_BLOCK + 1));
    let backup_start = layout.last_usable_lba + 1;
    let backup_metadata = BlockRegion::new(backup_start, reader.num_blocks() - backup_start);

    Ok(Some(VolumeScan {
        volumes,
        table_metadata: Vec::from([primary_metadata, backup_metadata]),
    }))
}

fn read_entry<R: BlockReader>(reader: &mut R, layout: &GptLayout, index: usize) -> Result<Vec<u8>> {
    let block_size = reader.block_size();
    let byte_offset = index
        .checked_mul(layout.entry_size)
        .ok_or(Error::InvalidPartitionTable)?;
    let block_offset = byte_offset / block_size;
    let offset_in_block = byte_offset % block_size;
    let first_block = layout
        .entries_start_lba
        .checked_add(u64::try_from(block_offset).map_err(|_| Error::InvalidPartitionTable)?)
        .ok_or(Error::InvalidPartitionTable)?;
    let blocks_to_read = (offset_in_block + CORE_ENTRY_SIZE).div_ceil(block_size);
    let mut buf = alloc::vec![0; blocks_to_read * block_size];
    reader.read_blocks(first_block, blocks_to_read as u64, &mut buf)?;
    let entry_start = offset_in_block;
    let entry_end = entry_start + CORE_ENTRY_SIZE;

    Ok(buf[entry_start..entry_end].to_vec())
}

#[derive(Clone, Copy)]
struct GptLayout {
    first_usable_lba: u64,
    last_usable_lba: u64,
    backup_lba: u64,
    entries_start_lba: u64,
    entry_count: usize,
    entry_size: usize,
}

fn parse_header(header: &[u8], disk_blocks: u64) -> Result<GptLayout> {
    if header.get(0..8) != Some(GPT_SIGNATURE) {
        return Err(Error::InvalidPartitionTable);
    }

    let header_size = le_u32(&header[12..16]) as usize;
    if !(MIN_HEADER_SIZE..=header.len()).contains(&header_size) {
        return Err(Error::InvalidPartitionTable);
    }

    let current_lba = le_u64(&header[24..32]);
    let backup_lba = le_u64(&header[32..40]);
    let first_usable_lba = le_u64(&header[40..48]);
    let last_usable_lba = le_u64(&header[48..56]);
    let entries_start_lba = le_u64(&header[72..80]);
    let entry_count = le_u32(&header[80..84]) as usize;
    let entry_size = le_u32(&header[84..88]) as usize;

    if current_lba != GPT_HEADER_BLOCK
        || backup_lba >= disk_blocks
        || first_usable_lba > last_usable_lba
        || last_usable_lba >= disk_blocks
        || entries_start_lba >= disk_blocks
        || entry_count == 0
        || entry_size < CORE_ENTRY_SIZE
        || !entry_size.is_multiple_of(8)
    {
        return Err(Error::InvalidPartitionTable);
    }

    Ok(GptLayout {
        first_usable_lba,
        last_usable_lba,
        backup_lba,
        entries_start_lba,
        entry_count,
        entry_size,
    })
}

fn is_unused_entry(entry: &[u8]) -> bool {
    entry[PARTITION_TYPE_GUID_OFFSET..PARTITION_TYPE_GUID_OFFSET + 16]
        .iter()
        .all(|byte| *byte == 0)
}

fn decode_partition_name(bytes: &[u8]) -> Option<String> {
    let mut name = String::new();
    for chunk in bytes.as_chunks::<2>().0 {
        let unit = u16::from_le_bytes(*chunk);
        if unit == 0 {
            break;
        }
        if let Some(ch) = char::from_u32(unit as u32) {
            name.push(ch);
        }
    }
    (!name.is_empty()).then_some(name)
}

fn format_guid(bytes: &[u8]) -> String {
    let data1 = le_u32(&bytes[0..4]);
    let data2 = le_u16(&bytes[4..6]);
    let data3 = le_u16(&bytes[6..8]);
    format!(
        "{data1:08x}-{data2:04x}-{data3:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes.try_into().expect("slice length is fixed"))
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("slice length is fixed"))
}

fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("slice length is fixed"))
}
