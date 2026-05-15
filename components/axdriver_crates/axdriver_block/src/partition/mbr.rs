use alloc::{format, vec, vec::Vec};

use log::{debug, warn};

use super::{PartitionInfo, PartitionRegion, PartitionTable, PartitionTableKind};
use crate::{BlockDriverOps, DevError, DevResult};

const MBR_SIZE: usize = 512;
const MBR_DISK_SIGNATURE_OFFSET: usize = 440;
const MBR_PARTITION_TABLE_OFFSET: usize = 446;
const MBR_SIGNATURE_OFFSET: usize = 510;
const MBR_SIGNATURE: [u8; 2] = [0x55, 0xaa];
const MBR_PARTITION_ENTRY_SIZE: usize = 16;
const MBR_PARTITION_COUNT: usize = 4;

const PARTITION_TYPE_EMPTY: u8 = 0x00;
const PARTITION_TYPE_EXTENDED: u8 = 0x05;
const PARTITION_TYPE_EXTENDED_LBA: u8 = 0x0f;
const PARTITION_TYPE_LINUX_EXTENDED: u8 = 0x85;
const PARTITION_TYPE_GPT_PROTECTIVE: u8 = 0xee;

pub(super) fn scan_mbr_partitions<T: BlockDriverOps + ?Sized>(
    inner: &mut T,
) -> DevResult<Option<PartitionTable>> {
    let block_size = inner.block_size();
    if block_size < MBR_SIZE {
        return Err(DevError::InvalidParam);
    }

    let mut block_buf = vec![0u8; block_size];
    inner.read_block(0, &mut block_buf)?;

    let mbr = &block_buf[..MBR_SIZE];
    if mbr[MBR_SIGNATURE_OFFSET..MBR_SIGNATURE_OFFSET + MBR_SIGNATURE.len()] != MBR_SIGNATURE {
        return Ok(None);
    }

    let disk_signature = u32::from_le_bytes(
        mbr[MBR_DISK_SIGNATURE_OFFSET..MBR_DISK_SIGNATURE_OFFSET + 4]
            .try_into()
            .map_err(|_| DevError::BadState)?,
    );
    let total_blocks = inner.num_blocks();
    let mut partitions = Vec::new();

    for index in 0..MBR_PARTITION_COUNT {
        let entry_offset = MBR_PARTITION_TABLE_OFFSET + index * MBR_PARTITION_ENTRY_SIZE;
        let entry = &mbr[entry_offset..entry_offset + MBR_PARTITION_ENTRY_SIZE];
        let boot_indicator = entry[0];
        let partition_type = entry[4];
        let start_lba =
            u32::from_le_bytes(entry[8..12].try_into().map_err(|_| DevError::BadState)?) as u64;
        let size_in_lba =
            u32::from_le_bytes(entry[12..16].try_into().map_err(|_| DevError::BadState)?) as u64;

        if partition_type == PARTITION_TYPE_EMPTY || size_in_lba == 0 {
            continue;
        }
        if partition_type == PARTITION_TYPE_GPT_PROTECTIVE {
            debug!("skipping protective GPT MBR partition[{index}]");
            continue;
        }
        if matches!(
            partition_type,
            PARTITION_TYPE_EXTENDED | PARTITION_TYPE_EXTENDED_LBA | PARTITION_TYPE_LINUX_EXTENDED
        ) {
            debug!("skipping unsupported extended MBR partition[{index}]");
            continue;
        }

        let Some(end_lba) = start_lba.checked_add(size_in_lba) else {
            warn!("skipping overflowing MBR partition[{index}]");
            continue;
        };
        if start_lba >= total_blocks || end_lba > total_blocks {
            warn!(
                "skipping out-of-range MBR partition[{index}] lba {start_lba}..{end_lba}, device \
                 blocks {total_blocks}"
            );
            continue;
        }

        partitions.push(PartitionInfo {
            index,
            table_kind: PartitionTableKind::Mbr,
            region: PartitionRegion { start_lba, end_lba },
            name: None,
            part_uuid: (disk_signature != 0)
                .then(|| format!("{disk_signature:08X}-{:02X}", index + 1)),
            bootable: boot_indicator == 0x80,
        });
    }

    Ok(Some(PartitionTable {
        kind: PartitionTableKind::Mbr,
        partitions,
    }))
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use ax_driver_base::{BaseDriverOps, DeviceType};

    use super::*;
    use crate::partition::scan_partitions;

    struct MemBlock {
        data: Vec<u8>,
        block_size: usize,
    }

    impl MemBlock {
        fn new(num_blocks: usize) -> Self {
            Self {
                data: vec![0; num_blocks * MBR_SIZE],
                block_size: MBR_SIZE,
            }
        }

        fn write_mbr_signature(&mut self) {
            self.data[MBR_SIGNATURE_OFFSET..MBR_SIGNATURE_OFFSET + MBR_SIGNATURE.len()]
                .copy_from_slice(&MBR_SIGNATURE);
        }

        fn write_disk_signature(&mut self, signature: u32) {
            self.data[MBR_DISK_SIGNATURE_OFFSET..MBR_DISK_SIGNATURE_OFFSET + 4]
                .copy_from_slice(&signature.to_le_bytes());
        }

        fn write_partition(
            &mut self,
            index: usize,
            boot_indicator: u8,
            partition_type: u8,
            start_lba: u32,
            size_in_lba: u32,
        ) {
            let offset = MBR_PARTITION_TABLE_OFFSET + index * MBR_PARTITION_ENTRY_SIZE;
            let entry = &mut self.data[offset..offset + MBR_PARTITION_ENTRY_SIZE];
            entry[0] = boot_indicator;
            entry[4] = partition_type;
            entry[8..12].copy_from_slice(&start_lba.to_le_bytes());
            entry[12..16].copy_from_slice(&size_in_lba.to_le_bytes());
        }
    }

    impl BaseDriverOps for MemBlock {
        fn device_name(&self) -> &str {
            "memblock"
        }

        fn device_type(&self) -> DeviceType {
            DeviceType::Block
        }
    }

    impl BlockDriverOps for MemBlock {
        fn num_blocks(&self) -> u64 {
            (self.data.len() / self.block_size) as u64
        }

        fn block_size(&self) -> usize {
            self.block_size
        }

        fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> DevResult {
            if !buf.len().is_multiple_of(self.block_size) {
                return Err(DevError::InvalidParam);
            }
            let offset = usize::try_from(block_id)
                .map_err(|_| DevError::BadState)?
                .checked_mul(self.block_size)
                .ok_or(DevError::BadState)?;
            let end = offset.checked_add(buf.len()).ok_or(DevError::BadState)?;
            if end > self.data.len() {
                return Err(DevError::Io);
            }
            buf.copy_from_slice(&self.data[offset..end]);
            Ok(())
        }

        fn write_block(&mut self, block_id: u64, buf: &[u8]) -> DevResult {
            if !buf.len().is_multiple_of(self.block_size) {
                return Err(DevError::InvalidParam);
            }
            let offset = usize::try_from(block_id)
                .map_err(|_| DevError::BadState)?
                .checked_mul(self.block_size)
                .ok_or(DevError::BadState)?;
            let end = offset.checked_add(buf.len()).ok_or(DevError::BadState)?;
            if end > self.data.len() {
                return Err(DevError::Io);
            }
            self.data[offset..end].copy_from_slice(buf);
            Ok(())
        }

        fn flush(&mut self) -> DevResult {
            Ok(())
        }
    }

    #[test]
    fn scans_valid_primary_partitions_without_boot_requirement() {
        let mut disk = MemBlock::new(128);
        disk.write_mbr_signature();
        disk.write_disk_signature(0x1234_abcd);
        disk.write_partition(0, 0x00, 0x83, 8, 16);
        disk.write_partition(1, 0x80, 0x0c, 40, 24);

        let table = scan_partitions(&mut disk).unwrap();

        assert_eq!(table.kind, PartitionTableKind::Mbr);
        assert_eq!(table.partitions.len(), 2);
        assert_eq!(table.partitions[0].index, 0);
        assert!(!table.partitions[0].bootable);
        assert_eq!(
            table.partitions[0].region,
            PartitionRegion {
                start_lba: 8,
                end_lba: 24,
            }
        );
        assert_eq!(
            table.partitions[0].part_uuid.as_deref(),
            Some("1234ABCD-01")
        );
        assert_eq!(table.partitions[1].index, 1);
        assert!(table.partitions[1].bootable);
        assert_eq!(
            table.partitions[1].region,
            PartitionRegion {
                start_lba: 40,
                end_lba: 64,
            }
        );
        assert_eq!(
            table.partitions[1].part_uuid.as_deref(),
            Some("1234ABCD-02")
        );
    }

    #[test]
    fn skips_empty_protective_extended_and_out_of_range_entries() {
        let mut disk = MemBlock::new(64);
        disk.write_mbr_signature();
        disk.write_partition(0, 0x00, PARTITION_TYPE_EMPTY, 0, 0);
        disk.write_partition(1, 0x00, PARTITION_TYPE_GPT_PROTECTIVE, 1, 63);
        disk.write_partition(2, 0x00, PARTITION_TYPE_EXTENDED_LBA, 8, 16);
        disk.write_partition(3, 0x00, 0x83, 60, 8);

        let table = scan_partitions(&mut disk).unwrap();

        assert_eq!(table.kind, PartitionTableKind::Mbr);
        assert!(table.partitions.is_empty());
    }

    #[test]
    fn omits_partuuid_when_disk_signature_is_zero() {
        let mut disk = MemBlock::new(64);
        disk.write_mbr_signature();
        disk.write_partition(2, 0x00, 0x83, 4, 8);

        let table = scan_partitions(&mut disk).unwrap();

        assert_eq!(table.kind, PartitionTableKind::Mbr);
        assert_eq!(table.partitions.len(), 1);
        assert_eq!(table.partitions[0].index, 2);
        assert_eq!(table.partitions[0].part_uuid, None);
    }
}
