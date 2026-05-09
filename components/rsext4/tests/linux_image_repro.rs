use std::{
    cell::Cell,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
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
        let file = OpenOptions::new()
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

fn command_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn e2fsck_status_ok(output: &Output, allow_fixed: bool) -> bool {
    match output.status.code() {
        Some(0) => true,
        Some(1) if allow_fixed => true,
        _ => false,
    }
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

fn run_debugfs_script(image: &Path, script: &str, context: &str) {
    let mut child = Command::new("debugfs")
        .arg("-w")
        .arg(image)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("failed to spawn debugfs for {context}: {err}"));

    {
        let mut stdin = child.stdin.take().expect("debugfs stdin");
        stdin
            .write_all(script.as_bytes())
            .unwrap_or_else(|err| panic!("failed to write debugfs script for {context}: {err}"));
    }

    let output = child
        .wait_with_output()
        .unwrap_or_else(|err| panic!("failed to wait for debugfs {context}: {err}"));
    assert!(
        output.status.success(),
        "debugfs {context} failed\n{}",
        command_text(&output)
    );
}

fn debugfs_query(image: &Path, request: &str) -> String {
    let output = run_command(
        {
            let mut command = Command::new("debugfs");
            command.args(["-R", request]).arg(image);
            command
        },
        &format!("debugfs -R {request}"),
    );
    command_text(&output)
}

fn e2fsck_readonly_clean(image: &Path, context: &str) {
    let output = Command::new("e2fsck")
        .args(["-fn"])
        .arg(image)
        .output()
        .unwrap_or_else(|err| panic!("failed to spawn e2fsck for {context}: {err}"));
    assert!(
        e2fsck_status_ok(&output, false),
        "e2fsck failed for {context}\n{}",
        command_text(&output)
    );
}

fn create_ext4_test_image(prefix: &str, size: &str) -> (PathBuf, PathBuf) {
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
            command.args(["-F", "-q", "-b", "4096"]).arg(&image);
            command
        },
        "mkfs.ext4 test image",
    );

    (temp_dir, image)
}

fn assert_debugfs_path_exists(image: &Path, path: &str) {
    let output = debugfs_query(image, &format!("stat {path}"));
    assert!(
        output.contains("Type: directory") || output.contains("Type: regular"),
        "debugfs did not find expected path {path}\n{output}"
    );
}

fn changed_image_blocks(before: &Path, after: &Path) -> Vec<u64> {
    let mut before = File::open(before).expect("open before image");
    let mut after = File::open(after).expect("open after image");
    let before_len = before.metadata().expect("before image metadata").len();
    let after_len = after.metadata().expect("after image metadata").len();
    assert_eq!(before_len, after_len, "image lengths should match");

    let mut before_block = vec![0u8; BLOCK_SIZE];
    let mut after_block = vec![0u8; BLOCK_SIZE];
    let mut changed = Vec::new();
    for block in 0..before_len / BLOCK_SIZE as u64 {
        before
            .read_exact(&mut before_block)
            .expect("read before image block");
        after
            .read_exact(&mut after_block)
            .expect("read after image block");
        if before_block != after_block {
            changed.push(block);
        }
    }
    changed
}

fn read_image_blocks(image: &Path, blocks: &[u64], output: &Path) {
    let mut image = File::open(image).expect("open image for block extraction");
    let mut payload = File::create(output).expect("create journal payload");
    let mut buffer = vec![0u8; BLOCK_SIZE];
    for &block in blocks {
        image
            .seek(SeekFrom::Start(block * BLOCK_SIZE as u64))
            .expect("seek image block");
        image.read_exact(&mut buffer).expect("read image block");
        payload.write_all(&buffer).expect("write payload block");
    }
    payload.sync_all().expect("sync journal payload");
}

fn inject_csum_v3_journal(image: &Path, target_blocks: &[u64], payload: &Path) {
    let blocks = target_blocks
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        "journal_open -c -v 3\njournal_write -b {blocks} {}\njournal_close\nquit\n",
        payload.display()
    );
    run_debugfs_script(image, &script, "inject csum-v3 journal");
}

fn repair_baseline_image(path: &PathBuf) {
    let probe = Command::new("e2fsck")
        .args(["-fn"])
        .arg(path)
        .output()
        .expect("probe e2fsck");
    let probe_text = command_text(&probe);

    if probe_text.contains("FEATURE_C12") {
        let output = Command::new("debugfs")
            .args(["-w", "-R", "feature ^FEATURE_C12"])
            .arg(path)
            .output()
            .expect("clear unsupported local test feature");
        assert!(
            output.status.success(),
            "debugfs failed while clearing FEATURE_C12\n{}",
            command_text(&output)
        );
    }

    let output = Command::new("e2fsck")
        .args(["-fy"])
        .arg(path)
        .output()
        .expect("repair baseline image");
    assert!(
        e2fsck_status_ok(&output, true),
        "baseline e2fsck repair failed\n{}",
        command_text(&output)
    );
}

#[test]
fn replay_csum_v3_multi_block_journal_from_debugfs() {
    for tool in ["mkfs.ext4", "debugfs", "e2fsck"] {
        require_tool(tool);
    }

    let temp_dir = std::env::temp_dir().join(format!(
        "rsext4-csum-v3-journal-repro-{}",
        std::process::id()
    ));
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir).expect("remove stale temp dir");
    }
    fs::create_dir(&temp_dir).expect("create temp dir");
    let image = temp_dir.join("fs.img");
    let mutated = temp_dir.join("mutated.img");
    let payload = temp_dir.join("journal-payload.bin");
    let baseline = temp_dir.join("baseline.img");

    run_command(
        {
            let mut command = Command::new("truncate");
            command.args(["-s", "64M"]).arg(&image);
            command
        },
        "truncate test image",
    );
    run_command(
        {
            let mut command = Command::new("mkfs.ext4");
            command.args(["-F", "-q", "-b", "4096"]).arg(&image);
            command
        },
        "mkfs.ext4 test image",
    );
    fs::copy(&image, &mutated).expect("copy mutation image");
    run_debugfs_script(
        &mutated,
        "mkdir /replay-repro\nmkdir /replay-repro/a\nmkdir /replay-repro/b\nquit\n",
        "create fixture directories",
    );
    e2fsck_readonly_clean(&mutated, "direct debugfs mutation");

    let changed_blocks = changed_image_blocks(&image, &mutated);
    assert!(
        changed_blocks.len() >= 2,
        "fixture should change multiple metadata blocks, got {changed_blocks:?}"
    );
    read_image_blocks(&mutated, &changed_blocks, &payload);
    inject_csum_v3_journal(&image, &changed_blocks, &payload);

    fs::copy(&image, &baseline).expect("copy baseline image");
    run_debugfs_script(
        &baseline,
        "journal_open\njournal_close\njournal_run\nquit\n",
        "baseline journal replay",
    );
    assert_debugfs_path_exists(&baseline, "/replay-repro/a");
    assert_debugfs_path_exists(&baseline, "/replay-repro/b");
    e2fsck_readonly_clean(&baseline, "debugfs journal replay baseline");

    {
        let dev = FileBlockDevice::open(image.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let fs = mount(&mut dev).expect("mount image with pending csum-v3 journal");
        umount(fs, &mut dev).expect("umount image after replay");
    }

    assert_debugfs_path_exists(&image, "/replay-repro/a");
    assert_debugfs_path_exists(&image, "/replay-repro/b");
    e2fsck_readonly_clean(&image, "rsext4 csum-v3 journal replay");
    fs::remove_dir_all(temp_dir).expect("remove temp dir");
}

#[test]
fn e2fsck_clean_after_sparse_extent_truncate_keeps_tree_blocks_counted() {
    for tool in ["mkfs.ext4", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_test_image("rsext4-sparse-truncate-repro", "64M");

    {
        let dev = FileBlockDevice::open(image.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = mount(&mut dev).expect("mount image");

        let path = "/extent-truncate.bin";
        mkfile(&mut dev, &mut fs, path, None, None).expect("create sparse file");
        for lbn in [0u64, 2, 4, 6, 8] {
            write_file(
                &mut dev,
                &mut fs,
                path,
                lbn * BLOCK_SIZE as u64,
                &[lbn as u8],
            )
            .expect("sparse write");
        }

        truncate(&mut dev, &mut fs, path, 9 * BLOCK_SIZE as u64).expect("truncate sparse file");
        umount(fs, &mut dev).expect("umount image");
    }

    e2fsck_readonly_clean(&image, "sparse extent truncate");
    fs::remove_dir_all(temp_dir).expect("remove temp dir");
}

#[test]
fn e2fsck_clean_after_deleting_split_extent_file_frees_tree_blocks() {
    for tool in ["mkfs.ext4", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_test_image("rsext4-split-extent-delete-repro", "64M");

    {
        let dev = FileBlockDevice::open(image.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = mount(&mut dev).expect("mount image");

        let path = "/extent-delete.bin";
        mkfile(&mut dev, &mut fs, path, None, None).expect("create sparse file");
        for lbn in [0u64, 2, 4, 6, 8] {
            write_file(
                &mut dev,
                &mut fs,
                path,
                lbn * BLOCK_SIZE as u64,
                &[0x80 | lbn as u8],
            )
            .expect("sparse write");
        }

        delete_file(&mut fs, &mut dev, path).expect("delete sparse file");
        umount(fs, &mut dev).expect("umount image");
    }

    e2fsck_readonly_clean(&image, "split extent delete");
    fs::remove_dir_all(temp_dir).expect("remove temp dir");
}

#[test]
fn e2fsck_clean_after_exact_32768_block_extent() {
    for tool in ["mkfs.ext4", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_test_image("rsext4-32768-extent-repro", "192M");

    {
        let dev = FileBlockDevice::open(image.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = mount(&mut dev).expect("mount image");

        let path = "/extent-32768.bin";
        mkfile(&mut dev, &mut fs, path, None, None).expect("create file");
        let block = vec![0x5a; BLOCK_SIZE];
        for lbn in 0..32768u64 {
            write_file(&mut dev, &mut fs, path, lbn * BLOCK_SIZE as u64, &block)
                .expect("write contiguous extent block");
        }

        let content = read_file(&mut dev, &mut fs, path).expect("read exact 32768-block file");
        assert_eq!(content.len(), 32768 * BLOCK_SIZE);
        assert_eq!(&content[..16], &[0x5a; 16]);
        assert_eq!(
            &content[32767 * BLOCK_SIZE..32767 * BLOCK_SIZE + 16],
            &[0x5a; 16]
        );

        umount(fs, &mut dev).expect("umount image");
    }

    e2fsck_readonly_clean(&image, "exact 32768-block extent");
    fs::remove_dir_all(temp_dir).expect("remove temp dir");
}

#[test]
#[ignore = "requires a Linux-created ext4 rootfs image"]
fn repro_linux_image_create_write_rename_then_e2fsck() {
    let src_from_env = std::env::var_os("RSEXT4_TEST_IMAGE").map(PathBuf::from);
    let src = src_from_env.clone().unwrap_or_else(|| {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .expect("repo root")
            .join("target/rootfs/rootfs-aarch64-debian.img")
    });

    if !src.exists() {
        if src_from_env.is_some() {
            panic!("test image does not exist: {}", src.display());
        }
        eprintln!("skip: default test image does not exist: {}", src.display());
        return;
    }

    let dst = std::env::temp_dir().join(format!(
        "rsext4-linux-image-repro-{}.img",
        std::process::id()
    ));
    fs::copy(&src, &dst).expect("copy test image");
    repair_baseline_image(&dst);

    {
        let dev = FileBlockDevice::open(dst.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = mount(&mut dev).expect("mount image");

        let probe = "/root/codex-fsck-probe";
        let _ = delete_dir(&mut fs, &mut dev, probe);
        mkdir(&mut dev, &mut fs, &format!("{probe}/sub")).expect("mkdir probe");
        mkfile(
            &mut dev,
            &mut fs,
            &format!("{probe}/sub/data.txt"),
            Some(b"line-0-starry-fsck-probe\n"),
            None,
        )
        .expect("create data");
        write_file(
            &mut dev,
            &mut fs,
            &format!("{probe}/sub/data.txt"),
            25,
            b"tail-starry-fsck-probe\n",
        )
        .expect("append data");
        rename(
            &mut dev,
            &mut fs,
            &format!("{probe}/sub/data.txt"),
            &format!("{probe}/data-renamed.txt"),
        )
        .expect("rename data");
        umount(fs, &mut dev).expect("umount image");
    }

    let output = Command::new("e2fsck")
        .args(["-fn"])
        .arg(&dst)
        .output()
        .expect("run e2fsck");
    assert!(
        e2fsck_status_ok(&output, false),
        "e2fsck failed\n{}",
        command_text(&output)
    );

    let _ = fs::remove_file(dst);
}
