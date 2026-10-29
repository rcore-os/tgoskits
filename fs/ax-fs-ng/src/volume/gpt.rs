//! GPT validation and copy selection, including backups left at an image's end.

use alloc::{format, string::String, vec, vec::Vec};

use super::{
    BlockReader, BlockRegion, BlockVolume, DiskId, Error, PartitionId, PartitionLabel,
    PartitionTableKind, PartitionUuid, Result, VolumeScan, mbr,
};

/// Header signature used to recognize GPT candidates before further validation.
const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";
/// Standard partition-entry prefix length in bytes, excluding vendor extensions.
const CORE_ENTRY_SIZE: usize = 128;
/// Byte offset of the type GUID whose all-zero value denotes an unused entry.
const PARTITION_TYPE_GUID_OFFSET: usize = 0;

/// Computes the GPT IEEE CRC-32 over exactly `bytes`, excluding sector padding.
pub(super) fn crc32(bytes: &[u8]) -> u32 {
    let mut value = !0u32;
    for byte in bytes {
        value ^= u32::from(*byte);
        for _ in 0..8 {
            value = (value >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(value & 1));
        }
    }
    !value
}

/// One independently validated copy. Raw records retain unknown extension
/// bytes so growth can preserve identities beyond the fields understood here.
#[derive(Clone)]
pub(super) struct Table {
    /// Complete header sector, including checksum and preserved extension bytes.
    pub header: Vec<u8>,
    /// CRC-checked entry records, excluding padding after the final record.
    pub entries: Vec<u8>,
    /// First usable device LBA, inclusive.
    pub first: u64,
    /// Last usable device LBA, inclusive, before this copy's backup metadata.
    pub last: u64,
    /// Device LBA of this header, checked against its physical read position.
    pub current: u64,
    /// Device LBA where this copy's partition-entry array begins.
    pub entries_lba: u64,
    /// On-disk record size in bytes, including extensions beyond the GPT core.
    pub entry_size: usize,
}

impl Table {
    /// Returns sectors needed by the entry array for `block_size` bytes/sector,
    /// rounding up to include padding in its final sector.
    pub(super) fn entry_blocks(&self, block_size: usize) -> u64 {
        self.entries.len().div_ceil(block_size) as u64
    }
    /// Borrows the occupied record at one-based `id`; rejects zero, unused and
    /// out-of-range partition numbers without changing the table.
    pub(super) fn entry(&self, id: PartitionId) -> Result<&[u8]> {
        let index = id.0.checked_sub(1).ok_or(Error::InvalidPartitionTable)? as usize;
        let offset = index
            .checked_mul(self.entry_size)
            .ok_or(Error::InvalidPartitionTable)?;
        self.entries
            .get(offset..offset + self.entry_size)
            .filter(|entry| !is_unused_entry(entry))
            .ok_or(Error::InvalidPartitionTable)
    }
    /// Removes CRCs, copy locations and the growable end from identity checks.
    pub(super) fn canonical_header(&self) -> Vec<u8> {
        let size = le_u32(&self.header[12..16]) as usize;
        let mut raw = self.header[..size].to_vec();
        raw[16..20].fill(0);
        raw[24..40].fill(0);
        raw[48..56].fill(0);
        raw[72..80].fill(0);
        raw[88..92].fill(0);
        raw
    }
    /// Proves identity preservation and at most one monotonic tail growth.
    /// `old` is another independently validated copy. Equality is accepted;
    /// relocation alone is allowed, but unrelated entry edits are not ordered.
    pub(super) fn grows_from(&self, old: &Self) -> bool {
        if self.canonical_header() != old.canonical_header()
            || self.entries.len() != old.entries.len()
            || self.last < old.last
        {
            return false;
        }
        let mut changed = false;
        for (new, old_entry) in self
            .entries
            .chunks_exact(self.entry_size)
            .zip(old.entries.chunks_exact(old.entry_size))
        {
            if new == old_entry {
                continue;
            }
            if changed
                || is_unused_entry(new)
                || new[..40] != old_entry[..40]
                || new[48..] != old_entry[48..]
                || le_u64(&new[40..48]) < le_u64(&old_entry[40..48])
            {
                return false;
            }
            let old_end = le_u64(&old_entry[40..48]);
            if old
                .entries
                .chunks_exact(old.entry_size)
                .any(|entry| !is_unused_entry(entry) && le_u64(&entry[40..48]) > old_end)
            {
                return false;
            }
            changed = true;
        }
        true
    }
}

/// Independently validated primary and backup candidates from one scan.
/// Read failures are retained separately from malformed metadata so discovery
/// can remain available while mutation refuses an incomplete view.
pub(super) struct Copies {
    /// Valid header/array at LBA 1, or None when absent, invalid or unreadable.
    pub primary: Option<Table>,
    /// Valid copies found at the physical tail and the old AlternateLBA hint.
    pub backups: Vec<Table>,
    /// Whether any probed header or entry array could not be read.
    read_failed: bool,
}

impl Copies {
    /// Discovery retains a valid primary's view until a growth operation repairs
    /// it; merely scanning a disk must not implicitly choose a larger region.
    pub(super) fn selected(&self) -> Result<&Table> {
        if let Some(primary) = &self.primary {
            return Ok(primary);
        }
        self.newest_validated()
    }
    /// Mutation requires every probed location to be readable. An I/O failure
    /// could hide a conflicting copy and cannot be treated as invalid metadata.
    pub(super) fn newest(&self) -> Result<&Table> {
        if self.read_failed {
            return Err(Error::Reader);
        }
        self.newest_validated()
    }
    /// Borrows candidates for mutation, using a valid primary as authority.
    /// Incompatible backups may belong to a previously flashed image and do
    /// not veto that primary. Compatible old/new backups remain candidates so
    /// an interrupted backup-first growth can resume from its durable update.
    /// Without a valid primary every backup participates; callers must reject
    /// ambiguity because GPT supplies no generation counter. Discovery's
    /// metadata protection must still cover all independently valid copies.
    pub(super) fn growth_tables(&self) -> impl Iterator<Item = &Table> {
        self.primary
            .iter()
            .chain(self.backups.iter().filter(|backup| {
                self.primary
                    .as_ref()
                    .is_none_or(|primary| backup.grows_from(primary) || primary.grows_from(backup))
            }))
    }
    /// Returns the greatest eligible copy under identity-preserving tail
    /// growth, rejecting candidates with no unambiguous monotonic ordering.
    fn newest_validated(&self) -> Result<&Table> {
        let mut tables = self.growth_tables();
        let mut selected = tables.next().ok_or(Error::InvalidPartitionTable)?;
        for table in tables {
            if table.grows_from(selected) {
                selected = table;
            } else if !selected.grows_from(table) {
                return Err(Error::ConflictingPartitionTables);
            }
        }
        Ok(selected)
    }
}

/// Reads one candidate at device LBA `position` from `reader`. Validates both
/// CRCs, metadata placement and all occupied partitions before returning raw
/// records; malformed, unreadable or unallocatable metadata remains an error.
fn read_table<R: BlockReader>(reader: &mut R, position: u64) -> Result<Table> {
    let block_size = reader.block_size();
    if block_size < 512 || position >= reader.num_blocks() {
        return Err(Error::InvalidPartitionTable);
    }
    let mut header = vec![0; block_size];
    reader.read_block(position, &mut header)?;
    if header.get(..8) != Some(GPT_SIGNATURE)
        || le_u32(&header[8..12]) != 0x0001_0000
        || le_u32(&header[20..24]) != 0
    {
        return Err(Error::InvalidPartitionTable);
    }
    let size = le_u32(&header[12..16]) as usize;
    if !(92..=block_size).contains(&size) {
        return Err(Error::InvalidPartitionTable);
    }
    let expected = le_u32(&header[16..20]);
    header[16..20].fill(0);
    let actual = crc32(&header[..size]);
    header[16..20].copy_from_slice(&expected.to_le_bytes());
    if actual != expected {
        return Err(Error::InvalidPartitionTable);
    }
    let current = le_u64(&header[24..32]);
    let alternate = le_u64(&header[32..40]);
    let first = le_u64(&header[40..48]);
    let last = le_u64(&header[48..56]);
    let entries_lba = le_u64(&header[72..80]);
    let count = le_u32(&header[80..84]) as usize;
    let entry_size = le_u32(&header[84..88]) as usize;
    if current != position
        || first > last
        || first < 3
        || last >= reader.num_blocks()
        || alternate >= reader.num_blocks()
        || alternate == current
        || count == 0
        || entry_size < CORE_ENTRY_SIZE
        || !(entry_size / CORE_ENTRY_SIZE).is_power_of_two()
        || !entry_size.is_multiple_of(CORE_ENTRY_SIZE)
    {
        return Err(Error::InvalidPartitionTable);
    }
    let bytes = count
        .checked_mul(entry_size)
        .ok_or(Error::InvalidPartitionTable)?;
    let blocks = bytes.div_ceil(block_size) as u64;
    let entries_end = entries_lba
        .checked_add(blocks)
        .filter(|end| *end <= reader.num_blocks())
        .ok_or(Error::InvalidPartitionTable)?;
    if current == 1 {
        if entries_lba < 2
            || entries_end > first
            || alternate
                .checked_sub(blocks)
                .is_none_or(|start| start <= last)
        {
            return Err(Error::InvalidPartitionTable);
        }
    } else if alternate != 1 || entries_lba <= last || entries_end > current {
        return Err(Error::InvalidPartitionTable);
    }
    let buffer_size = usize::try_from(blocks)
        .ok()
        .and_then(|count| count.checked_mul(block_size))
        .ok_or(Error::InvalidPartitionTable)?;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(buffer_size)
        .map_err(|_| Error::NoMemory)?;
    entries.resize(buffer_size, 0);
    reader.read_blocks(entries_lba, blocks, &mut entries)?;
    entries.truncate(bytes);
    if crc32(&entries) != le_u32(&header[88..92]) {
        return Err(Error::InvalidPartitionTable);
    }
    let table = Table {
        header,
        entries,
        first,
        last,
        current,
        entries_lba,
        entry_size,
    };
    validate_partitions(&table)?;
    Ok(table)
}

/// Checks `table` for bounded nonoverlapping partitions with distinct, nonzero
/// unique GUIDs; unused entry slots do not participate in these checks.
fn validate_partitions(table: &Table) -> Result<()> {
    let mut ranges = Vec::new();
    let mut identities: Vec<&[u8]> = Vec::new();
    for entry in table
        .entries
        .chunks_exact(table.entry_size)
        .filter(|entry| !is_unused_entry(entry))
    {
        let first = le_u64(&entry[32..40]);
        let last = le_u64(&entry[40..48]);
        if first < table.first
            || last > table.last
            || first > last
            || entry[16..32].iter().all(|byte| *byte == 0)
            || identities.contains(&&entry[16..32])
        {
            return Err(Error::InvalidPartitionTable);
        }
        ranges.push((first, last));
        identities.push(&entry[16..32]);
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 >= pair[1].0) {
        return Err(Error::InvalidPartitionTable);
    }
    Ok(())
}

/// Probes device LBA `position` on `reader`. Invalid metadata becomes None;
/// unreadable metadata also sets `read_failed` so a later writer must refuse it.
/// Allocation and other errors propagate instead of masquerading as absence.
fn optional_table<R: BlockReader>(
    reader: &mut R,
    position: u64,
    read_failed: &mut bool,
) -> Result<Option<Table>> {
    match read_table(reader, position) {
        Ok(table) => Ok(Some(table)),
        Err(Error::InvalidPartitionTable) => Ok(None),
        Err(Error::Reader) => {
            *read_failed = true;
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Reads the primary, physical-tail backup and bounded old-backup hint through
/// `reader`. Returns candidates only when discovery can select a valid copy;
/// callers performing writes must additionally use `Copies::newest`.
pub(super) fn read_copies<R: BlockReader>(reader: &mut R) -> Result<Copies> {
    let mut read_failed = false;
    let primary = optional_table(reader, 1, &mut read_failed)?;
    let physical_tail = reader
        .num_blocks()
        .checked_sub(1)
        .ok_or(Error::InvalidPartitionTable)?;
    let mut hints = vec![physical_tail];
    // An untrusted pointer is only a bounded read hint, never a write address.
    let mut raw = vec![0; reader.block_size()];
    if raw.len() < 512 {
        return Err(Error::InvalidPartitionTable);
    }
    match reader.read_block(1, &mut raw) {
        Ok(()) => {}
        Err(Error::Reader) => read_failed = true,
        Err(error) => return Err(error),
    }
    if raw.get(..8) == Some(GPT_SIGNATURE) {
        let hint = le_u64(&raw[32..40]);
        if hint > 1 && hint < reader.num_blocks() && hint != physical_tail {
            hints.push(hint);
        }
    }
    let mut backups = Vec::new();
    for position in hints {
        if let Some(table) = optional_table(reader, position, &mut read_failed)? {
            backups.push(table);
        }
    }
    let copies = Copies {
        primary,
        backups,
        read_failed,
    };
    copies.selected()?;
    Ok(copies)
}

/// Discovers GPT partitions on `reader`, assigning each volume `disk_id`.
/// Returns None without a protective MBR, errors for an unusable GPT, and
/// reserves metadata/tail regions conservatively across all validated copies.
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
    let copies = read_copies(reader)?;
    let table = copies.selected()?;
    let mut volumes = Vec::new();
    for (index, entry) in table.entries.chunks_exact(table.entry_size).enumerate() {
        if is_unused_entry(entry) {
            continue;
        }
        let first = le_u64(&entry[32..40]);
        let last = le_u64(&entry[40..48]);
        volumes.push(BlockVolume {
            disk_id,
            partition_id: PartitionId(index as u32 + 1),
            region: BlockRegion::new(first, last - first + 1),
            table_kind: PartitionTableKind::Gpt,
            bootable: false,
            partuuid: Some(PartitionUuid(format_guid(&entry[16..32]))),
            partlabel: decode_partition_name(&entry[56..CORE_ENTRY_SIZE]).map(PartitionLabel),
        });
    }
    // Protect both valid copies and the unused device tail during migration.
    let first = copies
        .primary
        .iter()
        .chain(copies.backups.iter())
        .map(|table| table.first)
        .max()
        .unwrap_or(table.first);
    let last = copies
        .primary
        .iter()
        .chain(copies.backups.iter())
        .map(|table| table.last)
        .min()
        .unwrap_or(table.last);
    Ok(Some(VolumeScan {
        volumes,
        table_metadata: vec![
            BlockRegion::new(0, first),
            BlockRegion::new(last + 1, reader.num_blocks() - last - 1),
        ],
    }))
}
/// Tests the type GUID of a record of at least `CORE_ENTRY_SIZE` bytes.
fn is_unused_entry(entry: &[u8]) -> bool {
    entry[PARTITION_TYPE_GUID_OFFSET..PARTITION_TYPE_GUID_OFFSET + 16]
        .iter()
        .all(|byte| *byte == 0)
}

/// Decodes little-endian UTF-16 name units up to NUL, omitting invalid scalar
/// values; `bytes` is the name field of a validated core entry.
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

/// Formats a 16-byte GPT GUID, whose first three fields use little-endian order.
fn format_guid(bytes: &[u8]) -> String {
    let data1 = le_u32(&bytes[0..4]);
    let data2 = le_u16(&bytes[4..6]);
    let data3 = le_u16(&bytes[6..8]);
    format!(
        "{data1:08x}-{data2:04x}-{data3:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

/// Decodes an exactly two-byte field from an already bounded GPT record.
fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes.try_into().expect("slice length is fixed"))
}

/// Decodes an exactly four-byte field from an already bounded GPT record.
fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("slice length is fixed"))
}

/// Decodes an exactly eight-byte field from an already bounded GPT record.
fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("slice length is fixed"))
}
