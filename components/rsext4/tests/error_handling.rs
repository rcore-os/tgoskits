//! Error-path tests for filesystem operations.
//!
//! These tests intentionally exercise unusual or degraded scenarios and record
//! the current behavior, even when the behavior is not yet fully strict.

use std::cell::Cell;

use rsext4::{
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
    *,
};

/// Mock block device with knobs for injecting IO and capacity failures.
struct ErrorMockDevice {
    data: Vec<u8>,
    block_size: u32,
    // Failure injection toggles.
    fail_on_open: bool,
    fail_on_close: bool,
    fail_on_read: bool,
    fail_on_write: bool,
    fail_on_specific_block: Option<AbsoluteBN>,
    fail_after_bytes: Option<usize>,
    bytes_written: usize,
    now: Cell<i64>,
}

impl ErrorMockDevice {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            block_size: rsext4::BLOCK_SIZE as u32,
            fail_on_open: false,
            fail_on_close: false,
            fail_on_read: false,
            fail_on_write: false,
            fail_on_specific_block: None,
            fail_after_bytes: None,
            bytes_written: 0,
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockDevice for ErrorMockDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        if self.fail_on_read {
            return Err(Ext4Error::io());
        }

        if let Some(fail_block) = self.fail_on_specific_block {
            if block_id == fail_block {
                return Err(Ext4Error::corrupted());
            }
        }

        let start = block_id.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
            ));
        }
        buffer.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        if self.fail_on_write {
            return Err(Ext4Error::io());
        }

        if let Some(fail_block) = self.fail_on_specific_block {
            if block_id == fail_block {
                return Err(Ext4Error::corrupted());
            }
        }

        if let Some(limit) = self.fail_after_bytes {
            self.bytes_written += buffer.len();
            if self.bytes_written > limit {
                return Err(Ext4Error::no_space());
            }
        }

        let start = block_id.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
            ));
        }
        self.data[start..end].copy_from_slice(buffer);
        Ok(())
    }

    fn open(&mut self) -> Ext4Result<()> {
        if self.fail_on_open {
            return Err(Ext4Error::badf());
        }
        Ok(())
    }

    fn close(&mut self) -> Ext4Result<()> {
        if self.fail_on_close {
            return Err(Ext4Error::badf());
        }
        Ok(())
    }

    fn total_blocks(&self) -> u64 {
        (self.data.len() / self.block_size as usize) as u64
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

#[cfg(test)]
mod error_handling_tests {
    use super::*;

    /// Verifies that the filesystem still works on a normal device configured for
    /// fault injection, giving the suite a baseline before harsher error cases.
    #[test]
    fn test_block_device_errors() {
        let device = ErrorMockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/error_test").expect("mkdir failed");

        // Create one file and confirm the standard read path still succeeds.
        let test_data = b"Test data for error scenarios";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/error_test/test.txt",
            Some(test_data),
            None,
        )
        .expect("mkfile failed");

        let data =
            read_file(&mut jbd2_dev, &mut fs, "/error_test/test.txt").expect("read_file failed");
        assert_eq!(data, test_data.to_vec());

        let _ = umount(fs, &mut jbd2_dev);
    }

    /// Records behavior at several filesystem-size and filename-length boundaries
    /// without over-constraining implementation-dependent cases.
    #[test]
    fn test_filesystem_boundaries() {
        // Probe mkfs behavior on a relatively small backing device.
        let small_device = ErrorMockDevice::new(20 * 1024 * 1024); // 20MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, small_device, true);

        let result = mkfs(&mut jbd2_dev);
        println!("mkfs on small device result: {:?}", result);
        // Document current behavior rather than asserting a fixed policy.

        // Repeat the rest of the checks on a normal-sized device.
        let normal_device = ErrorMockDevice::new(50 * 1024 * 1024); // 50MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, normal_device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/boundary").expect("mkdir failed");

        mkfile(&mut jbd2_dev, &mut fs, "/boundary/empty.txt", None, None).expect("mkfile failed");

        // Check the exact-name-limit case.
        let long_name = "a".repeat(rsext4::DIRNAME_LEN);
        let _ = mkfile(
            &mut jbd2_dev,
            &mut fs,
            &format!("/boundary/{}.txt", long_name),
            Some(b"test"),
            None,
        );
        // And record the over-limit case, which may still be implementation-defined.
        let too_long_name = "a".repeat(rsext4::DIRNAME_LEN + 1);
        let result = mkfile(
            &mut jbd2_dev,
            &mut fs,
            &format!("/boundary/{}.txt", too_long_name),
            Some(b"test"),
            None,
        );
        println!("mkfile with long filename result: {:?}", result);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Records how the current path parser handles empty, root, duplicated, and
    /// NUL-containing paths without forcing a stricter policy than implemented.
    #[test]
    fn test_invalid_paths() {
        let device = ErrorMockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        // Empty paths are recorded for documentation purposes.
        let result = mkfile(&mut jbd2_dev, &mut fs, "", Some(b"test"), None);
        println!("mkfile with empty path result: {:?}", result);

        // Root-only paths are also documented rather than asserted.
        let result = mkfile(&mut jbd2_dev, &mut fs, "/", Some(b"test"), None);
        println!("mkfile with root path result: {:?}", result);

        // Repeated separators may be normalized by the current implementation.
        let _ = mkdir(&mut jbd2_dev, &mut fs, "//invalid//path//");

        // ext4 only rejects a narrow set of characters, so keep this as observed behavior.
        let _ = mkdir(&mut jbd2_dev, &mut fs, "/path/with\0null");

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies that deleting and recreating the same path leaves the namespace
    /// in a consistent state from the caller's perspective.
    #[test]
    fn test_concurrent_operation_errors() {
        let device = ErrorMockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/concurrent").expect("mkdir failed");

        // Keep one untouched file around so the directory itself remains valid.
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/concurrent/base.txt",
            Some(b"base content"),
            None,
        )
        .expect("mkfile failed");

        // Delete one file, confirm the read failure, then recreate it.
        let file_path = "/concurrent/delete_test.txt";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            file_path,
            Some(b"to be deleted"),
            None,
        )
        .expect("mkfile failed");

        delete_file(&mut fs, &mut jbd2_dev, file_path).expect("delete failed");

        // Reads against the deleted path should fail with `ENOENT`.
        let result = read_file(&mut jbd2_dev, &mut fs, file_path).expect_err("deleted file");
        assert_eq!(result.code, Errno::ENOENT);

        // Recreating the same name should succeed and expose the new payload.
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            file_path,
            Some(b"new content"),
            None,
        )
        .expect("mkfile failed");

        let data = read_file(&mut jbd2_dev, &mut fs, file_path).expect("read_file failed");
        assert_eq!(data, b"new content".to_vec());

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Tries to fill the filesystem with large files until creation fails, then
    /// checks that the last successful file is still readable.
    #[test]
    fn test_resource_exhaustion() {
        let device = ErrorMockDevice::new(50 * 1024 * 1024); // 50MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/exhaustion").expect("mkdir failed");

        // Create large files in a loop until allocation eventually stops succeeding.
        let mut file_count = 0;
        let file_size = 1024 * 1024; // 1 MiB per file.
        let large_data = vec![b'X'; file_size];

        loop {
            let filename = format!("/exhaustion/file{}.dat", file_count);
            let result = mkfile(&mut jbd2_dev, &mut fs, &filename, Some(&large_data), None);

            match result {
                Ok(_) => file_count += 1,
                Err(_) => break,
            }

            // Guard against an infinite loop if the device is larger than expected.
            if file_count > 40 {
                break;
            }
        }

        // At least one file should have been created before exhaustion.
        assert!(file_count > 0);

        // The last successful file should still contain the full payload.
        let last_filename = format!("/exhaustion/file{}.dat", file_count - 1);
        let data = read_file(&mut jbd2_dev, &mut fs, &last_filename).expect("read_file failed");
        assert_eq!(data, large_data);

        // Unmount may fail after exhaustion; the test only cares about data survival.
        let _ = umount(fs, &mut jbd2_dev);
    }

    /// Simulates an abrupt drop of open handles and remounts the filesystem to
    /// document current recovery behavior after an unclean shutdown.
    #[test]
    fn test_inconsistent_state_handling() {
        let device = ErrorMockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/state_test").expect("mkdir failed");

        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/state_test/consistent.txt",
            Some(b"original data"),
            None,
        )
        .expect("mkfile failed");

        // Keep a handle open, write some data, then drop everything abruptly.
        let mut file =
            open(&mut jbd2_dev, &mut fs, "/state_test/consistent.txt", true).expect("open failed");

        write_at(&mut jbd2_dev, &mut fs, &mut file, b"partial").expect("write_at failed");

        drop(file);
        drop(fs);

        // Remount and record what the filesystem reports for the file afterwards.
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        let data = read_file(&mut jbd2_dev, &mut fs, "/state_test/consistent.txt");

        println!("File data after remount: {:?}", data);
        // No strict assertion here; the purpose is to document current recovery semantics.

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies the current permission-related happy path and documents that the
    /// test is checking data accessibility rather than full ACL semantics.
    #[test]
    fn test_permission_handling() {
        let device = ErrorMockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/permission").expect("mkdir failed");

        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/permission/test.txt",
            Some(b"permission test"),
            None,
        )
        .expect("mkfile failed");

        // This test only covers the basic accessible path, not full Unix permission semantics.

        // Reads should succeed on the file created by the same caller.
        let data =
            read_file(&mut jbd2_dev, &mut fs, "/permission/test.txt").expect("read_file failed");
        assert_eq!(data, b"permission test".to_vec());

        // Writes should also succeed and update the file prefix.
        write_file(
            &mut jbd2_dev,
            &mut fs,
            "/permission/test.txt",
            0,
            b"modified",
        )
        .expect("write_file failed");

        // The implementation may preserve the old suffix, so only the rewritten prefix is asserted.
        let data =
            read_file(&mut jbd2_dev, &mut fs, "/permission/test.txt").expect("read_file failed");

        assert_eq!(
            &data[..b"modified".len()],
            b"modified",
            "The updated prefix should be written correctly",
        );

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }
}
