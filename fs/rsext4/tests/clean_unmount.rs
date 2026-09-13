//! Clean publication must follow the durable journal checkpoint tail.

use std::sync::{Arc, Mutex};

use rsext4::{endian::DiskFormat, superblock::Ext4Superblock, *};

#[test]
fn clean_superblock_is_written_only_after_the_journal_tail_is_empty() {
    let storage = Arc::new(Mutex::new(Storage {
        bytes: vec![0; 64 * 1024 * 1024],
        journal_offset: None,
        clean_writes: 0,
        premature_clean: false,
    }));
    let mut device = Jbd2Dev::initial_jbd2dev(0, TraceDevice(Arc::clone(&storage)), true);
    mkfs(&mut device).unwrap();
    let mut filesystem = Ext4FileSystem::mount(&mut device).unwrap();
    mkfile(
        &mut device,
        &mut filesystem,
        "/clean-order",
        Some(b"persisted"),
        None,
    )
    .unwrap();
    storage.lock().unwrap().journal_offset = Some(
        filesystem.journal_sb_block_start.unwrap().raw() as usize
            * filesystem.superblock.block_size() as usize,
    );
    filesystem.umount(&mut device).unwrap();
    let storage = storage.lock().unwrap();
    assert_ne!(
        storage.clean_writes, 0,
        "the test must observe final clean publication"
    );
    assert!(
        !storage.premature_clean,
        "RECOVER was cleared while the journal still required replay"
    );
}

struct Storage {
    bytes: Vec<u8>,
    journal_offset: Option<usize>,
    clean_writes: usize,
    premature_clean: bool,
}

struct TraceDevice(Arc<Mutex<Storage>>);

impl BlockIo for TraceDevice {
    fn read(&mut self, output: &mut [u8], sector: SectorId, _count: u32) -> Ext4Result<()> {
        let start = sector.as_usize()? * BLOCK_SIZE;
        output.copy_from_slice(&self.0.lock().unwrap().bytes[start..start + output.len()]);
        Ok(())
    }

    fn write(&mut self, input: &[u8], sector: SectorId, _count: u32) -> Ext4Result<()> {
        let start = sector.as_usize()? * BLOCK_SIZE;
        let mut storage = self.0.lock().unwrap();
        if let Some(journal) = storage.journal_offset
            && start == 0
            && input.len() >= SUPERBLOCK_OFFSET as usize + SUPERBLOCK_SIZE
        {
            let superblock = Ext4Superblock::from_disk_bytes(
                &input[SUPERBLOCK_OFFSET as usize..SUPERBLOCK_OFFSET as usize + SUPERBLOCK_SIZE],
            );
            if superblock.s_feature_incompat & Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER == 0 {
                storage.clean_writes += 1;
                let tail = u32::from_be_bytes(
                    storage.bytes[journal + 28..journal + 32]
                        .try_into()
                        .unwrap(),
                );
                storage.premature_clean |= tail != 0;
            }
        }
        storage.bytes[start..start + input.len()].copy_from_slice(input);
        Ok(())
    }

    fn geometry(&self) -> DeviceGeometry {
        DeviceGeometry::new(
            BLOCK_SIZE as u32,
            (self.0.lock().unwrap().bytes.len() / BLOCK_SIZE) as u64,
        )
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            flush: true,
            ..Default::default()
        }
    }

    fn flush(&mut self) -> Ext4Result<()> {
        Ok(())
    }
}

impl Clock for TraceDevice {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        Ok(Ext4Timestamp::new(1_700_000_000, 0))
    }
}
