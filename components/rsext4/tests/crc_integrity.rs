//! CRC-focused integration tests for metadata integrity.
//!
//! These tests validate the `metadata_csum` behavior that protects ext4
//! metadata around normal file operations. They intentionally target
//! superblocks, group descriptors, and bitmaps after writing a file.
//! File payload blocks themselves are not covered because this implementation
//! does not currently expose a data-block CRC feature.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use rsext4::{
    blockgroup_description::Ext4GroupDesc,
    bmalloc::AbsoluteBN,
    checksum::{
        ext4_block_bitmap_csum32, ext4_group_desc_csum16, ext4_inode_bitmap_csum32,
        ext4_superblock_csum32,
    },
    endian::DiskFormat,
    error::{Errno, Ext4Error, Ext4Result},
    superblock::Ext4Superblock,
    *,
};

/// Shared in-memory block device so tests can remount the same disk image and
/// corrupt raw metadata bytes between mounts without relying on private APIs.
#[derive(Clone)]
struct SharedCrcDevice {
    data: Rc<RefCell<Vec<u8>>>,
    block_size: u32,
    now: Rc<Cell<i64>>,
}

impl SharedCrcDevice {
    fn new(size: usize) -> Self {
        Self {
            data: Rc::new(RefCell::new(vec![0; size])),
            block_size: BLOCK_SIZE as u32,
            now: Rc::new(Cell::new(1_700_000_000)),
        }
    }

    fn read_bytes(&self, offset: usize, len: usize) -> Vec<u8> {
        self.data.borrow()[offset..offset + len].to_vec()
    }

    fn write_bytes(&self, offset: usize, bytes: &[u8]) {
        self.data.borrow_mut()[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn read_block_bytes(&self, block_id: u64) -> Vec<u8> {
        self.read_bytes(block_id as usize * BLOCK_SIZE, BLOCK_SIZE)
    }

    fn write_block_bytes(&self, block_id: u64, bytes: &[u8]) {
        self.write_bytes(block_id as usize * BLOCK_SIZE, bytes);
    }
}

impl BlockDevice for SharedCrcDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        let start = block_id.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.borrow().len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                (self.data.borrow().len() / self.block_size as usize) as u64,
            ));
        }
        buffer.copy_from_slice(&self.data.borrow()[start..end]);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        let start = block_id.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.borrow().len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                (self.data.borrow().len() / self.block_size as usize) as u64,
            ));
        }
        self.data.borrow_mut()[start..end].copy_from_slice(buffer);
        Ok(())
    }

    fn open(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn close(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn total_blocks(&self) -> u64 {
        (self.data.borrow().len() / self.block_size as usize) as u64
    }

    fn block_size(&self) -> u32 {
        self.block_size
    }

    fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

fn new_jbd2_dev(device: SharedCrcDevice) -> Jbd2Dev<SharedCrcDevice> {
    Jbd2Dev::initial_jbd2dev(0, device, true)
}

fn build_filesystem_with_written_file() -> (SharedCrcDevice, Vec<u8>) {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let payload = b"crc integration payload".to_vec();

    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut fs = mount(&mut jbd2_dev).expect("mount failed");
    mkfile(&mut jbd2_dev, &mut fs, "/crc.txt", Some(&payload), None).expect("mkfile failed");
    umount(fs, &mut jbd2_dev).expect("umount failed");

    (device, payload)
}

fn read_superblock(device: &SharedCrcDevice) -> Ext4Superblock {
    let bytes = device.read_bytes(SUPERBLOCK_OFFSET as usize, Ext4Superblock::SUPERBLOCK_SIZE);
    Ext4Superblock::from_disk_bytes(&bytes)
}

fn write_superblock(device: &SharedCrcDevice, sb: &Ext4Superblock) {
    let mut bytes = vec![0u8; Ext4Superblock::SUPERBLOCK_SIZE];
    sb.to_disk_bytes(&mut bytes);
    device.write_bytes(SUPERBLOCK_OFFSET as usize, &bytes);
}

fn read_group_desc0(device: &SharedCrcDevice, sb: &Ext4Superblock) -> Ext4GroupDesc {
    let desc_size = sb.get_desc_size() as usize;
    let bytes = device.read_bytes(BLOCK_SIZE, desc_size);
    Ext4GroupDesc::from_disk_bytes(&bytes)
}

fn write_group_desc0(device: &SharedCrcDevice, sb: &Ext4Superblock, desc: &Ext4GroupDesc) {
    let desc_size = sb.get_desc_size() as usize;
    let mut bytes = vec![0u8; Ext4GroupDesc::EXT4_DESC_SIZE_64BIT];
    desc.to_disk_bytes(&mut bytes);
    device.write_bytes(BLOCK_SIZE, &bytes[..desc_size]);
}

#[test]
fn checksums_are_persisted_and_clean_remount_preserves_the_written_file() {
    // Test idea: write one real file, inspect the raw on-disk checksum fields,
    // and then remount to prove the intact image passes verification end to end.
    let (device, payload) = build_filesystem_with_written_file();

    let sb = read_superblock(&device);
    assert!(sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM));
    assert_ne!(sb.s_checksum, 0);
    assert_eq!(sb.s_checksum, ext4_superblock_csum32(&sb));

    let desc = read_group_desc0(&device, &sb);
    let mut desc_for_csum = desc;
    desc_for_csum.bg_checksum = 0;
    let mut desc_bytes = [0u8; Ext4GroupDesc::EXT4_DESC_SIZE_64BIT];
    desc_for_csum.to_disk_bytes(&mut desc_bytes);
    let expected_desc_csum =
        ext4_group_desc_csum16(&sb, 0, &desc_bytes[..sb.get_desc_size() as usize]);
    assert_eq!(desc.bg_checksum, expected_desc_csum);

    let block_bitmap = device.read_block_bytes(desc.block_bitmap());
    let inode_bitmap = device.read_block_bytes(desc.inode_bitmap());
    assert_eq!(
        desc.block_bitmap_csum(),
        ext4_block_bitmap_csum32(&sb, &block_bitmap)
    );
    assert_eq!(
        desc.inode_bitmap_csum(),
        ext4_inode_bitmap_csum32(&sb, &inode_bitmap)
    );

    let mut remount_dev = new_jbd2_dev(device.clone());
    let mut fs = mount(&mut remount_dev).expect("mount after intact checksum data failed");
    let read_back = read_file(&mut remount_dev, &mut fs, "/crc.txt").expect("read_file failed");
    assert_eq!(read_back, payload);
    umount(fs, &mut remount_dev).expect("umount failed");
}

#[test]
fn corrupted_superblock_checksum_is_reported_as_euclean_on_mount() {
    // Test idea: corrupt only the stored superblock CRC field and ensure mount
    // rejects the image with the checksum-specific EUCLEAN errno.
    let (device, _) = build_filesystem_with_written_file();

    let mut sb = read_superblock(&device);
    sb.s_checksum ^= 0x1;
    write_superblock(&device, &sb);

    let mut remount_dev = new_jbd2_dev(device);
    let err = match mount(&mut remount_dev) {
        Ok(_) => panic!("mount should fail on corrupted superblock CRC"),
        Err(err) => err,
    };
    assert_eq!(err.code, Errno::EUCLEAN);
}

#[test]
fn corrupted_group_descriptor_checksum_is_reported_as_euclean_on_mount() {
    // Test idea: corrupt the stored group descriptor checksum field and ensure
    // the descriptor verifier fails before mount starts normal filesystem work.
    let (device, _) = build_filesystem_with_written_file();

    let sb = read_superblock(&device);
    let mut desc = read_group_desc0(&device, &sb);
    desc.bg_checksum ^= 0x1;
    write_group_desc0(&device, &sb, &desc);

    let mut remount_dev = new_jbd2_dev(device);
    let err = match mount(&mut remount_dev) {
        Ok(_) => panic!("mount should fail on corrupted GDT CRC"),
        Err(err) => err,
    };
    assert_eq!(err.code, Errno::EUCLEAN);
}

#[test]
fn corrupted_block_bitmap_payload_is_reported_as_euclean_on_mount() {
    // Test idea: damage the protected bitmap payload while keeping the stored
    // checksum untouched so mount must discover the mismatch itself.
    let (device, _) = build_filesystem_with_written_file();

    let sb = read_superblock(&device);
    let desc = read_group_desc0(&device, &sb);
    let mut block_bitmap = device.read_block_bytes(desc.block_bitmap());
    block_bitmap[0] ^= 0x1;
    device.write_block_bytes(desc.block_bitmap(), &block_bitmap);

    let mut remount_dev = new_jbd2_dev(device);
    let err = match mount(&mut remount_dev) {
        Ok(_) => panic!("mount should fail on corrupted bitmap payload"),
        Err(err) => err,
    };
    assert_eq!(err.code, Errno::EUCLEAN);
}
