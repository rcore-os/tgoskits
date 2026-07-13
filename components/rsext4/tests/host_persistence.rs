//! Host-side persistence test: rsext4 writes through FileBlockDevice.
//! If this passes (data persists on disk), the bug is in the StarryOS
//! block-device runtime, not in rsext4 itself.

use std::{
    cell::Cell,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    process::Command,
};

use rsext4::{
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
    *,
};

struct FileBlockDevice {
    file: File,
    block_size: u32,
    total_blocks: u64,
    now: Cell<i64>,
}

impl FileBlockDevice {
    fn open(path: PathBuf) -> Self {
        let file = File::options()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open image");
        let len = file.metadata().expect("image metadata").len();
        Self {
            file,
            block_size: BLOCK_SIZE as u32,
            total_blocks: len / BLOCK_SIZE as u64,
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockDevice for FileBlockDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
        let required = self.block_size as usize * count as usize;
        if buffer.len() < required {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required));
        }
        let start = block_id.raw() * self.block_size as u64;
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(|_| Ext4Error::io())?;
        self.file
            .read_exact(&mut buffer[..required])
            .map_err(|_| Ext4Error::io())
    }

    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
        let required = self.block_size as usize * count as usize;
        if buffer.len() < required {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required));
        }
        let start = block_id.raw() * self.block_size as u64;
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(|_| Ext4Error::io())?;
        self.file
            .write_all(&buffer[..required])
            .map_err(|_| Ext4Error::io())
    }

    fn open(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn close(&mut self) -> Ext4Result<()> {
        self.flush()
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.file.sync_all().map_err(|_| Ext4Error::io())
    }

    fn total_blocks(&self) -> u64 {
        self.total_blocks
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

fn write_test_pattern(path: &str, dev: &mut Jbd2Dev<FileBlockDevice>, fs: &mut Ext4FileSystem) {
    // Use the rsext4 public API to create and write a file
    mkfile(dev, fs, path, None, None).expect("create test file");

    let block_size = BLOCK_SIZE as u64;
    let num_blocks = 200u64;
    let mut block = vec![0u8; BLOCK_SIZE];

    for blk in 0..num_blocks {
        // Per-block pattern: first 4 bytes = block number, rest = ramp
        block[0] = (blk >> 24) as u8;
        block[1] = (blk >> 16) as u8;
        block[2] = (blk >> 8) as u8;
        block[3] = blk as u8;
        for i in 4..BLOCK_SIZE {
            block[i] = (i & 0xFF) as u8;
        }
        write_file(dev, fs, path, blk * block_size, &block)
            .unwrap_or_else(|e| panic!("write block {blk} failed: {e:?}"));
    }

    println!("Wrote {num_blocks} blocks to {path}");
}

fn verify_test_pattern(path: &str, dev: &mut Jbd2Dev<FileBlockDevice>, fs: &mut Ext4FileSystem) {
    let data = read_file(dev, fs, path).expect("read test file");
    assert_eq!(data.len(), 200 * BLOCK_SIZE, "wrong file size");

    let mut errors = 0;
    for blk in 0..200u64 {
        let offset = (blk as usize) * BLOCK_SIZE;
        let actual = &data[offset..offset + BLOCK_SIZE];

        // Check first 4 bytes (block number)
        let expected_b0 = (blk >> 24) as u8;
        let expected_b1 = (blk >> 16) as u8;
        let expected_b2 = (blk >> 8) as u8;
        let expected_b3 = blk as u8;
        if actual[0] != expected_b0
            || actual[1] != expected_b1
            || actual[2] != expected_b2
            || actual[3] != expected_b3
        {
            println!(
                "CORRUPT block {blk}: expected [{expected_b0:02X} {expected_b1:02X} \
                 {expected_b2:02X} {expected_b3:02X}], got [{:02X} {:02X} {:02X} {:02X}]",
                actual[0], actual[1], actual[2], actual[3]
            );
            errors += 1;
            if errors >= 10 {
                panic!("too many errors");
            }
        }

        // Check ramp pattern
        for i in 4..BLOCK_SIZE {
            if actual[i] != (i & 0xFF) as u8 {
                println!(
                    "CORRUPT block {blk} byte {i}: expected {:02X}, got {:02X}",
                    (i & 0xFF) as u8,
                    actual[i]
                );
                errors += 1;
                if errors >= 10 {
                    panic!("too many errors at block {blk}");
                }
                break;
            }
        }
    }

    if errors == 0 {
        println!("VERIFIED: all 200 blocks correct");
    } else {
        panic!("{errors} blocks corrupted");
    }
}

#[test]
fn host_persistence_test() {
    // Use the pristine Alpine base image
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .join("tmp/axbuild/rootfs/rootfs-x86_64-alpine.img/rootfs-x86_64-alpine.img");
    if !src.exists() {
        eprintln!("SKIP: Alpine image not found at {}", src.display());
        return;
    }

    // Clone to a test image
    let dst = std::env::temp_dir().join(format!("rsext4-host-persist-{}.img", std::process::id()));
    fs::copy(&src, &dst).expect("copy image");

    let path = "/tmp/rsext4-host-test.bin";

    // Phase 1: Write
    {
        let dev = FileBlockDevice::open(dst.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = mount(&mut dev).expect("mount");

        write_test_pattern(path, &mut dev, &mut fs);
        verify_test_pattern(path, &mut dev, &mut fs);

        umount(fs, &mut dev).expect("umount");
    }

    // Phase 2: Remount and verify
    {
        let dev = FileBlockDevice::open(dst.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = mount(&mut dev).expect("remount");

        verify_test_pattern(path, &mut dev, &mut fs);

        umount(fs, &mut dev).expect("umount 2");
    }

    // Phase 3: e2fsck
    let output = Command::new("e2fsck")
        .args(["-fn"])
        .arg(&dst)
        .output()
        .expect("run e2fsck");
    let out_str = String::from_utf8_lossy(&output.stdout);
    let err_str = String::from_utf8_lossy(&output.stderr);
    println!("e2fsck: {}", output.status);
    if !output.status.success() {
        println!("e2fsck stdout:\n{out_str}");
        println!("e2fsck stderr:\n{err_str}");
        panic!("e2fsck failed!");
    }

    println!("HOST PERSISTENCE TEST PASSED");
    println!("Test image: {}", dst.display());
}
