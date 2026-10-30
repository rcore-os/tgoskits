//! GPT growth with a durable backup before any primary-table update.

use alloc::vec::Vec;

use super::{BlockReader, BlockRegion, BlockWriter, Error, PartitionId, Result, gpt};

/// Validated source bytes and physical LBAs for one backup-first GPT update.
struct Growth {
    /// Newest source proven to differ only by identity-preserving tail growth.
    table: gpt::Table,
    /// Original 512-byte MBR, retained apart from the protective size field.
    protective_mbr: Vec<u8>,
    /// Zero-based MBR slot containing the sole protective partition entry.
    protective_slot: usize,
    /// Final selected-partition region in physical device blocks.
    target: BlockRegion,
    /// Primary entry-array device LBA, retained or restored to validated space.
    primary_entries: u64,
    /// Final physical device LBA, where the new backup header will be written.
    backup: u64,
    /// Inclusive last data LBA, immediately before the backup entry array.
    last_usable: u64,
}

impl Growth {
    /// Resolves recoverable old/new copies without authorizing unrelated edits.
    /// `reader` describes the complete physical disk; `partition` is one-based
    /// and must identify its last occupied partition. Performs no writes.
    fn read<R: BlockReader>(reader: &mut R, partition: PartitionId) -> Result<Self> {
        let protective_mbr = super::mbr::read_sector0(reader)?
            .filter(|sector| super::mbr::has_protective_mbr(sector))
            .ok_or(Error::InvalidPartitionTable)?;
        // A hybrid MBR has another partition authority whose layout we cannot
        // update safely; only a single protective entry is eligible for growth.
        let mut slots = (0..4).filter(|index| protective_mbr[446 + index * 16 + 4] != 0);
        let protective_slot = slots.next().ok_or(Error::InvalidPartitionTable)?;
        let offset = 446 + protective_slot * 16;
        if slots.next().is_some()
            || protective_mbr[offset + 4] != 0xee
            || protective_mbr[offset + 8..offset + 12] != 1u32.to_le_bytes()
        {
            return Err(Error::Unsupported);
        }
        let copies = gpt::read_copies(reader)?;
        let selected = copies.newest()?;
        let entry = selected.entry(partition)?;
        let first = le_u64(&entry[32..40]);
        let old_last = le_u64(&entry[40..48]);
        if selected
            .entries
            .chunks_exact(selected.entry_size)
            .any(|entry| {
                entry[..16].iter().any(|byte| *byte != 0) && le_u64(&entry[40..48]) > old_last
            })
        {
            return Err(Error::Unsupported);
        }
        // A different partition's growth cannot authorize this operation.
        for old in copies.growth_tables() {
            let old_target = old.entry(partition)?;
            for (index, (new, previous)) in selected
                .entries
                .chunks_exact(selected.entry_size)
                .zip(old.entries.chunks_exact(old.entry_size))
                .enumerate()
            {
                if index + 1 != partition.0 as usize && new != previous {
                    return Err(Error::ConflictingPartitionTables);
                }
            }
            if old_target[..40] != entry[..40] || old_target[48..] != entry[48..] {
                return Err(Error::ConflictingPartitionTables);
            }
        }
        // Derive write destinations from physical capacity, never from an
        // unchecked AlternateLBA hint in an old or damaged primary header.
        let backup = reader
            .num_blocks()
            .checked_sub(1)
            .ok_or(Error::InvalidPartitionTable)?;
        let last_usable = backup
            .checked_sub(selected.entry_blocks(reader.block_size()) + 1)
            .ok_or(Error::InvalidPartitionTable)?;
        if old_last > last_usable || first > last_usable {
            return Err(Error::InvalidPartitionTable);
        }
        let primary_entries = copies.primary.as_ref().map_or(2, |table| table.entries_lba);
        if primary_entries
            .checked_add(selected.entry_blocks(reader.block_size()))
            .is_none_or(|end| end > selected.first)
        {
            return Err(Error::InvalidPartitionTable);
        }
        Ok(Self {
            table: selected.clone(),
            protective_mbr,
            protective_slot,
            target: BlockRegion::new(first, last_usable - first + 1),
            primary_entries,
            backup,
            last_usable,
        })
    }
}

/// The validated current and final regions of one last GPT partition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PartitionGrowth {
    /// Region from the newest provably compatible GPT copy, in device blocks.
    pub current: BlockRegion,
    /// Region ending immediately before the relocated backup entry array.
    pub target: BlockRegion,
}

/// Returns the current and final regions of the last GPT partition without writing.
/// A valid primary overrides unrelated backups; compatible backup-first growth
/// updates can resume. Without a valid primary, ambiguous backups are rejected.
/// Unreadable metadata and non-tail partitions are rejected. An invalid copy
/// may be recovered from another independently validated copy.
/// `reader` must expose the whole physical disk, not a partition view;
/// `partition` is one-based. Both returned regions use `reader`'s block size.
pub fn partition_growth<R: BlockReader>(
    reader: &mut R,
    partition: PartitionId,
) -> Result<PartitionGrowth> {
    let growth = Growth::read(reader, partition)?;
    let entry = growth.table.entry(partition)?;
    let start = le_u64(&entry[32..40]);
    let end = le_u64(&entry[40..48]);
    Ok(PartitionGrowth {
        current: BlockRegion::new(start, end - start + 1),
        target: growth.target,
    })
}

/// Decodes an exactly eight-byte field from a previously validated GPT record.
fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("fixed-width GPT field"))
}

/// Builds a checksummed header sector from `table`. `current`, `alternate` and
/// `entries` are device LBAs; `last` is the inclusive last usable data LBA.
/// Retains the source header's identity, first usable LBA and extension bytes.
fn header(table: &gpt::Table, current: u64, alternate: u64, entries: u64, last: u64) -> Vec<u8> {
    // Patch only location-dependent fields; retain disk identity and any header
    // extension bytes before recomputing both CRCs.
    let mut raw = table.header.clone();
    raw[16..20].fill(0);
    raw[24..32].copy_from_slice(&current.to_le_bytes());
    raw[32..40].copy_from_slice(&alternate.to_le_bytes());
    raw[48..56].copy_from_slice(&last.to_le_bytes());
    raw[72..80].copy_from_slice(&entries.to_le_bytes());
    raw[88..92].copy_from_slice(&gpt::crc32(&table.entries).to_le_bytes());
    let size = u32::from_le_bytes(raw[12..16].try_into().expect("fixed-width GPT field")) as usize;
    let checksum = gpt::crc32(&raw[..size]);
    raw[16..20].copy_from_slice(&checksum.to_le_bytes());
    raw
}

/// Writes `table`'s entry array at device LBA `start` through `writer`, zeroing
/// final-sector padding. Does not flush; the caller owns phase durability.
fn write_entries<W: BlockWriter>(writer: &mut W, table: &gpt::Table, start: u64) -> Result<()> {
    let size = writer.block_size();
    for (index, chunk) in table.entries.chunks(size).enumerate() {
        let mut block = alloc::vec![0; size];
        block[..chunk.len()].copy_from_slice(chunk);
        writer.write_block(start + index as u64, &block)?;
    }
    Ok(())
}

/// Expands one last partition, preserving all identities and extension bytes.
/// Writes a complete new backup and flushes it before changing the primary.
/// An error may leave a recoverable old/new pair; the caller must discard its
/// old partition devices and caches before reopening or retrying.
/// The caller must exclude other writers and provide a durable `flush`.
/// `writer` exposes the full disk; `partition` is a one-based partition number.
/// Returns its final region in device blocks after rereading both new copies.
/// Matching GPT copies and PMBR capacity cause no additional growth writes.
pub fn grow_partition<W: BlockWriter>(
    writer: &mut W,
    partition: PartitionId,
) -> Result<BlockRegion> {
    let mut growth = Growth::read(writer, partition)?;
    // Restore an invalid primary before reusing its only valid backup's
    // location. A torn backup update must always leave another valid copy.
    let initial = gpt::read_copies(writer)?;
    if initial.primary.is_none() && growth.table.current == growth.backup {
        write_entries(writer, &growth.table, growth.primary_entries)?;
        let restored = header(
            &growth.table,
            1,
            growth.table.current,
            growth.primary_entries,
            growth.table.last,
        );
        writer.write_block(1, &restored)?;
        writer.flush()?;
        if gpt::read_copies(writer)?.primary.is_none() {
            return Err(Error::InvalidPartitionTable);
        }
    }
    let index = (partition.0 as usize - 1) * growth.table.entry_size;
    growth.table.entries[index + 40..index + 48].copy_from_slice(&growth.last_usable.to_le_bytes());
    let backup_entries = growth.backup - growth.table.entry_blocks(writer.block_size());
    let backup_header = header(
        &growth.table,
        growth.backup,
        1,
        backup_entries,
        growth.last_usable,
    );
    let primary_header = header(
        &growth.table,
        1,
        growth.backup,
        growth.primary_entries,
        growth.last_usable,
    );
    let copies = gpt::read_copies(writer)?;
    let offset = 446 + growth.protective_slot * 16 + 12;
    let sectors = (growth.backup.min(u64::from(u32::MAX)) as u32).to_le_bytes();
    if copies.primary.as_ref().is_some_and(|table| {
        table.header == primary_header && table.entries == growth.table.entries
    }) && copies.backups.iter().any(|table| {
        table.current == growth.backup
            && table.header == backup_header
            && table.entries == growth.table.entries
    }) {
        if growth.protective_mbr[offset..offset + 4] != sectors {
            // GPT CRCs exclude sector zero. Retry must also converge when a
            // previous writeback persisted the new GPT before the PMBR size.
            let mut mbr = alloc::vec![0; writer.block_size()];
            writer.read_block(0, &mut mbr)?;
            mbr[offset..offset + 4].copy_from_slice(&sectors);
            writer.write_block(0, &mbr)?;
            writer.flush()?;
        }
        return Ok(growth.target);
    }
    // The backup array and its CRC-bearing header form the new authority.
    // Only after they are durable may an interrupted primary update be retried
    // from that copy. Relocation leaves the old backup at the image's old end.
    write_entries(writer, &growth.table, backup_entries)?;
    writer.write_block(growth.backup, &backup_header)?;
    writer.flush()?;
    // A protective MBR is retained byte-for-byte apart from its device size.
    let mut mbr = alloc::vec![0; writer.block_size()];
    writer.read_block(0, &mut mbr)?;
    mbr[..growth.protective_mbr.len()].copy_from_slice(&growth.protective_mbr);
    mbr[offset..offset + 4].copy_from_slice(&sectors);
    writer.write_block(0, &mbr)?;
    write_entries(writer, &growth.table, growth.primary_entries)?;
    writer.write_block(1, &primary_header)?;
    writer.flush()?;
    let verified = gpt::read_copies(writer)?;
    if !verified.primary.as_ref().is_some_and(|table| {
        table.header == primary_header && table.entries == growth.table.entries
    }) || !verified.backups.iter().any(|table| {
        table.current == growth.backup
            && table.header == backup_header
            && table.entries == growth.table.entries
    }) {
        return Err(Error::InvalidPartitionTable);
    }
    Ok(growth.target)
}
