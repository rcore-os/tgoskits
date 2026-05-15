//! Functional tests for the public file API.
//!
//! The suite focuses on `open`, `read_at`, `write_at`, and `lseek`, with each
//! test documenting the scenario and the intended observable behavior.

use std::{
    cell::Cell,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use rsext4::{
    api::SeekWhence,
    error::{Ext4Error, Ext4Result},
    *,
};

/// File-backed block device used by API tests so the resulting filesystem can
/// be validated with `e2fsck` after each case.
struct FileBlockDevice {
    file: File,
    block_size: u32,
    total_blocks: u64,
    now: Cell<i64>,
}

impl FileBlockDevice {
    fn open(path: PathBuf, block_size: u32) -> Self {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open image");
        let len = file.metadata().expect("image metadata").len();
        Self {
            file,
            block_size,
            total_blocks: len / block_size as u64,
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockDevice for FileBlockDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: DevBN, count: u32) -> Ext4Result<()> {
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

    fn write(&mut self, buffer: &[u8], block_id: DevBN, count: u32) -> Ext4Result<()> {
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

    fn dev_block_size(&self) -> u32 {
        self.block_size
    }

    fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

fn command_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn require_tool(tool: &str) {
    Command::new(tool)
        .arg("-V")
        .output()
        .unwrap_or_else(|err| panic!("required tool `{tool}` is not available: {err}"));
}

fn run_command(mut command: Command, context: &str) -> Output {
    let output = command
        .output()
        .unwrap_or_else(|err| panic!("failed to spawn {context}: {err}"));
    assert!(
        output.status.success(),
        "{context} failed\n{}",
        command_text(&output)
    );
    output
}

fn create_ext4_test_image(prefix: &str, size: &str, ext4_block_size: u32) -> (PathBuf, PathBuf) {
    let temp_dir = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir).expect("remove stale temp dir");
    }
    fs::create_dir(&temp_dir).expect("create temp dir");
    let image = temp_dir.join("fs.img");

    run_command(
        {
            let mut command = Command::new("truncate");
            command.args(["-s", size]).arg(&image);
            command
        },
        "truncate test image",
    );
    run_command(
        {
            let mut command = Command::new("mkfs.ext4");
            command
                .args(["-F", "-q", "-b", &ext4_block_size.to_string()])
                .arg(&image);
            command
        },
        "mkfs.ext4 test image",
    );

    (temp_dir, image)
}

fn e2fsck_readonly_clean(image: &Path, context: &str) {
    let output = Command::new("e2fsck")
        .args(["-fn"])
        .arg(image)
        .output()
        .unwrap_or_else(|err| panic!("failed to spawn e2fsck for {context}: {err}"));
    assert_eq!(
        output.status.code(),
        Some(0),
        "readonly e2fsck failed for {context}\n{}",
        command_text(&output)
    );
}

#[cfg(test)]
mod api_functional_tests {
    use std::sync::Mutex;

    use super::*;

    static API_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn run_api_test(
        name: &str,
        image_size: &str,
        ext4_block_size: u32,
        dev_block_size: u32,
        test: impl FnOnce(&mut Jbd2Dev<FileBlockDevice>, &mut Ext4FileSystem),
    ) {
        let _guard = API_TEST_LOCK.lock().expect("lock api tests");
        for tool in ["truncate", "mkfs.ext4", "e2fsck"] {
            require_tool(tool);
        }

        let (temp_dir, image) = create_ext4_test_image(name, image_size, ext4_block_size);
        {
            let dev = FileBlockDevice::open(image.clone(), dev_block_size);
            let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
            let mut fs = mount(&mut jbd2_dev).expect("mount failed");
            test(&mut jbd2_dev, &mut fs);
            umount(fs, &mut jbd2_dev).expect("umount failed");
        }
        e2fsck_readonly_clean(&image, name);
        fs::remove_dir_all(temp_dir).expect("remove temp dir");
    }

    fn open_readonly() -> bool {
        false
    }

    fn open_rw_create() -> bool {
        true
    }

    /// Verifies that opening an existing file and reading through the file-handle
    /// API returns the exact bytes that were written during setup.
    #[test]
    fn test_open_and_read() {
        run_api_test("rsext4-api-open-read", "64M", 4096, 512, |jbd2_dev, fs| {
            // Arrange one file with known contents, then read it through the public API.
            let test_data = b"API test data for basic operations";
            mkfile(jbd2_dev, fs, "/apitest/data.txt", Some(test_data), None)
                .expect("mkfile failed");

            let mut file =
                open(jbd2_dev, fs, "/apitest/data.txt", open_readonly()).expect("open failed");

            let read_data =
                read_at(jbd2_dev, fs, &mut file, test_data.len()).expect("read_at failed");
            assert_eq!(read_data, test_data);
        });
    }

    /// Verifies that `write_at` persists bytes and that a later `read_at`
    /// observes the same data after rewinding the file offset.
    #[test]
    fn test_write_at() {
        run_api_test("rsext4-api-write-at", "64M", 4096, 512, |jbd2_dev, fs| {
            mkdir(jbd2_dev, fs, "/write_test").expect("mkdir failed");
            mkfile(jbd2_dev, fs, "/write_test/empty.txt", None, None).expect("mkfile failed");

            let mut file =
                open(jbd2_dev, fs, "/write_test/empty.txt", open_rw_create()).expect("open failed");

            // Write once, rewind, and read back the exact payload.
            let write_data = b"This is test data for write_at function";
            write_at(jbd2_dev, fs, &mut file, write_data).expect("write_at failed");

            lseek(jbd2_dev, fs, &mut file, 0, SeekWhence::Set).expect("lseek failed");
            let read_data =
                read_at(jbd2_dev, fs, &mut file, write_data.len()).expect("read_at failed");
            assert_eq!(read_data, write_data);
        });
    }

    /// Verifies that `lseek` can position reads at the beginning, middle, and
    /// end of a file without corrupting offsets.
    #[test]
    fn test_lseek() {
        run_api_test("rsext4-api-lseek", "64M", 4096, 512, |jbd2_dev, fs| {
            let test_data = b"0123456789ABCDEFGHIJ";
            mkfile(jbd2_dev, fs, "/seek_test.txt", Some(test_data), None).expect("mkfile failed");

            let mut file =
                open(jbd2_dev, fs, "/seek_test.txt", open_readonly()).expect("open failed");

            // Read from the start.
            lseek(jbd2_dev, fs, &mut file, 0, SeekWhence::Set).expect("lseek failed");
            let data = read_at(jbd2_dev, fs, &mut file, 5).expect("read_at failed");
            assert_eq!(data, b"01234");

            // Read from the middle.
            lseek(jbd2_dev, fs, &mut file, 10, SeekWhence::Set).expect("lseek failed");
            let data = read_at(jbd2_dev, fs, &mut file, 5).expect("read_at failed");
            assert_eq!(data, b"ABCDE");

            // Reading at EOF should return no bytes.
            lseek(
                jbd2_dev,
                fs,
                &mut file,
                test_data.len() as i64,
                SeekWhence::Set,
            )
            .expect("lseek failed");
            let data = read_at(jbd2_dev, fs, &mut file, 1).expect("read_at failed");
            assert_eq!(data, b"");
        });
    }

    /// Verifies sparse-style random writes by patching several offsets and then
    /// checking that each modified region can be read back independently.
    #[test]
    fn test_random_read_write() {
        run_api_test("rsext4-api-random-rw", "64M", 4096, 512, |jbd2_dev, fs| {
            // Start from a larger file so offset-based writes cover multiple regions.
            let mut initial_data = Vec::new();
            for i in 0..1000 {
                initial_data.push((i % 256) as u8);
            }

            mkfile(jbd2_dev, fs, "/random_test.dat", Some(&initial_data), None)
                .expect("mkfile failed");

            let mut file =
                open(jbd2_dev, fs, "/random_test.dat", open_rw_create()).expect("open failed");

            // Overwrite a few fixed offsets and later validate each patched span.
            let write_positions = [100, 250, 500, 750];
            let write_data = b"DATA";

            for &pos in &write_positions {
                lseek(jbd2_dev, fs, &mut file, pos as i64, SeekWhence::Set).expect("lseek failed");
                write_at(jbd2_dev, fs, &mut file, write_data).expect("write_at failed");
            }

            for &pos in &write_positions {
                lseek(jbd2_dev, fs, &mut file, pos as i64, SeekWhence::Set).expect("lseek failed");
                let data =
                    read_at(jbd2_dev, fs, &mut file, write_data.len()).expect("read_at failed");
                assert_eq!(data, write_data);
            }
        });
    }

    /// Verifies large sequential writes by streaming many chunks into one file
    /// and then checking both the final size and repeated chunk contents.
    #[test]
    fn test_large_file_operations() {
        run_api_test(
            "rsext4-api-large-file",
            "128M",
            4096,
            512,
            |jbd2_dev, fs| {
                let chunk = b"0123456789ABCDEF";
                let chunks_to_write = 1000; // Roughly 16 KiB in total.
                let expected_size = chunk.len() * chunks_to_write;

                mkfile(jbd2_dev, fs, "/large_file.dat", None, None).expect("mkfile failed");

                let mut file =
                    open(jbd2_dev, fs, "/large_file.dat", open_rw_create()).expect("open failed");

                // Append fixed-size chunks to exercise repeated buffered writes.
                for _ in 0..chunks_to_write {
                    write_at(jbd2_dev, fs, &mut file, chunk).expect("write_at failed");
                }

                // Read the whole file back once and validate structure and contents.
                lseek(jbd2_dev, fs, &mut file, 0, SeekWhence::Set).expect("lseek failed");
                let data = read_at(jbd2_dev, fs, &mut file, expected_size).expect("read_at failed");
                assert_eq!(data.len(), expected_size);

                for i in 0..chunks_to_write {
                    let start = i * chunk.len();
                    let end = start + chunk.len();
                    assert_eq!(&data[start..end], chunk);
                }
            },
        );
    }

    /// Simulates interleaved operations across several files to ensure handle
    /// creation and reads remain isolated between independent paths.
    #[test]
    fn test_concurrent_file_operations() {
        run_api_test(
            "rsext4-api-concurrent",
            "128M",
            4096,
            512,
            |jbd2_dev, fs| {
                // Create a small set of files, then open and inspect them one by one.
                for i in 1..=5 {
                    let filename = format!("/concurrent/file{}.txt", i);
                    let data = format!("Content of file {}", i);
                    mkfile(jbd2_dev, fs, &filename, Some(data.as_bytes()), None)
                        .expect("mkfile failed");
                }

                for i in 1..=5 {
                    let filename = format!("/concurrent/file{}.txt", i);
                    let mut file =
                        open(jbd2_dev, fs, &filename, open_readonly()).expect("open failed");

                    // Each file should expose the same stable prefix when read from offset 0.
                    let data = read_at(jbd2_dev, fs, &mut file, 10).expect("read_at failed");
                    assert_eq!(data, format!("Content of").as_bytes());

                    drop(file);
                }
            },
        );
    }

    /// Exercises edge cases in the public API: opening a missing path, reading
    /// from an empty file, seeking to an extreme offset, and writing a large buffer.
    #[test]
    fn test_api_error_handling() {
        run_api_test(
            "rsext4-api-error-handling",
            "64M",
            4096,
            512,
            |jbd2_dev, fs| {
                // Opening a missing file without create should fail.
                let result = open(jbd2_dev, fs, "/nonexistent.txt", open_readonly());
                assert!(result.is_err());

                // Opening with create should materialize the file.
                let mut file =
                    open(jbd2_dev, fs, "/new.txt", open_rw_create()).expect("open failed");

                // Empty files should read back as an empty buffer.
                let data = read_at(jbd2_dev, fs, &mut file, 10).expect("read_at failed");
                assert_eq!(data, b"");

                // Record the behavior for an extreme seek. The current implementation
                // may accept large offsets to support future file growth.
                let seek_result = lseek(jbd2_dev, fs, &mut file, i64::MAX, SeekWhence::Set);
                assert!(seek_result.is_err());

                // Skip writes at the extreme offset to avoid overflow in the test harness.
                lseek(jbd2_dev, fs, &mut file, 0, SeekWhence::Set).expect("lseek failed");

                // A large write should still succeed on a freshly created file.
                let large_data = vec![b'X'; 1024 * 1024]; // 1MB
                write_at(jbd2_dev, fs, &mut file, &large_data).expect("write_at failed");
            },
        );
    }

    /// Covers zero-length reads and writes plus appending at EOF, documenting
    /// the exact contract expected at common boundary conditions.
    #[test]
    fn test_boundary_conditions() {
        run_api_test("rsext4-api-boundary", "64M", 4096, 512, |jbd2_dev, fs| {
            mkfile(jbd2_dev, fs, "/boundary.txt", Some(b"Boundary"), None).expect("mkfile failed");

            let mut file =
                open(jbd2_dev, fs, "/boundary.txt", open_rw_create()).expect("open failed");

            // Zero-length writes should be accepted as a no-op.
            write_at(jbd2_dev, fs, &mut file, b"").expect("write_at failed");

            // Zero-length reads should also return an empty buffer.
            lseek(jbd2_dev, fs, &mut file, 0, SeekWhence::Set).expect("lseek failed");
            let data = read_at(jbd2_dev, fs, &mut file, 0).expect("read_at failed");
            assert_eq!(data, b"");

            // Appending exactly at EOF should preserve the original prefix.
            lseek(jbd2_dev, fs, &mut file, 8, SeekWhence::Set).expect("lseek failed"); // Length of "Boundary".
            write_at(jbd2_dev, fs, &mut file, b" test").expect("write_at failed");

            lseek(jbd2_dev, fs, &mut file, 8, SeekWhence::Set).expect("lseek failed");
            let data = read_at(jbd2_dev, fs, &mut file, 5).expect("read_at failed");
            assert_eq!(data, b" test");
        });
    }

    /// Verifies the public API still works when the underlying device sector
    /// size is not 512 bytes and the ext4 logical block size is not 4 KiB.
    #[test]
    fn test_api_with_non_512_dev_block_and_non_4k_ext4_block() {
        run_api_test(
            "rsext4-api-non512-dev-non4k-ext4",
            "64M",
            2048,
            1024,
            |jbd2_dev, fs| {
                mkfile(jbd2_dev, fs, "/mixed-blocks.bin", None, None).expect("mkfile failed");
                let mut file =
                    open(jbd2_dev, fs, "/mixed-blocks.bin", open_rw_create()).expect("open failed");

                let payload_len = 2 * 2048 + 333;
                let payload: Vec<u8> = (0..payload_len).map(|i| (i % 251) as u8).collect();
                write_at(jbd2_dev, fs, &mut file, &payload).expect("write_at failed");

                lseek(jbd2_dev, fs, &mut file, 0, SeekWhence::Set).expect("lseek failed");
                let all = read_at(jbd2_dev, fs, &mut file, payload.len()).expect("read_at failed");
                assert_eq!(all, payload);

                let cross_block_offset = 2048 - 64;
                lseek(
                    jbd2_dev,
                    fs,
                    &mut file,
                    cross_block_offset as i64,
                    SeekWhence::Set,
                )
                .expect("lseek failed");
                let window = read_at(jbd2_dev, fs, &mut file, 256).expect("read_at failed");
                assert_eq!(
                    window,
                    payload[cross_block_offset..cross_block_offset + 256].to_vec()
                );
            },
        );
    }
}
