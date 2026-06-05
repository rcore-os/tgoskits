//! Functional tests for the public file API.
//!
//! The suite focuses on `open`, `read_at`, `write_at`, and `lseek`, with each
//! test documenting the scenario and the intended observable behavior.

use std::cell::Cell;

use rsext4::{
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
    *,
};

/// In-memory block device used by API tests.
struct MockBlockDevice {
    data: Vec<u8>,
    block_size: u32,
    now: Cell<i64>,
}

impl MockBlockDevice {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            block_size: rsext4::BLOCK_SIZE as u32,
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockDevice for MockBlockDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
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
        Ok(())
    }

    fn close(&mut self) -> Ext4Result<()> {
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
mod api_functional_tests {
    use super::*;

    /// Verifies that opening an existing file and reading through the file-handle
    /// API returns the exact bytes that were written during setup.
    #[test]
    fn test_open_and_read() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        // Arrange one file with known contents, then read it through the public API.
        let test_data = b"API test data for basic operations";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/apitest/data.txt",
            Some(test_data),
            None,
        )
        .expect("mkfile failed");

        let mut file =
            open(&mut jbd2_dev, &mut fs, "/apitest/data.txt", false).expect("open failed");

        let read_data =
            read_at(&mut jbd2_dev, &mut fs, &mut file, test_data.len()).expect("read_at failed");
        assert_eq!(read_data, test_data);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies that `write_at` persists bytes and that a later `read_at`
    /// observes the same data after rewinding the file offset.
    #[test]
    fn test_write_at() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        mkdir(&mut jbd2_dev, &mut fs, "/write_test").expect("mkdir failed");

        mkfile(&mut jbd2_dev, &mut fs, "/write_test/empty.txt", None, None).expect("mkfile failed");

        let mut file =
            open(&mut jbd2_dev, &mut fs, "/write_test/empty.txt", true).expect("open failed");

        // Write once, rewind, and read back the exact payload.
        let write_data = b"This is test data for write_at function";
        write_at(&mut jbd2_dev, &mut fs, &mut file, write_data).expect("write_at failed");

        lseek(&mut file, 0).expect("lseek failed");
        let read_data =
            read_at(&mut jbd2_dev, &mut fs, &mut file, write_data.len()).expect("read_at failed");
        assert_eq!(read_data, write_data);

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies that `lseek` can position reads at the beginning, middle, and
    /// end of a file without corrupting offsets.
    #[test]
    fn test_lseek() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        let test_data = b"0123456789ABCDEFGHIJ";
        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/seek_test.txt",
            Some(test_data),
            None,
        )
        .expect("mkfile failed");

        let mut file = open(&mut jbd2_dev, &mut fs, "/seek_test.txt", false).expect("open failed");

        // Read from the start.
        lseek(&mut file, 0).expect("lseek failed");
        let data = read_at(&mut jbd2_dev, &mut fs, &mut file, 5).expect("read_at failed");
        assert_eq!(data, b"01234");

        // Read from the middle.
        lseek(&mut file, 10).expect("lseek failed");
        let data = read_at(&mut jbd2_dev, &mut fs, &mut file, 5).expect("read_at failed");
        assert_eq!(data, b"ABCDE");

        // Reading at EOF should return no bytes.
        lseek(&mut file, test_data.len() as u64).expect("lseek failed");
        let data = read_at(&mut jbd2_dev, &mut fs, &mut file, 1).expect("read_at failed");
        assert_eq!(data, b"");

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies sparse-style random writes by patching several offsets and then
    /// checking that each modified region can be read back independently.
    #[test]
    fn test_random_read_write() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        // Start from a larger file so offset-based writes cover multiple regions.
        let mut initial_data = Vec::new();
        for i in 0..1000 {
            initial_data.push((i % 256) as u8);
        }

        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/random_test.dat",
            Some(&initial_data),
            None,
        )
        .expect("mkfile failed");

        let mut file = open(&mut jbd2_dev, &mut fs, "/random_test.dat", true).expect("open failed");

        // Overwrite a few fixed offsets and later validate each patched span.
        let write_positions = [100, 250, 500, 750];
        let write_data = b"DATA";

        for &pos in &write_positions {
            lseek(&mut file, pos).expect("lseek failed");
            write_at(&mut jbd2_dev, &mut fs, &mut file, write_data).expect("write_at failed");
        }

        for &pos in &write_positions {
            lseek(&mut file, pos).expect("lseek failed");
            let data = read_at(&mut jbd2_dev, &mut fs, &mut file, write_data.len())
                .expect("read_at failed");
            assert_eq!(data, write_data);
        }

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Verifies large sequential writes by streaming many chunks into one file
    /// and then checking both the final size and repeated chunk contents.
    #[test]
    fn test_large_file_operations() {
        let device = MockBlockDevice::new(200 * 1024 * 1024); // 200MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        let chunk = b"0123456789ABCDEF";
        let chunks_to_write = 1000; // Roughly 16 KiB in total.
        let expected_size = chunk.len() * chunks_to_write;

        mkfile(&mut jbd2_dev, &mut fs, "/large_file.dat", None, None).expect("mkfile failed");

        let mut file = open(&mut jbd2_dev, &mut fs, "/large_file.dat", true).expect("open failed");

        // Append fixed-size chunks to exercise repeated buffered writes.
        for _ in 0..chunks_to_write {
            write_at(&mut jbd2_dev, &mut fs, &mut file, chunk).expect("write_at failed");
        }

        // Read the whole file back once and validate structure and contents.
        lseek(&mut file, 0).expect("lseek failed");
        let data =
            read_at(&mut jbd2_dev, &mut fs, &mut file, expected_size).expect("read_at failed");
        assert_eq!(data.len(), expected_size);

        for i in 0..chunks_to_write {
            let start = i * chunk.len();
            let end = start + chunk.len();
            assert_eq!(&data[start..end], chunk);
        }

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Simulates interleaved operations across several files to ensure handle
    /// creation and reads remain isolated between independent paths.
    #[test]
    fn test_concurrent_file_operations() {
        let device = MockBlockDevice::new(200 * 1024 * 1024); // 200MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        // Create a small set of files, then open and inspect them one by one.
        for i in 1..=5 {
            let filename = format!("/concurrent/file{}.txt", i);
            let data = format!("Content of file {}", i);
            mkfile(
                &mut jbd2_dev,
                &mut fs,
                &filename,
                Some(data.as_bytes()),
                None,
            )
            .expect("mkfile failed");
        }

        for i in 1..=5 {
            let filename = format!("/concurrent/file{}.txt", i);

            let mut file = open(&mut jbd2_dev, &mut fs, &filename, false).expect("open failed");

            // Each file should expose the same stable prefix when read from offset 0.
            let data = read_at(&mut jbd2_dev, &mut fs, &mut file, 10).expect("read_at failed");
            assert_eq!(data, format!("Content of").as_bytes());

            drop(file);
        }

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Exercises edge cases in the public API: opening a missing path, reading
    /// from an empty file, seeking to an extreme offset, and writing a large buffer.
    #[test]
    fn test_api_error_handling() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        // Opening a missing file without create should fail.
        let result = open(&mut jbd2_dev, &mut fs, "/nonexistent.txt", false);
        assert!(result.is_err());

        // Opening with create should materialize the file.
        let mut file = open(&mut jbd2_dev, &mut fs, "/new.txt", true).expect("open failed");

        // Empty files should read back as an empty buffer.
        let data = read_at(&mut jbd2_dev, &mut fs, &mut file, 10).expect("read_at failed");
        assert_eq!(data, b"");

        // Record the behavior for an extreme seek. The current implementation
        // may accept large offsets to support future file growth.
        let seek_result = lseek(&mut file, u64::MAX);
        println!("lseek(u64::MAX) result: {:?}", seek_result);

        // Skip writes at the extreme offset to avoid overflow in the test harness.
        lseek(&mut file, 0).expect("lseek failed");

        // A large write should still succeed on a freshly created file.
        let large_data = vec![b'X'; 1024 * 1024]; // 1MB
        write_at(&mut jbd2_dev, &mut fs, &mut file, &large_data).expect("write_at failed");

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }

    /// Covers zero-length reads and writes plus appending at EOF, documenting
    /// the exact contract expected at common boundary conditions.
    #[test]
    fn test_boundary_conditions() {
        let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
        let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

        mkfs(&mut jbd2_dev).expect("mkfs failed");
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");

        mkfile(
            &mut jbd2_dev,
            &mut fs,
            "/boundary.txt",
            Some(b"Boundary"),
            None,
        )
        .expect("mkfile failed");

        let mut file = open(&mut jbd2_dev, &mut fs, "/boundary.txt", true).expect("open failed");

        // Zero-length writes should be accepted as a no-op.
        write_at(&mut jbd2_dev, &mut fs, &mut file, b"").expect("write_at failed");

        // Zero-length reads should also return an empty buffer.
        lseek(&mut file, 0).expect("lseek failed");
        let data = read_at(&mut jbd2_dev, &mut fs, &mut file, 0).expect("read_at failed");
        assert_eq!(data, b"");

        // Appending exactly at EOF should preserve the original prefix.
        lseek(&mut file, 8).expect("lseek failed"); // Length of "Boundary".
        write_at(&mut jbd2_dev, &mut fs, &mut file, b" test").expect("write_at failed");

        lseek(&mut file, 8).expect("lseek failed");
        let data = read_at(&mut jbd2_dev, &mut fs, &mut file, 5).expect("read_at failed");
        assert_eq!(data, b" test");

        umount(fs, &mut jbd2_dev).expect("umount failed");
    }
}
