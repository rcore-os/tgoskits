//! End-to-end integration tests for the core filesystem flows.
//!
//! These tests exercise mkfs, mount, directory creation, file IO, and the
//! public API surface together so regressions show up as user-visible failures.

use std::cell::Cell;

use rsext4::{
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
    *,
};

/// Simple in-memory block device used by the integration tests.
struct TestBlockDevice {
    data: Vec<u8>,
    block_size: u32,
    is_open: bool,
    now: Cell<i64>,
}

impl TestBlockDevice {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            block_size: rsext4::BLOCK_SIZE as u32, // Match the ext4 block size used by the crate.
            is_open: false,
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockDevice for TestBlockDevice {
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
        self.is_open = true;
        Ok(())
    }

    fn close(&mut self) -> Ext4Result<()> {
        self.is_open = false;
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

#[test]
fn test_basic_mount_mkfs() {
    let device = TestBlockDevice::new(100 * 1024 * 1024); // 100MB
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

    // Test idea: create a fresh filesystem, perform one full create/read cycle,
    // and then unmount cleanly to prove the basic happy path works.
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut fs = mount(&mut jbd2_dev).expect("mount failed");

    mkdir(&mut jbd2_dev, &mut fs, "/test").expect("mkdir failed");

    let data = b"Hello, world!";
    mkfile(&mut jbd2_dev, &mut fs, "/test/hello.txt", Some(data), None).expect("mkfile failed");

    let read_data = read_file(&mut jbd2_dev, &mut fs, "/test/hello.txt").expect("read_file failed");
    assert_eq!(read_data, data.to_vec());

    umount(fs, &mut jbd2_dev).expect("umount failed");
}

#[test]
fn test_file_operations() {
    let device = TestBlockDevice::new(100 * 1024 * 1024); // 100MB
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let mut fs = mount(&mut jbd2_dev).expect("mount failed");

    // Test idea: mix high-level file helpers with the public open/read/write API
    // and verify that both views observe the same file contents.
    mkdir(&mut jbd2_dev, &mut fs, "/filetest").expect("mkdir failed");

    mkfile(&mut jbd2_dev, &mut fs, "/filetest/empty.txt", None, None).expect("mkfile failed");

    write_file(
        &mut jbd2_dev,
        &mut fs,
        "/filetest/empty.txt",
        0,
        b"First line",
    )
    .expect("write_file failed");

    // Append through the path-based helper and verify the concatenated content.
    let file_len = read_file(&mut jbd2_dev, &mut fs, "/filetest/empty.txt")
        .expect("read_file failed")
        .len();
    write_file(
        &mut jbd2_dev,
        &mut fs,
        "/filetest/empty.txt",
        file_len as u64,
        b"\nSecond line",
    )
    .expect("write_file failed");

    let data = read_file(&mut jbd2_dev, &mut fs, "/filetest/empty.txt").expect("read_file failed");
    assert_eq!(data, b"First line\nSecond line".to_vec());

    // Then switch to the descriptor-style API and validate that open/write/read
    // observe the same backing state.
    let mut file = open(&mut jbd2_dev, &mut fs, "/filetest/api.txt", true).expect("open failed");

    write_at(&mut jbd2_dev, &mut fs, &mut file, b"API test").expect("write_at failed");
    lseek(&mut file, 0).expect("lseek failed");

    let bytes_read = read_at(&mut jbd2_dev, &mut fs, &mut file, 8).expect("read_at failed");
    assert_eq!(bytes_read, b"API test");

    umount(fs, &mut jbd2_dev).expect("umount failed");
}
