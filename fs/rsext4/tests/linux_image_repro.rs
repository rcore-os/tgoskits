use std::{
    cell::Cell,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use rsext4::{
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
        Self::open_with_sector_size(path, BLOCK_SIZE as u32)
    }

    fn open_with_sector_size(path: PathBuf, sector_size: u32) -> Self {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open image");
        let len = file.metadata().expect("image metadata").len();
        assert_eq!(
            len % u64::from(sector_size),
            0,
            "image length must be aligned to the device sector size"
        );
        Self {
            file,
            block_size: sector_size,
            total_blocks: len / u64::from(sector_size),
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockIo for FileBlockDevice {
    fn read(&mut self, buffer: &mut [u8], sector: rsext4::SectorId, count: u32) -> Ext4Result<()> {
        let required = self.block_size as usize * count as usize;
        if buffer.len() < required {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required));
        }
        let start = sector.raw() * self.block_size as u64;
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(|_| Ext4Error::io())?;
        self.file
            .read_exact(&mut buffer[..required])
            .map_err(|_| Ext4Error::io())
    }

    fn write(&mut self, buffer: &[u8], sector: rsext4::SectorId, count: u32) -> Ext4Result<()> {
        let required = self.block_size as usize * count as usize;
        if buffer.len() < required {
            return Err(Ext4Error::buffer_too_small(buffer.len(), required));
        }
        let start = sector.raw() * self.block_size as u64;
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(|_| Ext4Error::io())?;
        self.file
            .write_all(&buffer[..required])
            .map_err(|_| Ext4Error::io())
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.file.sync_all().map_err(|_| Ext4Error::io())
    }

    fn geometry(&self) -> rsext4::DeviceGeometry {
        rsext4::DeviceGeometry::new(self.block_size, self.total_blocks)
    }

    fn capabilities(&self) -> rsext4::DeviceCapabilities {
        rsext4::DeviceCapabilities {
            read_only: { false },

            flush: true,

            ..rsext4::DeviceCapabilities::default()
        }
    }
}

impl rsext4::Clock for FileBlockDevice {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

struct TestClock(Cell<i64>);

impl Clock for TestClock {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        let seconds = self.0.get();
        self.0.set(seconds + 1);
        Ok(Ext4Timestamp::new(seconds, 0))
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

fn create_ext4_geometry_image(
    prefix: &str,
    size: &str,
    filesystem_block_size: u32,
) -> (PathBuf, PathBuf) {
    let temp_dir = std::env::temp_dir().join(format!(
        "{prefix}-{filesystem_block_size}-{}",
        std::process::id()
    ));
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
        "truncate geometry test image",
    );
    run_command(
        {
            let mut command = Command::new("mkfs.ext4");
            command
                .args(["-F", "-q", "-b", &filesystem_block_size.to_string()])
                .arg(&image);
            command
        },
        "mkfs.ext4 geometry test image",
    );

    (temp_dir, image)
}

fn linux_image_geometry_round_trip(filesystem_block_size: u32) {
    for tool in ["mkfs.ext4", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_geometry_image(
        "rsext4-dynamic-block-geometry",
        "64M",
        filesystem_block_size,
    );
    let payload = vec![0x5a; filesystem_block_size as usize + 37];

    {
        let dev = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = mount(&mut dev).expect("mount Linux-created geometry image");
        assert!(fs.superblock.has_feature_ro_compat(
            rsext4::superblock::Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE,
        ));
        assert!(fs.superblock.has_feature_ro_compat(
            rsext4::superblock::Ext4Superblock::EXT4_FEATURE_RO_COMPAT_DIR_NLINK,
        ));

        mkdir(&mut dev, &mut fs, "/geometry").expect("create geometry directory");
        mkfile(&mut dev, &mut fs, "/geometry/source.bin", None, None)
            .expect("create geometry file");
        write_file(&mut dev, &mut fs, "/geometry/source.bin", 0, &payload)
            .expect("write across a filesystem block boundary");
        let _ = rename(
            &mut dev,
            &mut fs,
            "/geometry/source.bin",
            "/geometry/renamed.bin",
        )
        .expect("rename geometry file");
        assert_eq!(
            read_file(&mut dev, &mut fs, "/geometry/renamed.bin")
                .expect("read renamed geometry file"),
            payload
        );

        umount(fs, &mut dev).expect("umount geometry image");
    }

    e2fsck_readonly_clean(
        &image,
        &format!("{filesystem_block_size}-byte filesystem block round trip"),
    );
    fs::remove_dir_all(temp_dir).expect("remove temp dir");
}

fn rsext4_mkfs_geometry_round_trip(filesystem_block_size: u32) {
    for tool in ["e2fsck", "truncate"] {
        require_tool(tool);
    }

    let temp_dir = std::env::temp_dir().join(format!(
        "rsext4-mkfs-dynamic-geometry-{filesystem_block_size}-{}",
        std::process::id()
    ));
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir).expect("remove stale temp dir");
    }
    fs::create_dir(&temp_dir).expect("create temp dir");
    let image = temp_dir.join("fs.img");
    run_command(
        {
            let mut command = Command::new("truncate");
            command.args(["-s", "64M"]).arg(&image);
            command
        },
        "truncate rsext4 mkfs image",
    );

    {
        let dev = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, false);
        mkfs_with_options(
            &mut dev,
            MkfsOptions {
                block_size: filesystem_block_size,
                ..MkfsOptions::default()
            },
        )
        .expect("format image with dynamic filesystem geometry");
    }

    e2fsck_readonly_clean(
        &image,
        &format!("rsext4 mkfs {filesystem_block_size}-byte geometry"),
    );

    {
        let dev = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = mount(&mut dev).expect("mount rsext4-created geometry image");
        mkfile(&mut dev, &mut fs, "/mkfs-geometry.bin", None, None)
            .expect("create file on rsext4-created image");
        let payload = vec![0xa5; filesystem_block_size as usize + 19];
        write_file(&mut dev, &mut fs, "/mkfs-geometry.bin", 0, &payload)
            .expect("write dynamic mkfs payload");
        assert_eq!(
            read_file(&mut dev, &mut fs, "/mkfs-geometry.bin").expect("read dynamic mkfs payload"),
            payload
        );
        umount(fs, &mut dev).expect("unmount rsext4-created image");
    }

    e2fsck_readonly_clean(
        &image,
        &format!("rsext4 mkfs {filesystem_block_size}-byte round trip"),
    );
    fs::remove_dir_all(temp_dir).expect("remove temp dir");
}

#[test]
fn linux_image_round_trip_with_1k_filesystem_blocks() {
    linux_image_geometry_round_trip(1024);
}

#[test]
fn linux_image_round_trip_with_2k_filesystem_blocks() {
    linux_image_geometry_round_trip(2048);
}

#[test]
fn linux_image_round_trip_with_4k_filesystem_blocks() {
    linux_image_geometry_round_trip(4096);
}

#[test]
fn rsext4_mkfs_round_trip_with_1k_filesystem_blocks() {
    rsext4_mkfs_geometry_round_trip(1024);
}

#[test]
fn rsext4_mkfs_round_trip_with_2k_filesystem_blocks() {
    rsext4_mkfs_geometry_round_trip(2048);
}

#[test]
fn rsext4_mkfs_round_trip_with_4k_filesystem_blocks() {
    rsext4_mkfs_geometry_round_trip(4096);
}

#[test]
fn rsext4_special_device_is_linux_readable() {
    for tool in ["mkfs.ext4", "debugfs", "e2fsck"] {
        require_tool(tool);
    }
    let (temp_dir, image) = create_ext4_test_image("rsext4-special-device", "64M");
    let expected_device = DeviceNumber::new(259, 511).expect("valid modern device number");

    {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let services = MountServices::new(
            TestClock(Cell::new(1_800_000_000)),
            (),
            (),
            (),
            NoopObserver,
        );
        let mut filesystem =
            Ext4::mount(device, services, MountOptions::read_write()).expect("mount Linux image");
        filesystem
            .create_special_inode(
                MutationContext::new(1000, 1001, 0, 0),
                filesystem.root_inode(),
                FileName::new(b"modern-device").expect("valid raw name"),
                FilePermissions::new(0o600).expect("valid permissions"),
                SpecialInodeKind::CharacterDevice(expected_device),
            )
            .expect("create special inode");
        filesystem.unmount().expect("unmount special-device image");
    }

    e2fsck_readonly_clean(&image, "rsext4 special device");
    let stat = debugfs_query(&image, "stat /modern-device");
    assert!(stat.contains("Type: character special"), "{stat}");
    assert!(
        stat.contains("Device major/minor number: 259:511"),
        "Linux did not decode the expected modern device number\n{stat}"
    );
    fs::remove_dir_all(temp_dir).expect("remove temp dir");
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

fn dumpe2fs_header(image: &Path, context: &str) -> String {
    let output = Command::new("dumpe2fs")
        .arg("-h")
        .arg(image)
        .output()
        .unwrap_or_else(|err| panic!("failed to spawn dumpe2fs for {context}: {err}"));
    assert!(
        output.status.success(),
        "dumpe2fs failed for {context}\n{}",
        command_text(&output)
    );
    command_text(&output)
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
    let dirty_header = dumpe2fs_header(&image, "pending journal fixture");
    assert!(
        dirty_header.contains("needs_recovery"),
        "debugfs journal fixture should require recovery\n{dirty_header}"
    );

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
    let recovered_header = dumpe2fs_header(&image, "rsext4 journal replay");
    assert!(
        !recovered_header.contains("needs_recovery"),
        "rsext4 should clear needs_recovery after successful replay\n{recovered_header}"
    );
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

fn sparse_growth_round_trip(filesystem_block_size: u32) {
    for tool in ["mkfs.ext4", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) =
        create_ext4_geometry_image("rsext4-sparse-grow", "64M", filesystem_block_size);
    let extent_path = "/extent-sparse-grow.bin";
    let legacy_path = "/legacy-sparse-grow.bin";
    let extent_size = 20 * u64::from(filesystem_block_size);
    let legacy_size = 14 * u64::from(filesystem_block_size);

    {
        let dev = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = mount(&mut dev).expect("mount image");

        mkfile(&mut dev, &mut fs, extent_path, None, None).expect("create extent sparse file");
        truncate(&mut dev, &mut fs, extent_path, extent_size).expect("grow extent sparse file");

        mkfile(&mut dev, &mut fs, legacy_path, None, None).expect("create legacy sparse file");
        let legacy_inode = dir::get_inode_with_num(&mut fs, &mut dev, legacy_path)
            .expect("lookup legacy sparse file")
            .expect("legacy sparse file missing")
            .0;
        fs.modify_inode(&mut dev, legacy_inode, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
            inode.i_blocks_lo = 0;
            inode.l_i_blocks_high = 0;
        })
        .expect("convert sparse file to legacy mapping");
        truncate(&mut dev, &mut fs, legacy_path, legacy_size).expect("grow legacy sparse file");

        umount(fs, &mut dev).expect("umount sparse growth image");
    }

    e2fsck_readonly_clean(&image, "sparse growth");

    {
        let dev = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = mount(&mut dev).expect("remount sparse growth image");
        for (path, expected_size) in [(extent_path, extent_size), (legacy_path, legacy_size)] {
            let data = read_file(&mut dev, &mut fs, path).expect("read remounted sparse file");
            assert_eq!(data.len(), expected_size as usize);
            assert!(data.iter().all(|&byte| byte == 0));
            let (_, inode) = dir::get_inode_with_num(&mut fs, &mut dev, path)
                .expect("lookup remounted sparse file")
                .expect("remounted sparse file missing");
            assert_eq!(inode.i_blocks_lo, 0);
            assert_eq!(inode.l_i_blocks_high, 0);
        }
        umount(fs, &mut dev).expect("umount remounted sparse growth image");
    }

    e2fsck_readonly_clean(&image, "remounted sparse growth");
    fs::remove_dir_all(temp_dir).expect("remove temp dir");
}

#[test]
fn sparse_growth_round_trip_with_1k_filesystem_blocks() {
    sparse_growth_round_trip(1024);
}

#[test]
fn sparse_growth_round_trip_with_2k_filesystem_blocks() {
    sparse_growth_round_trip(2048);
}

#[test]
fn sparse_growth_round_trip_with_4k_filesystem_blocks() {
    sparse_growth_round_trip(4096);
}

#[test]
fn legacy_write_crosses_direct_single_boundary_and_remounts_cleanly() {
    for tool in ["mkfs.ext4", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_test_image("rsext4-legacy-single-write", "64M");
    let path = "/legacy-single.bin";
    let mut expected = vec![0u8; 14 * BLOCK_SIZE];
    for (index, byte) in expected.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(17).wrapping_add(3);
    }

    {
        let dev = FileBlockDevice::open(image.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = mount(&mut dev).expect("mount image");
        mkfile(&mut dev, &mut fs, path, None, None).expect("create legacy file");
        let inode_number = dir::get_inode_with_num(&mut fs, &mut dev, path)
            .expect("lookup legacy file")
            .expect("legacy file inode")
            .0;
        fs.modify_inode(&mut dev, inode_number, |inode| {
            inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
            inode.i_block = [0; 15];
        })
        .expect("convert empty inode to legacy mapping");

        write_file(&mut dev, &mut fs, path, 0, &expected).expect("write legacy file");
        let (_, inode) = dir::get_inode_with_num(&mut fs, &mut dev, path)
            .expect("lookup written legacy file")
            .expect("written legacy file inode");
        let huge_file = fs
            .superblock
            .has_feature_ro_compat(superblock::Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
        assert!(!inode.uses_extents());
        assert_eq!(
            inode.blocks_count(BLOCK_SIZE as u32, huge_file),
            15 * (BLOCK_SIZE / 512) as u64,
            "14 data blocks plus one single-indirect block must be accounted"
        );
        umount(fs, &mut dev).expect("umount legacy image");
    }

    e2fsck_readonly_clean(&image, "legacy direct-to-single write");

    {
        let dev = FileBlockDevice::open(image.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = mount(&mut dev).expect("remount legacy image");
        assert_eq!(
            read_file(&mut dev, &mut fs, path).expect("read remounted legacy file"),
            expected
        );
        umount(fs, &mut dev).expect("umount remounted legacy image");
    }

    e2fsck_readonly_clean(&image, "remounted legacy direct-to-single write");
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
        let _ = rename(
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
