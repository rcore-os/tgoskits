extern crate alloc;

use core::{fmt, ops::Range, str::FromStr};

use alloc::vec::Vec;
use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use log::{debug, info};

use crate::BlockDriverOps;

const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";
const GPT_HEADER_SIZE: usize = 92;
const GPT_PARTITION_ENTRY_SIZE: usize = 128;
const GPT_PARTITION_NAME_SIZE: usize = 72;
const MAX_BLOCK_SIZE: usize = 4096;

#[derive(Clone, Copy, Debug)]
struct GptHeader {
    current_lba: u64,
    backup_lba: u64,
    first_usable_lba: u64,
    last_usable_lba: u64,
    partition_entry_lba: u64,
    number_of_partition_entries: u32,
    size_of_partition_entry: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GptPartitionName([u8; GPT_PARTITION_NAME_SIZE]);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GptPartitionNameFromStrError {
    Length,
    InvalidChar,
}

impl fmt::Display for GptPartitionNameFromStrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length => f.write_str("input string is too long"),
            Self::InvalidChar => {
                f.write_str("input string contains a character that cannot be represented in UCS-2")
            }
        }
    }
}

impl core::error::Error for GptPartitionNameFromStrError {}

impl Default for GptPartitionName {
    fn default() -> Self {
        Self([0; GPT_PARTITION_NAME_SIZE])
    }
}

impl fmt::Display for GptPartitionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for bytes in self.0.chunks_exact(2) {
            let code_unit = u16::from_le_bytes([bytes[0], bytes[1]]);
            if code_unit == 0 {
                break;
            }
            let ch = char::from_u32(u32::from(code_unit)).unwrap_or('?');
            write!(f, "{ch}")?;
        }
        Ok(())
    }
}

impl FromStr for GptPartitionName {
    type Err = GptPartitionNameFromStrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut name = Self::default();
        let mut next_slot = 0usize;

        for ch in s.chars() {
            if next_slot >= GPT_PARTITION_NAME_SIZE {
                return Err(GptPartitionNameFromStrError::Length);
            }

            let code_unit = u16::try_from(u32::from(ch))
                .map_err(|_| GptPartitionNameFromStrError::InvalidChar)?;
            let encoded = code_unit.to_le_bytes();
            name.0[next_slot] = encoded[0];
            name.0[next_slot + 1] = encoded[1];
            next_slot += 2;
        }

        Ok(name)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GptPartitionEntry {
    pub partition_type_guid: [u8; 16],
    pub unique_partition_guid: [u8; 16],
    pub starting_lba: u64,
    pub ending_lba: u64,
    pub attributes: u64,
    pub name: GptPartitionName,
}

impl GptPartitionEntry {
    fn is_used(&self) -> bool {
        self.partition_type_guid.iter().any(|&byte| byte != 0)
    }
}

impl fmt::Display for GptPartitionEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "name='{}', lba {}..={}, attrs={:#x}",
            self.name, self.starting_lba, self.ending_lba, self.attributes
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GptPartition {
    pub entry: GptPartitionEntry,
    pub range: Range<u64>,
}

fn block_size<T: BlockDriverOps>(inner: &T) -> DevResult<usize> {
    let block_size = inner.block_size();
    if !(512..=MAX_BLOCK_SIZE).contains(&block_size) {
        return Err(DevError::InvalidParam);
    }
    Ok(block_size)
}

fn checked_add(lhs: u64, rhs: u64) -> DevResult<u64> {
    lhs.checked_add(rhs).ok_or(DevError::BadState)
}

fn checked_mul(lhs: u64, rhs: u64) -> DevResult<u64> {
    lhs.checked_mul(rhs).ok_or(DevError::BadState)
}

fn read_le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_le_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn read_bytes<T: BlockDriverOps>(
    inner: &mut T,
    mut offset: u64,
    mut dst: &mut [u8],
    block_buf: &mut [u8; MAX_BLOCK_SIZE],
) -> DevResult {
    let block_size = block_size(inner)?;
    let block_size_u64 = u64::try_from(block_size).map_err(|_| DevError::BadState)?;

    while !dst.is_empty() {
        let block_id = offset / block_size_u64;
        let within_block =
            usize::try_from(offset % block_size_u64).map_err(|_| DevError::BadState)?;

        inner.read_block(block_id, &mut block_buf[..block_size])?;

        let copy_len = dst.len().min(block_size - within_block);
        dst[..copy_len].copy_from_slice(&block_buf[within_block..within_block + copy_len]);

        let (_, rest) = dst.split_at_mut(copy_len);
        dst = rest;
        offset = checked_add(
            offset,
            u64::try_from(copy_len).map_err(|_| DevError::BadState)?,
        )?;
    }

    Ok(())
}

fn read_gpt_header<T: BlockDriverOps>(
    inner: &mut T,
    lba: u64,
    block_buf: &mut [u8; MAX_BLOCK_SIZE],
) -> DevResult<GptHeader> {
    let block_size = block_size(inner)?;
    inner.read_block(lba, &mut block_buf[..block_size])?;

    let header = &block_buf[..block_size];
    if &header[..GPT_SIGNATURE.len()] != GPT_SIGNATURE {
        return Err(DevError::InvalidParam);
    }

    let header_size = usize::try_from(read_le_u32(header, 12)).map_err(|_| DevError::BadState)?;
    if !(GPT_HEADER_SIZE..=block_size).contains(&header_size) {
        return Err(DevError::InvalidParam);
    }

    Ok(GptHeader {
        current_lba: read_le_u64(header, 24),
        backup_lba: read_le_u64(header, 32),
        first_usable_lba: read_le_u64(header, 40),
        last_usable_lba: read_le_u64(header, 48),
        partition_entry_lba: read_le_u64(header, 72),
        number_of_partition_entries: read_le_u32(header, 80),
        size_of_partition_entry: read_le_u32(header, 84),
    })
}

fn parse_partition_entry(raw: &[u8; GPT_PARTITION_ENTRY_SIZE]) -> GptPartitionEntry {
    let mut partition_type_guid = [0u8; 16];
    partition_type_guid.copy_from_slice(&raw[0..16]);

    let mut unique_partition_guid = [0u8; 16];
    unique_partition_guid.copy_from_slice(&raw[16..32]);

    let mut name = [0u8; GPT_PARTITION_NAME_SIZE];
    name.copy_from_slice(&raw[56..128]);

    GptPartitionEntry {
        partition_type_guid,
        unique_partition_guid,
        starting_lba: read_le_u64(raw, 32),
        ending_lba: read_le_u64(raw, 40),
        attributes: read_le_u64(raw, 48),
        name: GptPartitionName(name),
    }
}

pub fn is_gpt_disk<T: BlockDriverOps>(inner: &mut T) -> DevResult<bool> {
    let mut block_buf = [0u8; MAX_BLOCK_SIZE];
    match read_gpt_header(inner, 1, &mut block_buf) {
        Ok(_) => Ok(true),
        Err(DevError::InvalidParam) => Ok(false),
        Err(err) => Err(err),
    }
}

pub fn find_partition_range<T, F>(inner: &mut T, mut predicate: F) -> DevResult<Option<Range<u64>>>
where
    T: BlockDriverOps,
    F: FnMut(usize, &GptPartitionEntry) -> bool,
{
    let partitions = list_partitions(inner)?;
    for (index, partition) in partitions.iter().enumerate() {
        if predicate(index, &partition.entry) {
            info!("Selected GPT partition: {}", partition.entry);
            return Ok(Some(partition.range.clone()));
        }
    }

    Ok(None)
}

pub fn list_partitions<T: BlockDriverOps>(inner: &mut T) -> DevResult<Vec<GptPartition>> {
    let block_size = block_size(inner)?;
    let block_size_u64 = u64::try_from(block_size).map_err(|_| DevError::BadState)?;
    let num_blocks = inner.num_blocks();
    let last_block = num_blocks.checked_sub(1).ok_or(DevError::BadState)?;

    let mut block_buf = [0u8; MAX_BLOCK_SIZE];
    let primary_header = read_gpt_header(inner, 1, &mut block_buf)?;
    debug!("Primary GPT header: {:?}", primary_header);

    if primary_header.current_lba != 1
        || primary_header.backup_lba != last_block
        || primary_header.number_of_partition_entries == 0
    {
        return Err(DevError::InvalidParam);
    }

    if primary_header.size_of_partition_entry < u32::try_from(GPT_PARTITION_ENTRY_SIZE).unwrap() {
        return Err(DevError::InvalidParam);
    }

    if usize::try_from(primary_header.size_of_partition_entry).map_err(|_| DevError::BadState)?
        > MAX_BLOCK_SIZE
    {
        return Err(DevError::InvalidParam);
    }

    let secondary_header = read_gpt_header(inner, last_block, &mut block_buf)?;
    debug!("Secondary GPT header: {:?}", secondary_header);

    if secondary_header.current_lba != last_block || secondary_header.backup_lba != 1 {
        return Err(DevError::InvalidParam);
    }

    let entry_size = u64::from(primary_header.size_of_partition_entry);
    let entry_array_offset = checked_mul(primary_header.partition_entry_lba, block_size_u64)?;
    let entry_count = usize::try_from(primary_header.number_of_partition_entries)
        .map_err(|_| DevError::BadState)?;
    let mut partitions = Vec::new();

    for index in 0..entry_count {
        let index_u64 = u64::try_from(index).map_err(|_| DevError::BadState)?;
        let entry_offset = checked_add(entry_array_offset, checked_mul(index_u64, entry_size)?)?;

        let mut raw = [0u8; GPT_PARTITION_ENTRY_SIZE];
        read_bytes(inner, entry_offset, &mut raw, &mut block_buf)?;

        let part = parse_partition_entry(&raw);
        if !part.is_used() {
            continue;
        }

        if part.starting_lba < primary_header.first_usable_lba
            || part.ending_lba > primary_header.last_usable_lba
            || part.starting_lba > part.ending_lba
        {
            continue;
        }

        debug!("GPT partition[{index}]: {part}");
        let range_end = checked_add(part.ending_lba, 1)?;
        partitions.push(GptPartition {
            entry: part,
            range: part.starting_lba..range_end,
        });
    }

    Ok(partitions)
}

pub struct GptPartitionDev<T> {
    inner: T,
    range: Range<u64>,
}

impl<T: BlockDriverOps> GptPartitionDev<T> {
    pub fn new(inner: T, range: Range<u64>) -> Self {
        Self { inner, range }
    }
}

impl<T: BlockDriverOps> BaseDriverOps for GptPartitionDev<T> {
    fn device_name(&self) -> &str {
        self.inner.device_name()
    }

    fn device_type(&self) -> DeviceType {
        self.inner.device_type()
    }

    fn irq_num(&self) -> Option<usize> {
        self.inner.irq_num()
    }
}

impl<T: BlockDriverOps> BlockDriverOps for GptPartitionDev<T> {
    fn num_blocks(&self) -> u64 {
        self.range.end.saturating_sub(self.range.start)
    }

    fn block_size(&self) -> usize {
        self.inner.block_size()
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> DevResult {
        if block_id >= self.num_blocks() {
            return Err(DevError::InvalidParam);
        }

        let end_block = checked_add(
            block_id,
            u64::try_from(buf.len().div_ceil(self.block_size())).map_err(|_| DevError::BadState)?,
        )?;
        if end_block > self.num_blocks() {
            return Err(DevError::InvalidParam);
        }

        self.inner.read_block(self.range.start + block_id, buf)
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> DevResult {
        if block_id >= self.num_blocks() {
            return Err(DevError::InvalidParam);
        }

        let end_block = checked_add(
            block_id,
            u64::try_from(buf.len().div_ceil(self.block_size())).map_err(|_| DevError::BadState)?,
        )?;
        if end_block > self.num_blocks() {
            return Err(DevError::InvalidParam);
        }

        self.inner.write_block(self.range.start + block_id, buf)
    }

    fn flush(&mut self) -> DevResult {
        self.inner.flush()
    }
}
