use alloc::{
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use gpt_disk_io::{
    BlockIo, Disk, DiskError,
    gpt_disk_types::{BlockSize, GptHeader, GptHeaderRevision, GptPartitionEntryArray, Lba},
};
use log::{debug, warn};

use super::{PartitionInfo, PartitionRegion, PartitionTable, PartitionTableKind};
use crate::{BlockDriverOps, DevError, DevResult};

struct BlockDriverAdapter<'a, T: ?Sized>(&'a mut T);

impl<T: BlockDriverOps + ?Sized> BlockIo for BlockDriverAdapter<'_, T> {
    type Error = DevError;

    fn block_size(&self) -> BlockSize {
        BlockSize::from_usize(self.0.block_size()).expect("validated block size")
    }

    fn num_blocks(&mut self) -> Result<u64, Self::Error> {
        Ok(self.0.num_blocks())
    }

    fn read_blocks(&mut self, start_lba: Lba, dst: &mut [u8]) -> Result<(), Self::Error> {
        self.block_size().assert_valid_block_buffer(dst);
        self.0.read_block(start_lba.0, dst)
    }

    fn write_blocks(&mut self, start_lba: Lba, src: &[u8]) -> Result<(), Self::Error> {
        self.block_size().assert_valid_block_buffer(src);
        self.0.write_block(start_lba.0, src)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush()
    }
}

#[derive(Debug)]
enum GptProbeResult {
    Absent,
    Valid(PartitionTable),
}

pub(super) fn scan_gpt_partitions<T: BlockDriverOps + ?Sized>(
    inner: &mut T,
) -> DevResult<Option<PartitionTable>> {
    match probe_gpt(inner)? {
        GptProbeResult::Absent => Ok(None),
        GptProbeResult::Valid(table) => Ok(Some(table)),
    }
}

fn probe_gpt<T: BlockDriverOps + ?Sized>(inner: &mut T) -> DevResult<GptProbeResult> {
    let block_size = BlockSize::from_usize(inner.block_size()).ok_or(DevError::InvalidParam)?;
    let block_size_usize = block_size.to_usize().ok_or(DevError::BadState)?;
    let num_blocks = inner.num_blocks();
    if num_blocks < 2 {
        return Ok(GptProbeResult::Absent);
    }

    let mut block_buf = vec![0u8; block_size_usize];
    let mbr_ok = validate_protective_mbr(inner, &mut block_buf);
    let mut disk = Disk::new(BlockDriverAdapter(inner)).map_err(map_disk_error)?;

    let primary = match try_read_valid_header(&mut disk, Lba(1), &mut block_buf, num_blocks) {
        Ok(header) => header,
        Err(HeaderProbeError::Absent) => None,
        Err(HeaderProbeError::Invalid(err)) => return Err(err),
    };
    let secondary_lba = num_blocks.checked_sub(1).ok_or(DevError::BadState)?;
    let secondary =
        match try_read_valid_header(&mut disk, Lba(secondary_lba), &mut block_buf, num_blocks) {
            Ok(header) => header,
            Err(HeaderProbeError::Absent) => None,
            Err(HeaderProbeError::Invalid(err)) => return Err(err),
        };

    let header = match (primary, secondary) {
        (None, None) => return Ok(GptProbeResult::Absent),
        (Some(primary), Some(secondary)) => {
            validate_header_pair(&primary, &secondary, num_blocks)?;
            primary
        }
        (Some(primary), None) => {
            warn!("secondary GPT header is unavailable or invalid; using primary header only");
            primary
        }
        (None, Some(secondary)) => {
            warn!("primary GPT header is unavailable or invalid; using secondary header only");
            secondary
        }
    };

    if let Err(err) = mbr_ok {
        warn!("protective MBR validation failed: {err}");
    }

    let table = load_partition_table(&mut disk, &header, block_size)?;
    Ok(GptProbeResult::Valid(table))
}

fn validate_protective_mbr<T: BlockDriverOps + ?Sized>(
    inner: &mut T,
    block_buf: &mut [u8],
) -> Result<(), String> {
    inner
        .read_block(0, block_buf)
        .map_err(|err| format!("failed to read protective MBR: {err:?}"))?;

    let mbr_bytes = &block_buf[..512];
    let signature = &mbr_bytes[510..512];
    if signature != [0x55, 0xaa].as_slice() {
        return Err("invalid protective MBR signature".into());
    }

    let partitions = &mbr_bytes[446..510];
    let mut has_protective = false;
    for chunk in partitions.chunks_exact(16) {
        let os_indicator = chunk[4];
        if os_indicator == 0xee {
            has_protective = true;
            break;
        }
    }

    if !has_protective {
        return Err("protective MBR does not contain an 0xEE partition".into());
    }

    Ok(())
}

#[derive(Debug)]
enum HeaderProbeError {
    Absent,
    Invalid(DevError),
}

fn try_read_valid_header<I>(
    disk: &mut Disk<I>,
    lba: Lba,
    block_buf: &mut [u8],
    num_blocks: u64,
) -> Result<Option<GptHeader>, HeaderProbeError>
where
    I: BlockIo<Error = DevError>,
{
    let header = disk
        .read_gpt_header(lba, block_buf)
        .map_err(map_disk_error)
        .map_err(HeaderProbeError::Invalid)?;

    if !header.is_signature_valid() {
        return Err(HeaderProbeError::Absent);
    }

    validate_header(&header, lba.0, num_blocks, block_buf.len())
        .map_err(HeaderProbeError::Invalid)?;
    Ok(Some(header))
}

fn validate_header(
    header: &GptHeader,
    expected_lba: u64,
    num_blocks: u64,
    block_size: usize,
) -> DevResult {
    if header.revision != GptHeaderRevision::VERSION_1_0 {
        return Err(DevError::InvalidParam);
    }

    let header_size = header.header_size.to_u32();
    if header_size < u32::try_from(core::mem::size_of::<GptHeader>()).unwrap()
        || usize::try_from(header_size).map_err(|_| DevError::BadState)? > block_size
    {
        return Err(DevError::InvalidParam);
    }

    if header.reserved.to_u32() != 0 {
        return Err(DevError::InvalidParam);
    }

    if header.my_lba.to_u64() != expected_lba {
        return Err(DevError::InvalidParam);
    }

    let last_block = num_blocks.checked_sub(1).ok_or(DevError::BadState)?;
    let alternate_lba = header.alternate_lba.to_u64();
    if alternate_lba > last_block || alternate_lba == expected_lba {
        return Err(DevError::InvalidParam);
    }

    if header.calculate_header_crc32() != header.header_crc32 {
        return Err(DevError::InvalidParam);
    }

    let layout = header
        .get_partition_entry_array_layout()
        .map_err(|_| DevError::InvalidParam)?;
    let entry_array_bytes = layout
        .num_bytes_rounded_to_block(
            BlockSize::from_usize(block_size).ok_or(DevError::InvalidParam)?,
        )
        .ok_or(DevError::BadState)?;
    let entry_array_blocks = entry_array_bytes
        .checked_div(u64::try_from(block_size).map_err(|_| DevError::BadState)?)
        .ok_or(DevError::BadState)?;
    let entry_array_start = layout.start_lba.to_u64();
    let entry_array_end = entry_array_start
        .checked_add(entry_array_blocks)
        .ok_or(DevError::BadState)?;
    if entry_array_end > num_blocks {
        return Err(DevError::InvalidParam);
    }

    let first_usable = header.first_usable_lba.to_u64();
    let last_usable = header.last_usable_lba.to_u64();
    if first_usable > last_usable || last_usable > last_block {
        return Err(DevError::InvalidParam);
    }

    Ok(())
}

fn validate_header_pair(primary: &GptHeader, secondary: &GptHeader, num_blocks: u64) -> DevResult {
    let last_block = num_blocks.checked_sub(1).ok_or(DevError::BadState)?;
    let primary_disk_guid = primary.disk_guid;
    let secondary_disk_guid = secondary.disk_guid;
    if primary.my_lba.to_u64() != 1
        || primary.alternate_lba.to_u64() != last_block
        || secondary.my_lba.to_u64() != last_block
        || secondary.alternate_lba.to_u64() != 1
    {
        return Err(DevError::InvalidParam);
    }

    if primary.first_usable_lba != secondary.first_usable_lba
        || primary.last_usable_lba != secondary.last_usable_lba
        || primary_disk_guid != secondary_disk_guid
        || primary.number_of_partition_entries != secondary.number_of_partition_entries
        || primary.size_of_partition_entry != secondary.size_of_partition_entry
        || primary.partition_entry_array_crc32 != secondary.partition_entry_array_crc32
    {
        return Err(DevError::InvalidParam);
    }

    Ok(())
}

fn load_partition_table<I>(
    disk: &mut Disk<I>,
    header: &GptHeader,
    block_size: BlockSize,
) -> DevResult<PartitionTable>
where
    I: BlockIo<Error = DevError>,
{
    let layout = header
        .get_partition_entry_array_layout()
        .map_err(|_| DevError::InvalidParam)?;
    let storage_len = layout
        .num_bytes_rounded_to_block_as_usize(block_size)
        .ok_or(DevError::BadState)?;
    let mut storage = vec![0u8; storage_len];
    let entry_array = disk
        .read_gpt_partition_entry_array(layout, &mut storage)
        .map_err(map_disk_error)?;
    validate_partition_array(header, &entry_array)?;

    let mut partitions = Vec::new();
    for index in 0..layout.num_entries {
        let entry = entry_array
            .get_partition_entry(index)
            .ok_or(DevError::BadState)?;
        if !entry.is_used() {
            continue;
        }

        let range = entry.lba_range().ok_or(DevError::InvalidParam)?;
        let region = PartitionRegion {
            start_lba: range.start().to_u64(),
            end_lba: range
                .end()
                .to_u64()
                .checked_add(1)
                .ok_or(DevError::BadState)?,
        };

        if region.start_lba < header.first_usable_lba.to_u64()
            || region.end_lba.checked_sub(1).ok_or(DevError::BadState)?
                > header.last_usable_lba.to_u64()
        {
            return Err(DevError::InvalidParam);
        }

        let name = entry.name.to_string();
        let part_uuid = format_guid_as_partuuid(&entry.unique_partition_guid.to_bytes());
        debug!("validated GPT partition[{index}]: {entry}");
        partitions.push(PartitionInfo {
            index: usize::try_from(index).map_err(|_| DevError::BadState)?,
            table_kind: PartitionTableKind::Gpt,
            region,
            name: if name.is_empty() { None } else { Some(name) },
            part_uuid: Some(part_uuid),
        });
    }

    Ok(PartitionTable {
        kind: PartitionTableKind::Gpt,
        partitions,
    })
}

fn validate_partition_array(
    header: &GptHeader,
    entry_array: &GptPartitionEntryArray<'_>,
) -> DevResult {
    if entry_array.calculate_crc32() != header.partition_entry_array_crc32 {
        return Err(DevError::InvalidParam);
    }

    let mut ranges = Vec::new();
    for index in 0..entry_array.layout().num_entries {
        let entry = entry_array
            .get_partition_entry(index)
            .ok_or(DevError::BadState)?;
        if !entry.is_used() {
            continue;
        }
        let range = entry.lba_range().ok_or(DevError::InvalidParam)?;
        ranges.push((range.start().to_u64(), range.end().to_u64()));
    }

    ranges.sort_unstable_by_key(|range| range.0);
    for pair in ranges.windows(2) {
        let (_, prev_end) = pair[0];
        let (next_start, _) = pair[1];
        if next_start <= prev_end {
            return Err(DevError::InvalidParam);
        }
    }

    Ok(())
}

fn format_guid_as_partuuid(guid: &[u8; 16]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:\
         02X}{:02X}{:02X}",
        guid[3],
        guid[2],
        guid[1],
        guid[0],
        guid[5],
        guid[4],
        guid[7],
        guid[6],
        guid[8],
        guid[9],
        guid[10],
        guid[11],
        guid[12],
        guid[13],
        guid[14],
        guid[15]
    )
}

fn map_disk_error(err: DiskError<DevError>) -> DevError {
    match err {
        DiskError::BufferTooSmall => DevError::InvalidParam,
        DiskError::Overflow => DevError::BadState,
        DiskError::BlockSizeSmallerThanPartitionEntry => DevError::InvalidParam,
        DiskError::Io(err) => err,
    }
}
