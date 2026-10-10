use super::{
    BlockReader, BlockRegion, DiskId, Error, PartitionId, PartitionTableKind, Result, scan_volumes,
};

const BLOCK_SIZE: usize = 512;

struct MemReader {
    data: Vec<u8>,
    block_size: usize,
}

impl MemReader {
    fn new(blocks: usize) -> Self {
        Self {
            data: vec![0; blocks * BLOCK_SIZE],
            block_size: BLOCK_SIZE,
        }
    }

    fn block_mut(&mut self, block: usize) -> &mut [u8] {
        let start = block * self.block_size;
        &mut self.data[start..start + self.block_size]
    }
}

impl BlockReader for MemReader {
    fn block_size(&self) -> usize {
        self.block_size
    }

    fn num_blocks(&self) -> u64 {
        (self.data.len() / self.block_size) as u64
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8]) -> Result<()> {
        if buf.len() != self.block_size {
            return Err(Error::BufferSizeMismatch);
        }
        let start = block as usize * self.block_size;
        let end = start + self.block_size;
        let src = self.data.get(start..end).ok_or(Error::OutOfRange)?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

fn write_mbr_signature(block: &mut [u8]) {
    block[510] = 0x55;
    block[511] = 0xaa;
}

fn write_mbr_entry(
    block: &mut [u8],
    index: usize,
    partition_type: u8,
    start: u32,
    blocks: u32,
    bootable: bool,
) {
    let offset = 446 + index * 16;
    block[offset] = if bootable { 0x80 } else { 0 };
    block[offset + 4] = partition_type;
    block[offset + 8..offset + 12].copy_from_slice(&start.to_le_bytes());
    block[offset + 12..offset + 16].copy_from_slice(&blocks.to_le_bytes());
}

fn write_protective_gpt_header(
    reader: &mut MemReader,
    backup_lba: u64,
    first_usable_lba: u64,
    last_usable_lba: u64,
    entries_start_lba: u64,
    entry_count: u32,
) {
    let disk_blocks = reader.num_blocks();
    let mbr = reader.block_mut(0);
    mbr[446 + 4] = 0xee;
    mbr[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    mbr[446 + 12..446 + 16].copy_from_slice(&(disk_blocks as u32 - 1).to_le_bytes());
    write_mbr_signature(mbr);

    let header = reader.block_mut(1);
    header[0..8].copy_from_slice(b"EFI PART");
    header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    header[12..16].copy_from_slice(&92u32.to_le_bytes());
    header[24..32].copy_from_slice(&1u64.to_le_bytes());
    header[32..40].copy_from_slice(&backup_lba.to_le_bytes());
    header[40..48].copy_from_slice(&first_usable_lba.to_le_bytes());
    header[48..56].copy_from_slice(&last_usable_lba.to_le_bytes());
    header[72..80].copy_from_slice(&entries_start_lba.to_le_bytes());
    header[80..84].copy_from_slice(&entry_count.to_le_bytes());
    header[84..88].copy_from_slice(&128u32.to_le_bytes());
}

#[test]
fn raw_disk_fallback_covers_entire_reader() {
    let mut reader = MemReader::new(32);
    let scan = scan_volumes(&mut reader, DiskId(7)).unwrap();
    let volumes = &scan.volumes;

    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0].disk_id, DiskId(7));
    assert_eq!(volumes[0].partition_id, PartitionId(0));
    assert_eq!(volumes[0].table_kind, PartitionTableKind::Raw);
    assert_eq!(volumes[0].region, BlockRegion::new(0, 32));
    assert!(!volumes[0].bootable);
    assert_eq!(volumes[0].partuuid, None);
    assert_eq!(volumes[0].partlabel, None);
    assert!(scan.table_metadata.is_empty());
}

#[test]
fn scans_mbr_primary_partition() {
    let mut reader = MemReader::new(128);
    let mbr = reader.block_mut(0);
    mbr[440..444].copy_from_slice(&0x1234_abcd_u32.to_le_bytes());
    write_mbr_entry(mbr, 0, 0x83, 8, 40, true);
    write_mbr_signature(mbr);

    let scan = scan_volumes(&mut reader, DiskId(2)).unwrap();
    let volumes = &scan.volumes;

    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0].disk_id, DiskId(2));
    assert_eq!(volumes[0].partition_id, PartitionId(1));
    assert_eq!(volumes[0].table_kind, PartitionTableKind::Mbr);
    assert_eq!(volumes[0].region, BlockRegion::new(8, 40));
    assert!(volumes[0].bootable);
    assert_eq!(
        volumes[0].partuuid.as_ref().map(|uuid| uuid.0.as_str()),
        Some("1234abcd-01")
    );
    assert_eq!(scan.table_metadata, vec![BlockRegion::new(0, 1)]);
}

#[test]
fn preserves_mbr_and_ebr_metadata_without_data_partitions() {
    let mut reader = MemReader::new(64);
    let mbr = reader.block_mut(0);
    write_mbr_entry(mbr, 0, 0x05, 8, 40, false);
    write_mbr_signature(mbr);

    let ebr = reader.block_mut(8);
    write_mbr_signature(ebr);

    let scan = scan_volumes(&mut reader, DiskId(6)).unwrap();

    assert_eq!(scan.volumes.len(), 1);
    assert_eq!(scan.volumes[0].table_kind, PartitionTableKind::Raw);
    assert_eq!(scan.volumes[0].region, BlockRegion::new(0, 64));
    assert_eq!(
        scan.table_metadata,
        vec![BlockRegion::new(0, 1), BlockRegion::new(8, 1)]
    );
}

#[test]
fn scans_mbr_logical_partitions_without_exposing_extended_container() {
    let mut reader = MemReader::new(160);
    let mbr = reader.block_mut(0);
    mbr[440..444].copy_from_slice(&0x1234_abcd_u32.to_le_bytes());
    write_mbr_entry(mbr, 0, 0x83, 8, 8, false);
    write_mbr_entry(mbr, 1, 0x0f, 32, 96, false);
    write_mbr_signature(mbr);

    let ebr0 = reader.block_mut(32);
    write_mbr_entry(ebr0, 0, 0x83, 1, 10, true);
    write_mbr_entry(ebr0, 1, 0x05, 20, 40, false);
    write_mbr_signature(ebr0);

    let ebr1 = reader.block_mut(52);
    write_mbr_entry(ebr1, 0, 0x83, 1, 5, false);
    write_mbr_signature(ebr1);

    let scan = scan_volumes(&mut reader, DiskId(9)).unwrap();
    let volumes = &scan.volumes;

    assert_eq!(volumes.len(), 3);
    assert_eq!(volumes[0].partition_id, PartitionId(1));
    assert_eq!(volumes[0].region, BlockRegion::new(8, 8));
    assert_eq!(volumes[1].partition_id, PartitionId(5));
    assert_eq!(volumes[1].region, BlockRegion::new(33, 10));
    assert!(volumes[1].bootable);
    assert_eq!(
        volumes[1].partuuid.as_ref().map(|uuid| uuid.0.as_str()),
        Some("1234abcd-05")
    );
    assert_eq!(volumes[2].partition_id, PartitionId(6));
    assert_eq!(volumes[2].region, BlockRegion::new(53, 5));
    assert!(!volumes[2].bootable);
    assert_eq!(
        volumes[2].partuuid.as_ref().map(|uuid| uuid.0.as_str()),
        Some("1234abcd-06")
    );
    assert_eq!(
        scan.table_metadata,
        vec![
            BlockRegion::new(0, 1),
            BlockRegion::new(32, 1),
            BlockRegion::new(52, 1),
        ]
    );
}

#[test]
fn scans_gpt_single_partition() {
    let mut reader = MemReader::new(128);
    write_protective_gpt_header(&mut reader, 127, 34, 125, 2, 4);

    let entry = &mut reader.block_mut(2)[0..128];
    entry[0] = 0xaf;
    entry[16..32].copy_from_slice(&[
        0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd,
        0xef,
    ]);
    entry[32..40].copy_from_slice(&40u64.to_le_bytes());
    entry[40..48].copy_from_slice(&63u64.to_le_bytes());
    let name = "root";
    for (idx, unit) in name.encode_utf16().enumerate() {
        entry[56 + idx * 2..58 + idx * 2].copy_from_slice(&unit.to_le_bytes());
    }

    let scan = scan_volumes(&mut reader, DiskId(3)).unwrap();
    let volumes = &scan.volumes;

    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0].disk_id, DiskId(3));
    assert_eq!(volumes[0].partition_id, PartitionId(1));
    assert_eq!(volumes[0].table_kind, PartitionTableKind::Gpt);
    assert_eq!(volumes[0].region, BlockRegion::new(40, 24));
    assert_eq!(
        volumes[0].partuuid.as_ref().map(|uuid| uuid.0.as_str()),
        Some("76543210-ba98-fedc-0123-456789abcdef")
    );
    assert_eq!(
        volumes[0].partlabel.as_ref().map(|label| label.0.as_str()),
        Some("root")
    );
    assert_eq!(
        scan.table_metadata,
        vec![BlockRegion::new(0, 34), BlockRegion::new(126, 2)]
    );
}

#[test]
fn gpt_primary_entry_array_must_precede_first_usable_lba() {
    let mut reader = MemReader::new(128);
    write_protective_gpt_header(&mut reader, 127, 34, 125, 100, 4);

    let err = scan_volumes(&mut reader, DiskId(10)).unwrap_err();

    assert_eq!(err, Error::InvalidPartitionTable);
}

#[test]
fn gpt_backup_entry_array_must_follow_last_usable_lba() {
    let mut reader = MemReader::new(128);
    write_protective_gpt_header(&mut reader, 127, 34, 126, 2, 4);

    let err = scan_volumes(&mut reader, DiskId(11)).unwrap_err();

    assert_eq!(err, Error::InvalidPartitionTable);
}

#[test]
fn gpt_entry_array_must_fit_disk() {
    let mut reader = MemReader::new(128);
    let mbr = reader.block_mut(0);
    mbr[446 + 4] = 0xee;
    mbr[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    mbr[446 + 12..446 + 16].copy_from_slice(&127u32.to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xaa;

    let header = reader.block_mut(1);
    header[0..8].copy_from_slice(b"EFI PART");
    header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    header[12..16].copy_from_slice(&92u32.to_le_bytes());
    header[24..32].copy_from_slice(&1u64.to_le_bytes());
    header[32..40].copy_from_slice(&127u64.to_le_bytes());
    header[40..48].copy_from_slice(&34u64.to_le_bytes());
    header[48..56].copy_from_slice(&126u64.to_le_bytes());
    header[72..80].copy_from_slice(&127u64.to_le_bytes());
    header[80..84].copy_from_slice(&8u32.to_le_bytes());
    header[84..88].copy_from_slice(&128u32.to_le_bytes());

    let err = scan_volumes(&mut reader, DiskId(4)).unwrap_err();

    assert_eq!(err, Error::InvalidPartitionTable);
}
