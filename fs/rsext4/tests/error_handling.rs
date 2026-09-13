//! Error-path tests for filesystem operations.
//!
//! Every case fixes one observable result so an error-path regression cannot
//! be reported as a passing test.

use std::{cell::Cell, rc::Rc};

use rsext4::{
    bmalloc::{AbsoluteBN, BGIndex},
    error::{Ext4Error, Ext4Result},
    *,
};

/// Mock block device with knobs for injecting IO and capacity failures.
struct ErrorMockDevice {
    data: Vec<u8>,
    block_size: u32,
    // Failure injection toggles.
    fail_on_read: Rc<Cell<bool>>,
    fail_on_write: bool,
    fail_on_specific_block: Option<SectorId>,
    fail_after_bytes: Option<usize>,
    bytes_written: usize,
    now: Cell<i64>,
}

impl ErrorMockDevice {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            block_size: rsext4::BLOCK_SIZE as u32,
            fail_on_read: Rc::new(Cell::new(false)),
            fail_on_write: false,
            fail_on_specific_block: None,
            fail_after_bytes: None,
            bytes_written: 0,
            now: Cell::new(1_700_000_000),
        }
    }

    fn with_read_failure_switch(size: usize) -> (Self, Rc<Cell<bool>>) {
        let device = Self::new(size);
        let fail_on_read = Rc::clone(&device.fail_on_read);
        (device, fail_on_read)
    }
}

impl BlockIo for ErrorMockDevice {
    fn read(&mut self, buffer: &mut [u8], sector: rsext4::SectorId, _count: u32) -> Ext4Result<()> {
        if self.fail_on_read.get() {
            return Err(Ext4Error::io());
        }

        if let Some(fail_block) = self.fail_on_specific_block
            && sector == fail_block
        {
            return Err(Ext4Error::corrupted());
        }

        let start = sector.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                sector.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
            ));
        }
        buffer.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], sector: rsext4::SectorId, _count: u32) -> Ext4Result<()> {
        if self.fail_on_write {
            return Err(Ext4Error::io());
        }

        if let Some(fail_block) = self.fail_on_specific_block
            && sector == fail_block
        {
            return Err(Ext4Error::corrupted());
        }

        if let Some(limit) = self.fail_after_bytes {
            self.bytes_written += buffer.len();
            if self.bytes_written > limit {
                return Err(Ext4Error::no_space());
            }
        }

        let start = sector.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                sector.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
            ));
        }
        self.data[start..end].copy_from_slice(buffer);
        Ok(())
    }

    fn geometry(&self) -> rsext4::DeviceGeometry {
        rsext4::DeviceGeometry::new(self.block_size, {
            (self.data.len() / self.block_size as usize) as u64
        })
    }

    fn capabilities(&self) -> rsext4::DeviceCapabilities {
        rsext4::DeviceCapabilities {
            read_only: { false },

            flush: true,

            ..rsext4::DeviceCapabilities::default()
        }
    }

    fn flush(&mut self) -> rsext4::Ext4Result<()> {
        Ok(())
    }
}

impl rsext4::Clock for ErrorMockDevice {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

#[cfg(test)]
mod error_handling_tests {
    use super::*;

    #[test]
    fn inode_bitmap_read_failure_is_not_reported_as_free() {
        let (device, fail_on_read) = ErrorMockDevice::with_read_failure_switch(100 * 1024 * 1024);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        fs.bitmap_cache = rsext4::cache::BitmapCache::create_default();
        fail_on_read.set(true);

        let error = fs
            .inode_num_already_allocated(
                &mut jbd2_dev,
                rsext4::bmalloc::InodeNumber::new(2).expect("valid root inode"),
            )
            .expect_err("inode bitmap I/O failure must remain distinguishable from a free inode");

        assert_eq!(error.kind(), Ext4ErrorKind::Io);
    }

    /// Verifies filesystem-size and ext4 component-length boundaries.
    #[test]
    fn test_filesystem_boundaries() {
        // Probe mkfs behavior on a relatively small backing device.
        let small_device = ErrorMockDevice::new(20 * 1024 * 1024); // 20MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, small_device, true);

        mkfs(&mut jbd2_dev).expect("20 MiB filesystem must format successfully");

        // Repeat the rest of the checks on a normal-sized device.
        let normal_device = ErrorMockDevice::new(50 * 1024 * 1024); // 50MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, normal_device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/boundary").expect("mkdir failed");

        mkfile(&mut jbd2_dev, &mut fs, "/boundary/empty.txt", None, None).expect("mkfile failed");

        // Check the exact-name-limit case.
        let long_name = "a".repeat(rsext4::DIRNAME_LEN);
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            &format!("/boundary/{long_name}"),
            Some(b"test"),
            None,
        )
        .expect("255-byte ext4 component must be accepted");
        let too_long_name = "a".repeat(rsext4::DIRNAME_LEN + 1);
        let error = mkfile(
            &mut jbd2_dev,
            &mut fs,
            &format!("/boundary/{too_long_name}"),
            Some(b"test"),
            None,
        )
        .expect_err("256-byte ext4 component must be rejected");
        assert_eq!(error.kind(), Ext4ErrorKind::InvalidInput);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Tries to fill the filesystem with large files until creation fails, then
    /// checks that the last successful file is still readable.
    #[test]
    fn test_resource_exhaustion() {
        let device = ErrorMockDevice::new(50 * 1024 * 1024); // 50MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/exhaustion").expect("mkdir failed");

        // Create large files in a loop until allocation eventually stops succeeding.
        let mut file_count = 0;
        let exhaustion_error;
        let file_size = 1024 * 1024; // 1 MiB per file.
        let large_data = vec![b'X'; file_size];

        loop {
            let filename = format!("/exhaustion/file{}.dat", file_count);
            let result = mkfile(&mut jbd2_dev, &mut fs, &filename, Some(&large_data), None);

            match result {
                Ok(_) => file_count += 1,
                Err(error) => {
                    exhaustion_error = error;
                    break;
                }
            }

            // Guard against an infinite loop if the device is larger than expected.
            assert!(
                file_count <= 40,
                "fixture must exhaust before its safety bound"
            );
        }

        // At least one file should have been created before exhaustion.
        assert!(file_count > 0);
        assert_eq!(exhaustion_error.kind(), Ext4ErrorKind::NoSpace);

        // The last successful file should still contain the full payload.
        let last_filename = format!("/exhaustion/file{}.dat", file_count - 1);
        let data = read_file(&mut jbd2_dev, &mut fs, &last_filename).expect("read_file failed");
        assert_eq!(data, large_data);

        umount(fs, &mut jbd2_dev).expect("full filesystem must still unmount cleanly");
    }

    /// A device whose size does not end on a full block-group boundary must
    /// not expose padding bits past `s_blocks_count` as allocatable blocks.
    #[test]
    fn test_partial_last_group_padding_is_not_allocated() {
        let blocks_per_group = 8 * rsext4::BLOCK_SIZE as u64;
        let total_blocks = blocks_per_group + 128;
        let device = ErrorMockDevice::new(total_blocks as usize * rsext4::BLOCK_SIZE);
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

        let last_group = BGIndex::new(fs.group_count - 1);
        let last_desc = fs
            .get_group_desc(last_group)
            .expect("last group descriptor should exist");
        assert!(last_desc.free_blocks_count() < 128);

        jbd2_dev
            .read_block(AbsoluteBN::new(last_desc.block_bitmap()))
            .expect("read last block bitmap");
        let bitmap = jbd2_dev.buffer();
        for bit in 128..fs.superblock.s_blocks_per_group as usize {
            assert!(
                bitmap[bit / 8] & (1 << (bit % 8)) != 0,
                "partial-group padding bit {bit} should be marked allocated"
            );
        }

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }
}
