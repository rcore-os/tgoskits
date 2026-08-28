use std::{
    cell::{Cell, RefCell},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    rc::Rc,
    time::Duration,
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

struct CountingFileBlockDevice {
    inner: FileBlockDevice,
    reads: Rc<Cell<usize>>,
}

struct MmpWriteOrderDevice {
    inner: FileBlockDevice,
    mmp_sector: u64,
    writes: Rc<RefCell<Vec<bool>>>,
}

struct MmpReleaseFailureDevice {
    inner: FileBlockDevice,
    mmp_sector: u64,
    fail_clean: Rc<Cell<bool>>,
}

impl MmpReleaseFailureDevice {
    fn open(path: PathBuf, mmp_block: u64) -> (Self, Rc<Cell<bool>>) {
        let fail_clean = Rc::new(Cell::new(false));
        (
            Self {
                inner: FileBlockDevice::open_with_sector_size(path, 512),
                mmp_sector: mmp_block * (4096 / 512),
                fail_clean: Rc::clone(&fail_clean),
            },
            fail_clean,
        )
    }

    fn is_clean_mmp(&self, buffer: &[u8], sector: SectorId) -> bool {
        sector.raw() == self.mmp_sector
            && buffer.get(0..4) == Some(&0x004d_4d50_u32.to_le_bytes())
            && buffer.get(4..8) == Some(&0xff4d_4d50_u32.to_le_bytes())
    }
}

impl BlockIo for MmpReleaseFailureDevice {
    fn read(&mut self, buffer: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        self.inner.read(buffer, sector, count)
    }

    fn write(&mut self, buffer: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        if self.fail_clean.get() && self.is_clean_mmp(buffer, sector) {
            self.fail_clean.set(false);
            return Err(Ext4Error::io());
        }
        self.inner.write(buffer, sector, count)
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.inner.flush()
    }

    fn geometry(&self) -> DeviceGeometry {
        self.inner.geometry()
    }

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.capabilities()
    }
}

impl MmpWriteOrderDevice {
    fn open(path: PathBuf, mmp_block: u64) -> (Self, Rc<RefCell<Vec<bool>>>) {
        let writes = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                inner: FileBlockDevice::open_with_sector_size(path, 512),
                mmp_sector: mmp_block * (4096 / 512),
                writes: Rc::clone(&writes),
            },
            writes,
        )
    }
}

impl BlockIo for MmpWriteOrderDevice {
    fn read(&mut self, buffer: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        self.inner.read(buffer, sector, count)
    }

    fn write(&mut self, buffer: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        let is_clean_mmp = sector.raw() == self.mmp_sector
            && buffer.get(0..4) == Some(&0x004d_4d50_u32.to_le_bytes())
            && buffer.get(4..8) == Some(&0xff4d_4d50_u32.to_le_bytes());
        self.writes.borrow_mut().push(is_clean_mmp);
        self.inner.write(buffer, sector, count)
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.inner.flush()
    }

    fn geometry(&self) -> DeviceGeometry {
        self.inner.geometry()
    }

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.capabilities()
    }
}

impl CountingFileBlockDevice {
    fn open_with_sector_size(path: PathBuf, sector_size: u32) -> (Self, Rc<Cell<usize>>) {
        let reads = Rc::new(Cell::new(0));
        (
            Self {
                inner: FileBlockDevice::open_with_sector_size(path, sector_size),
                reads: Rc::clone(&reads),
            },
            reads,
        )
    }
}

impl BlockIo for CountingFileBlockDevice {
    fn read(&mut self, buffer: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        self.reads.set(self.reads.get() + 1);
        self.inner.read(buffer, sector, count)
    }

    fn write(&mut self, buffer: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        self.inner.write(buffer, sector, count)
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.inner.flush()
    }

    fn geometry(&self) -> DeviceGeometry {
        self.inner.geometry()
    }

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.capabilities()
    }
}

struct PatternFailureDevice {
    inner: FileBlockDevice,
    pattern: Vec<u8>,
    armed: Rc<Cell<bool>>,
}

impl PatternFailureDevice {
    fn open(path: PathBuf, pattern: &[u8]) -> (Self, Rc<Cell<bool>>) {
        let armed = Rc::new(Cell::new(false));
        (
            Self {
                inner: FileBlockDevice::open_with_sector_size(path, 512),
                pattern: pattern.to_vec(),
                armed: Rc::clone(&armed),
            },
            armed,
        )
    }
}

impl BlockIo for PatternFailureDevice {
    fn read(&mut self, buffer: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        self.inner.read(buffer, sector, count)
    }

    fn write(&mut self, buffer: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        self.inner.write(buffer, sector, count)?;
        if self.armed.get()
            && buffer
                .windows(self.pattern.len())
                .any(|window| window == self.pattern)
        {
            self.armed.set(false);
            return Err(Ext4Error::io());
        }
        Ok(())
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.inner.flush()
    }

    fn geometry(&self) -> DeviceGeometry {
        self.inner.geometry()
    }

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.capabilities()
    }
}

impl Clock for PatternFailureDevice {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        self.inner.now()
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

struct FixedEntropy(u32);

impl EntropySource for FixedEntropy {
    fn fill_bytes(&mut self, output: &mut [u8]) -> Ext4Result<()> {
        for chunk in output.chunks_mut(4) {
            let bytes = self.0.to_ne_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
            self.0 = self.0.wrapping_add(1);
        }
        Ok(())
    }
}

#[derive(Default)]
struct LogicalDelay {
    elapsed: Duration,
}

impl Delay for LogicalDelay {
    fn wait(&mut self, duration: Duration) -> Ext4Result<()> {
        self.elapsed += duration;
        Ok(())
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
    create_ext4_test_image_with_args(prefix, size, &[])
}

fn create_ext4_test_image_with_args(
    prefix: &str,
    size: &str,
    mkfs_args: &[&str],
) -> (PathBuf, PathBuf) {
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
                .args(["-F", "-q", "-b", "4096"])
                .args(mkfs_args)
                .arg(&image);
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

#[test]
fn linux_mmp_image_preserves_read_only_mounts_and_runs_writable_ownership_lifecycle() {
    for tool in ["mkfs.ext4", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) =
        create_ext4_test_image_with_args("rsext4-linux-mmp-readonly", "64M", &["-O", "mmp"]);
    let original_image = fs::read(&image).expect("snapshot Linux MMP image");

    let superblock_offset = 1024;
    let mmp_field_offset = superblock_offset + 0x168;
    let mmp_block = u64::from_le_bytes(
        original_image[mmp_field_offset..mmp_field_offset + 8]
            .try_into()
            .unwrap(),
    );
    {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let services = MountServices::new(TestClock(Cell::new(1_800_000_000)), (), NoopObserver);
        let mut filesystem = Ext4::mount(
            device,
            services,
            MountOptions::read_only_no_journal_replay(),
        )
        .expect("mount Linux MMP image read-only");
        let root = filesystem
            .inode(filesystem.root_inode())
            .expect("inspect MMP image root inode");
        assert!(root.is_directory());

        let read_only_options = filesystem.options();
        let error = filesystem
            .remount(MountOptions {
                readonly: false,
                ..read_only_options
            })
            .expect_err("writable MMP needs injected entropy");
        assert_eq!(error.kind(), Ext4ErrorKind::UnsupportedCapability);
        assert_eq!(filesystem.options(), read_only_options);
        filesystem.unmount().expect("unmount MMP image read-only");
    }

    assert_eq!(
        fs::read(&image).expect("read MMP image after rsext4 mount"),
        original_image,
        "read-only mount must not rewrite the superblock or MMP protection block"
    );

    let write_order;
    {
        let (device, writes) = MmpWriteOrderDevice::open(image.clone(), mmp_block);
        write_order = writes;
        let services = MountServices::new(
            TestClock(Cell::new(1_800_000_000)),
            FixedEntropy(0x1234_5678),
            NoopObserver,
        )
        .with_mmp(
            LogicalDelay::default(),
            MmpIdentity::from_names(b"rsext4-test", b"linux-mmp.img"),
        );
        let mut filesystem = Ext4::mount(device, services, MountOptions::read_write())
            .expect("claim writable Linux MMP image");
        let interval = filesystem
            .mmp_refresh_interval()
            .expect("MMP mount must expose its refresh interval");
        assert!(interval <= Duration::from_secs(300));
        assert_eq!(filesystem.refresh_mmp(interval).unwrap(), Some(interval));
        filesystem.unmount().expect("clean writable MMP unmount");
    }
    assert_eq!(
        write_order.borrow().last(),
        Some(&true),
        "Linux releases MMP only after the filesystem and journal are clean"
    );

    let current_image = fs::read(&image).expect("read writable MMP image");
    let mmp_offset = usize::try_from(mmp_block * 4096).unwrap();
    assert_eq!(
        u32::from_le_bytes(
            current_image[mmp_offset + 4..mmp_offset + 8]
                .try_into()
                .unwrap()
        ),
        0xff4d_4d50,
        "clean unmount must release the MMP lease"
    );
    e2fsck_readonly_clean(&image, "read-only Linux MMP image");
    fs::remove_dir_all(temp_dir).expect("remove MMP temp dir");
}

#[test]
fn failed_mmp_clean_release_leaves_a_terminal_non_mutating_mount() {
    for tool in ["mkfs.ext4", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) =
        create_ext4_test_image_with_args("rsext4-linux-mmp-release-fault", "64M", &["-O", "mmp"]);
    let initial_image = fs::read(&image).expect("read Linux MMP image");
    let mmp_field_offset = 1024 + 0x168;
    let mmp_block = u64::from_le_bytes(
        initial_image[mmp_field_offset..mmp_field_offset + 8]
            .try_into()
            .unwrap(),
    );
    let (device, fail_clean) = MmpReleaseFailureDevice::open(image.clone(), mmp_block);
    let services = MountServices::new(
        TestClock(Cell::new(1_800_000_000)),
        FixedEntropy(0x1234_5678),
        NoopObserver,
    )
    .with_mmp(LogicalDelay::default(), MmpIdentity::default());
    let mut filesystem = Ext4::mount(device, services, MountOptions::read_write())
        .expect("claim writable Linux MMP image");

    fail_clean.set(true);
    assert_eq!(filesystem.unmount().unwrap_err().kind(), Ext4ErrorKind::Io);
    assert_eq!(filesystem.sync().unwrap_err().kind(), Ext4ErrorKind::Io);
    assert_eq!(
        filesystem
            .remount(MountOptions {
                readonly: true,
                ..MountOptions::read_write()
            })
            .unwrap_err()
            .kind(),
        Ext4ErrorKind::Busy
    );
    assert_eq!(filesystem.unmount().unwrap_err().kind(), Ext4ErrorKind::Io);

    let current_image = fs::read(&image).expect("read failed-release MMP image");
    let mmp_offset = usize::try_from(mmp_block * 4096).unwrap();
    assert_ne!(
        u32::from_le_bytes(
            current_image[mmp_offset + 4..mmp_offset + 8]
                .try_into()
                .unwrap()
        ),
        0xff4d_4d50,
        "a failed CLEAN write must not be retried after ownership becomes uncertain"
    );
    fs::remove_dir_all(temp_dir).expect("remove MMP fault temp dir");
}

#[test]
fn linux_indexed_directory_lookup_uses_on_disk_htree_root() {
    for tool in ["mkfs.ext4", "debugfs", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_test_image("rsext4-linux-htree-lookup", "64M");
    let source = temp_dir.join("payload.bin");
    let payload = b"linux htree lookup payload";
    fs::write(&source, payload).expect("write HTree payload");
    let mut script = String::from("mkdir /indexed\n");
    for index in 0..800 {
        script.push_str(&format!(
            "write {} /indexed/entry-{index:04}.bin\n",
            source.display()
        ));
    }
    run_debugfs_script(&image, &script, "populate indexed directory");

    let output = Command::new("e2fsck")
        .args(["-fyD"])
        .arg(&image)
        .output()
        .expect("run e2fsck directory optimizer");
    assert!(
        e2fsck_status_ok(&output, true),
        "e2fsck failed to create HTree index\n{}",
        command_text(&output)
    );
    let dump = debugfs_query(&image, "htree_dump /indexed");
    assert!(
        dump.contains("Root node dump"),
        "e2fsprogs did not create an HTree root\n{dump}"
    );
    e2fsck_readonly_clean(&image, "Linux HTree fixture");

    let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
    let mut device = Jbd2Dev::initial_jbd2dev(0, device, true);
    let mut filesystem = Ext4FileSystem::mount(&mut device).expect("mount Linux HTree fixture");
    assert_eq!(
        read_file(&mut device, &mut filesystem, "/indexed/entry-0799.bin")
            .expect("lookup HTree leaf through rsext4"),
        payload
    );
    umount(filesystem, &mut device).expect("unmount Linux HTree fixture");

    let (device, device_reads) = CountingFileBlockDevice::open_with_sector_size(image.clone(), 512);
    let services = MountServices::new(TestClock(Cell::new(1_800_000_000)), (), NoopObserver);
    let mut filesystem =
        Ext4::mount(device, services, MountOptions::read_write()).expect("mount HTree fixture");
    let indexed = filesystem
        .lookup_child(
            filesystem.root_inode(),
            FileName::new(b"indexed").expect("valid indexed name"),
        )
        .expect("lookup indexed directory")
        .expect("indexed directory exists");
    assert_eq!(
        filesystem
            .directory_end_cursor(indexed.number)
            .expect("query indexed directory end cursor"),
        DirectoryCursor::End,
        "Linux HTree directories must expose a hash-space EOF"
    );
    device_reads.set(0);
    let first_entry = filesystem
        .read_directory(indexed.number, DirectoryCursor::Start, 1)
        .expect("read first indexed directory entry");
    assert_eq!(first_entry.len(), 1);
    assert!(
        device_reads.get() <= 4,
        "a one-entry HTree batch must not read the complete directory: {} device reads",
        device_reads.get()
    );
    let entries = filesystem
        .read_directory(indexed.number, DirectoryCursor::Start, 1_000)
        .expect("read indexed directory");
    assert!(
        entries.iter().all(|entry| matches!(
            entry.next_cursor,
            DirectoryCursor::HTree { .. } | DirectoryCursor::End
        )),
        "indexed readdir must never expose a linear byte cursor: {entries:?}"
    );
    assert_eq!(entries.len(), 802, "HTree readdir lost or invented records");
    assert!(
        entries.windows(2).all(|pair| {
            let key = |cursor| match cursor {
                DirectoryCursor::HTree {
                    major,
                    minor,
                    collision,
                } => (major, minor, collision),
                DirectoryCursor::End => (u32::MAX, u32::MAX, u32::MAX),
                DirectoryCursor::Start | DirectoryCursor::Linear { .. } => unreachable!(),
            };
            key(pair[0].next_cursor) < key(pair[1].next_cursor)
        }),
        "HTree cursors must advance monotonically"
    );

    let expected_names = entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect::<Vec<_>>();
    let mut reader = filesystem
        .open_directory_reader(indexed.number)
        .expect("open indexed directory reader");
    let mut cursor = DirectoryCursor::Start;
    let mut batched_names = Vec::new();
    loop {
        let batch = filesystem
            .read_directory_with_reader(&mut reader, cursor, 1)
            .expect("resume indexed directory");
        let Some(entry) = batch.into_iter().next() else {
            break;
        };
        batched_names.push(entry.name);
        cursor = entry.next_cursor;
    }
    assert_eq!(
        batched_names, expected_names,
        "HTree cursor repeated or skipped records"
    );

    let mut mutation_reader = filesystem
        .open_directory_reader(indexed.number)
        .expect("open mutation-aware indexed directory reader");
    let first_batch = filesystem
        .read_directory_with_reader(&mut mutation_reader, DirectoryCursor::Start, 1)
        .expect("prime indexed directory range cache");
    let first_entry = first_batch
        .into_iter()
        .next()
        .expect("indexed directory contains dot entry");
    let removed_name = expected_names[2].clone();
    let removed = filesystem
        .unlink(
            indexed.number,
            FileName::new(&removed_name).expect("valid cached entry name"),
        )
        .expect("unlink entry cached by open reader");
    assert!(removed.requires_reap());
    filesystem
        .reap_unlinked_inode(removed.inode)
        .expect("reap cached unlinked entry");

    let mut names_after_mutation = vec![first_entry.name];
    let mut cursor = first_entry.next_cursor;
    loop {
        let batch = filesystem
            .read_directory_with_reader(&mut mutation_reader, cursor, 1)
            .expect("resume indexed directory after mutation");
        let Some(entry) = batch.into_iter().next() else {
            break;
        };
        names_after_mutation.push(entry.name);
        cursor = entry.next_cursor;
    }
    let expected_after_mutation = expected_names
        .into_iter()
        .filter(|name| name != &removed_name)
        .collect::<Vec<_>>();
    assert_eq!(
        names_after_mutation, expected_after_mutation,
        "an open HTree reader must discard cached records after i_version changes"
    );
    filesystem.unmount().expect("unmount owned HTree fixture");

    e2fsck_readonly_clean(&image, "rsext4-read Linux HTree fixture");
    fs::remove_dir_all(temp_dir).expect("remove HTree temp dir");
}

#[test]
fn owned_insert_preserves_linux_htree_index() {
    for tool in ["mkfs.ext4", "debugfs", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_test_image("rsext4-linux-owned-htree-insert", "64M");
    let source = temp_dir.join("payload.bin");
    fs::write(&source, b"linux htree insert payload").expect("write HTree payload");
    let mut script = String::from("mkdir /indexed\n");
    for index in 0..800 {
        script.push_str(&format!(
            "write {} /indexed/entry-{index:04}.bin\n",
            source.display()
        ));
    }
    run_debugfs_script(
        &image,
        &script,
        "populate indexed directory for owned insert",
    );

    let output = Command::new("e2fsck")
        .args(["-fyD"])
        .arg(&image)
        .output()
        .expect("run e2fsck directory optimizer");
    assert!(
        e2fsck_status_ok(&output, true),
        "e2fsck failed to create HTree index\n{}",
        command_text(&output)
    );
    assert!(
        debugfs_query(&image, "htree_dump /indexed").contains("Root node dump"),
        "e2fsprogs did not create an HTree root"
    );
    e2fsck_readonly_clean(&image, "Linux HTree owned-insert fixture");

    {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let services = MountServices::new(TestClock(Cell::new(1_800_000_000)), (), NoopObserver);
        let mut filesystem =
            Ext4::mount(device, services, MountOptions::read_write()).expect("mount Linux image");
        let root = filesystem.root_inode();
        let indexed = filesystem
            .lookup_child(root, FileName::new(b"indexed").expect("valid indexed name"))
            .expect("lookup indexed directory")
            .expect("indexed directory exists");
        assert!(indexed.flags.contains(InodeFlags::DIRECTORY_INDEX));

        filesystem
            .create_regular_file(
                MutationContext::new(1000, 1001, 0, 0o022),
                indexed.number,
                FileName::new(b"rsext4-owned-insert.bin").expect("valid insert name"),
                FilePermissions::new(0o644).expect("valid permissions"),
            )
            .expect("insert into Linux HTree directory");
        assert!(
            filesystem
                .inode(indexed.number)
                .expect("inspect indexed directory after insert")
                .flags
                .contains(InodeFlags::DIRECTORY_INDEX),
            "inserting through the portable core must preserve the Linux HTree"
        );
        assert!(
            filesystem
                .lookup_child(
                    indexed.number,
                    FileName::new(b"rsext4-owned-insert.bin").expect("valid inserted name"),
                )
                .expect("lookup inserted HTree child")
                .is_some()
        );
        filesystem.unmount().expect("unmount HTree insert image");
    }

    let dump = debugfs_query(&image, "htree_dump /indexed");
    assert!(
        dump.contains("Root node dump") && dump.contains("rsext4-owned-insert.bin"),
        "Linux did not preserve and decode the updated HTree\n{dump}"
    );
    e2fsck_readonly_clean(&image, "rsext4-updated Linux HTree fixture");
    fs::remove_dir_all(temp_dir).expect("remove HTree insert temp dir");
}

#[test]
fn owned_full_linear_directory_converts_to_linux_htree() {
    for tool in ["mkfs.ext4", "debugfs", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_test_image("rsext4-linux-owned-make-indexed", "64M");
    let names = (0..40)
        .map(|index| format!("auto-index-{index:03}-{}", "x".repeat(220)))
        .collect::<Vec<_>>();
    {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let services = MountServices::new(TestClock(Cell::new(1_800_000_000)), (), NoopObserver);
        let mut filesystem =
            Ext4::mount(device, services, MountOptions::read_write()).expect("mount Linux image");
        let context = MutationContext::new(1000, 1001, 0, 0o022);
        let directory = filesystem
            .create_directory(
                context,
                filesystem.root_inode(),
                FileName::new(b"auto-indexed").expect("valid directory name"),
                FilePermissions::new(0o755).expect("valid directory permissions"),
            )
            .expect("create linear directory");
        assert!(!directory.flags.contains(InodeFlags::DIRECTORY_INDEX));

        let permissions = FilePermissions::new(0o644).expect("valid file permissions");
        for (index, name) in names.iter().enumerate() {
            filesystem
                .create_regular_file(
                    context,
                    directory.number,
                    FileName::new(name.as_bytes()).expect("valid long directory name"),
                    permissions,
                )
                .unwrap_or_else(|error| panic!("fill linear directory at entry {index}: {error}"));
        }
        let indexed = filesystem
            .inode(directory.number)
            .expect("inspect converted directory");
        assert!(
            indexed.flags.contains(InodeFlags::DIRECTORY_INDEX),
            "the first full linear block must be converted to an HTree"
        );
        assert!(indexed.size >= 3 * 4096);
        for name in &names {
            assert!(
                filesystem
                    .lookup_child(
                        directory.number,
                        FileName::new(name.as_bytes()).expect("valid long name"),
                    )
                    .unwrap_or_else(|error| panic!("lookup {name} after conversion: {error}"))
                    .is_some(),
                "converted HTree lost {name}"
            );
        }
        filesystem.unmount().expect("unmount converted directory");
    }

    let dump = debugfs_query(&image, "htree_dump /auto-indexed");
    assert!(
        dump.contains("Root node dump"),
        "Linux did not decode the converted HTree\n{dump}"
    );
    for name in &names {
        assert!(dump.contains(name), "Linux HTree dump lost {name}\n{dump}");
    }
    e2fsck_readonly_clean(&image, "rsext4-created HTree root");
    fs::remove_dir_all(temp_dir).expect("remove make-indexed temp dir");
}

#[test]
fn failed_linear_to_htree_conversion_restores_linear_directory() {
    for tool in ["mkfs.ext4", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_test_image("rsext4-linux-make-indexed-rollback", "64M");
    let calibration = temp_dir.join("calibration.img");
    fs::copy(&image, &calibration).expect("copy make-indexed calibration image");
    let conversion_index = {
        let device = FileBlockDevice::open_with_sector_size(calibration.clone(), 512);
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        let mut filesystem =
            Ext4FileSystem::mount(&mut journal).expect("mount conversion calibration");
        mkdir(&mut journal, &mut filesystem, "/auto-indexed")
            .expect("create calibration directory");
        let mut conversion_index = None;
        for index in 0..40 {
            let name = format!("auto-index-{index:03}-{}", "x".repeat(220));
            mkfile(
                &mut journal,
                &mut filesystem,
                &format!("/auto-indexed/{name}"),
                None,
                None,
            )
            .unwrap_or_else(|error| panic!("fill calibration directory at {index}: {error}"));
            let (_, directory) =
                rsext4::dir::get_inode_with_num(&mut filesystem, &mut journal, "/auto-indexed")
                    .expect("lookup calibration directory")
                    .expect("calibration directory exists");
            if directory.i_flags & rsext4::disknode::Ext4Inode::EXT4_INDEX_FL != 0 {
                conversion_index = Some(index);
                break;
            }
        }
        umount(filesystem, &mut journal).expect("unmount conversion calibration");
        conversion_index.expect("calibration did not convert the linear directory")
    };
    fs::remove_file(calibration).expect("remove conversion calibration image");

    let failed_name = format!("auto-index-{conversion_index:03}-{}", "x".repeat(220));
    let existing_names = (0..conversion_index)
        .map(|index| format!("auto-index-{index:03}-{}", "x".repeat(220)))
        .collect::<Vec<_>>();
    let (device, armed) = PatternFailureDevice::open(image.clone(), failed_name.as_bytes());
    let before_stats;
    let before_directory_ino;
    let before_directory;
    let before_root_physical;
    let before_root_data;
    {
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        let mut filesystem =
            Ext4FileSystem::mount(&mut journal).expect("mount conversion fault image");
        mkdir(&mut journal, &mut filesystem, "/auto-indexed")
            .expect("create conversion fault directory");
        for (index, name) in existing_names.iter().enumerate() {
            mkfile(
                &mut journal,
                &mut filesystem,
                &format!("/auto-indexed/{name}"),
                None,
                None,
            )
            .unwrap_or_else(|error| panic!("fill fault directory at {index}: {error}"));
        }
        filesystem
            .sync_filesystem(&mut journal)
            .expect("sync pre-conversion fixture");
        journal.flush().expect("checkpoint pre-conversion fixture");
        journal
            .set_journal_use(false)
            .expect("disable journal for conversion post-write fault");
        before_stats = filesystem.statfs();
        (before_directory_ino, before_directory) =
            rsext4::dir::get_inode_with_num(&mut filesystem, &mut journal, "/auto-indexed")
                .expect("lookup pre-conversion directory")
                .expect("pre-conversion directory exists");
        assert_eq!(before_directory.size(), 4096);
        assert_eq!(
            before_directory.i_flags & rsext4::disknode::Ext4Inode::EXT4_INDEX_FL,
            0
        );
        let mut mapping_inode = before_directory;
        before_root_physical = rsext4::loopfile::resolve_inode_block(
            &filesystem,
            &mut journal,
            before_directory_ino,
            &mut mapping_inode,
            0,
        )
        .expect("resolve pre-conversion root block")
        .expect("pre-conversion root block is mapped");
        before_root_data = filesystem
            .datablock_cache
            .get_or_load(&mut journal, before_root_physical)
            .expect("read pre-conversion root block")
            .data
            .as_ref()
            .clone();

        armed.set(true);
        let error = mkfile(
            &mut journal,
            &mut filesystem,
            &format!("/auto-indexed/{failed_name}"),
            None,
            None,
        )
        .expect_err("post-write failure must abort HTree conversion");
        assert_eq!(error.kind(), rsext4::Ext4ErrorKind::Io);
        assert!(!armed.get(), "conversion fault pattern was not written");
        filesystem
            .umount(&mut journal)
            .expect("unmount after failed HTree conversion");
    }

    {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, false);
        let mut filesystem =
            Ext4FileSystem::mount(&mut journal).expect("remount after failed HTree conversion");
        let (directory_ino, directory) =
            rsext4::dir::get_inode_with_num(&mut filesystem, &mut journal, "/auto-indexed")
                .expect("lookup directory after failed conversion")
                .expect("linear directory survives failed conversion");
        assert_eq!(directory.size(), before_directory.size());
        assert_eq!(directory.i_blocks_lo, before_directory.i_blocks_lo);
        assert_eq!(directory.l_i_blocks_high, before_directory.l_i_blocks_high);
        assert_eq!(directory.i_flags, before_directory.i_flags);
        let mut mapping_inode = directory;
        let restored_root_physical = rsext4::loopfile::resolve_inode_block(
            &filesystem,
            &mut journal,
            directory_ino,
            &mut mapping_inode,
            0,
        )
        .expect("resolve restored linear root block")
        .expect("restored linear root block is mapped");
        assert_eq!(restored_root_physical, before_root_physical);
        let restored_root_data = filesystem
            .datablock_cache
            .get_or_load(&mut journal, restored_root_physical)
            .expect("read restored linear root block")
            .data
            .as_ref()
            .clone();
        assert_eq!(
            restored_root_data, before_root_data,
            "failed conversion must restore the complete linear directory block"
        );
        let after_stats = filesystem.statfs();
        assert_eq!(after_stats.free_blocks, before_stats.free_blocks);
        assert_eq!(after_stats.free_inodes, before_stats.free_inodes);
        assert!(
            rsext4::dir::get_inode_with_num(
                &mut filesystem,
                &mut journal,
                &format!("/auto-indexed/{failed_name}"),
            )
            .expect("lookup rejected conversion child")
            .is_none()
        );
        for name in &existing_names {
            assert!(
                rsext4::dir::get_inode_with_num(
                    &mut filesystem,
                    &mut journal,
                    &format!("/auto-indexed/{name}"),
                )
                .unwrap_or_else(|error| panic!("lookup retained linear child {name}: {error}"))
                .is_some(),
                "failed conversion lost retained child {name}"
            );
        }
        umount(filesystem, &mut journal).expect("unmount conversion rollback image");
    }

    e2fsck_readonly_clean(&image, "failed linear-to-HTree conversion rollback");
    fs::remove_dir_all(temp_dir).expect("remove conversion rollback temp dir");
}

#[test]
fn indexed_delete_and_rename_keep_linux_htree_layout() {
    for tool in ["mkfs.ext4", "debugfs", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_test_image("rsext4-linux-indexed-delete", "64M");
    let names = (0..40)
        .map(|index| format!("indexed-delete-{index:03}-{}", "x".repeat(216)))
        .collect::<Vec<_>>();
    let deleted_index = 17usize;
    let replaced_index = 18usize;
    let rename_source = b"indexed-rename-source";
    let directory_number;
    let indexed_size;
    let renamed_inode;
    {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let services = MountServices::new(TestClock(Cell::new(1_810_000_000)), (), NoopObserver);
        let mut filesystem =
            Ext4::mount(device, services, MountOptions::read_write()).expect("mount Linux image");
        let context = MutationContext::new(1000, 1001, 0, 0o022);
        let directory = filesystem
            .create_directory(
                context,
                filesystem.root_inode(),
                FileName::new(b"indexed-delete").expect("valid directory name"),
                FilePermissions::new(0o755).expect("valid directory permissions"),
            )
            .expect("create indexed-delete directory");
        directory_number = directory.number;
        let permissions = FilePermissions::new(0o644).expect("valid file permissions");
        for (index, name) in names.iter().enumerate() {
            filesystem
                .create_regular_file(
                    context,
                    directory_number,
                    FileName::new(name.as_bytes()).expect("valid long name"),
                    permissions,
                )
                .unwrap_or_else(|error| panic!("create indexed child {index}: {error}"));
        }
        let source = filesystem
            .create_regular_file(
                context,
                directory_number,
                FileName::new(rename_source).expect("valid rename source"),
                permissions,
            )
            .expect("create indexed rename source");
        renamed_inode = source.number;
        let indexed = filesystem
            .inode(directory_number)
            .expect("inspect indexed-delete directory");
        assert!(indexed.flags.contains(InodeFlags::DIRECTORY_INDEX));
        indexed_size = indexed.size;

        let rename = filesystem
            .rename(
                directory_number,
                FileName::new(rename_source).expect("valid rename source"),
                directory_number,
                FileName::new(names[replaced_index].as_bytes()).expect("valid replacement name"),
                RenameOptions::REPLACE,
            )
            .expect("replace indexed target");
        let replaced = rename.replaced.expect("indexed rename must replace target");
        assert!(replaced.requires_reap());
        filesystem
            .reap_unlinked_inode(replaced.inode)
            .expect("reap replaced indexed target");

        let deleted = filesystem
            .unlink(
                directory_number,
                FileName::new(names[deleted_index].as_bytes()).expect("valid deleted name"),
            )
            .expect("unlink indexed child");
        assert!(deleted.requires_reap());
        filesystem
            .reap_unlinked_inode(deleted.inode)
            .expect("reap deleted indexed child");

        assert!(
            filesystem
                .lookup_child(
                    directory_number,
                    FileName::new(names[deleted_index].as_bytes()).expect("valid deleted name"),
                )
                .expect("lookup deleted indexed child")
                .is_none()
        );
        assert!(
            filesystem
                .lookup_child(
                    directory_number,
                    FileName::new(rename_source).expect("valid rename source"),
                )
                .expect("lookup old indexed rename source")
                .is_none()
        );
        assert_eq!(
            filesystem
                .lookup_child(
                    directory_number,
                    FileName::new(names[replaced_index].as_bytes())
                        .expect("valid replacement name"),
                )
                .expect("lookup indexed rename target")
                .expect("indexed rename target exists")
                .number,
            renamed_inode
        );
        let after = filesystem
            .inode(directory_number)
            .expect("inspect indexed directory after mutation");
        assert_eq!(after.size, indexed_size);
        assert!(after.flags.contains(InodeFlags::DIRECTORY_INDEX));
        filesystem.unmount().expect("unmount indexed-delete image");
    }

    {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let services = MountServices::new(TestClock(Cell::new(1_820_000_000)), (), NoopObserver);
        let mut filesystem =
            Ext4::mount(device, services, MountOptions::read_write()).expect("remount Linux image");
        let directory = filesystem
            .lookup_child(
                filesystem.root_inode(),
                FileName::new(b"indexed-delete").expect("valid directory name"),
            )
            .expect("lookup indexed-delete directory")
            .expect("indexed-delete directory exists");
        assert_eq!(directory.number, directory_number);
        assert_eq!(directory.size, indexed_size);
        assert!(directory.flags.contains(InodeFlags::DIRECTORY_INDEX));
        for (index, name) in names.iter().enumerate() {
            let child = filesystem
                .lookup_child(
                    directory_number,
                    FileName::new(name.as_bytes()).expect("valid long name"),
                )
                .unwrap_or_else(|error| panic!("lookup remounted indexed child {index}: {error}"));
            if index == deleted_index {
                assert!(child.is_none(), "deleted indexed child reappeared");
            } else {
                let child = child.unwrap_or_else(|| panic!("indexed child {index} disappeared"));
                if index == replaced_index {
                    assert_eq!(child.number, renamed_inode);
                }
            }
        }

        for (index, name) in names.iter().enumerate() {
            if index == deleted_index {
                continue;
            }
            let outcome = filesystem
                .unlink(
                    directory_number,
                    FileName::new(name.as_bytes()).expect("valid long name"),
                )
                .unwrap_or_else(|error| panic!("unlink indexed child {index}: {error}"));
            assert!(outcome.requires_reap());
            filesystem
                .reap_unlinked_inode(outcome.inode)
                .unwrap_or_else(|error| panic!("reap indexed child {index}: {error}"));
        }
        let empty = filesystem
            .inode(directory_number)
            .expect("inspect empty indexed directory");
        assert_eq!(empty.size, indexed_size);
        assert!(empty.flags.contains(InodeFlags::DIRECTORY_INDEX));
        filesystem
            .unmount()
            .expect("unmount empty indexed directory");
    }

    let dump = debugfs_query(&image, "htree_dump /indexed-delete");
    assert!(
        dump.contains("Root node dump"),
        "Linux did not preserve the empty HTree\n{dump}"
    );
    assert!(!dump.contains(&names[deleted_index]));
    e2fsck_readonly_clean(&image, "indexed unlink and rename");
    fs::remove_dir_all(temp_dir).expect("remove indexed-delete temp dir");
}

#[test]
fn owned_insert_splits_linux_htree_leaf() {
    for tool in ["mkfs.ext4", "debugfs", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_test_image("rsext4-linux-owned-htree-split", "64M");
    let source = temp_dir.join("payload.bin");
    fs::write(&source, b"linux htree split payload").expect("write HTree payload");
    let mut script = String::from("mkdir /indexed\n");
    for index in 0..800 {
        script.push_str(&format!(
            "write {} /indexed/entry-{index:04}.bin\n",
            source.display()
        ));
    }
    run_debugfs_script(&image, &script, "populate indexed directory for leaf split");
    let output = Command::new("e2fsck")
        .args(["-fyD"])
        .arg(&image)
        .output()
        .expect("run e2fsck directory optimizer");
    assert!(
        e2fsck_status_ok(&output, true),
        "e2fsck failed to create HTree index\n{}",
        command_text(&output)
    );

    {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let services = MountServices::new(TestClock(Cell::new(1_800_000_000)), (), NoopObserver);
        let mut filesystem =
            Ext4::mount(device, services, MountOptions::read_write()).expect("mount Linux image");
        let indexed = filesystem
            .lookup_child(
                filesystem.root_inode(),
                FileName::new(b"indexed").expect("valid indexed name"),
            )
            .expect("lookup indexed directory")
            .expect("indexed directory exists");
        let context = MutationContext::new(1000, 1001, 0, 0o022);
        let permissions = FilePermissions::new(0o644).expect("valid permissions");
        for index in 0..1_000 {
            let name = format!("rsext4-split-{index:04}.bin");
            filesystem
                .create_regular_file(
                    context,
                    indexed.number,
                    FileName::new(name.as_bytes()).expect("valid split entry name"),
                    permissions,
                )
                .unwrap_or_else(|error| panic!("insert HTree split entry {index}: {error}"));
        }
        assert!(
            filesystem
                .inode(indexed.number)
                .expect("inspect split directory")
                .flags
                .contains(InodeFlags::DIRECTORY_INDEX)
        );
        for index in [0, 499, 999] {
            let name = format!("rsext4-split-{index:04}.bin");
            assert!(
                filesystem
                    .lookup_child(
                        indexed.number,
                        FileName::new(name.as_bytes()).expect("valid lookup name"),
                    )
                    .expect("lookup split entry")
                    .is_some(),
                "missing split entry {index}"
            );
        }
        filesystem.unmount().expect("unmount split image");
    }

    let dump = debugfs_query(&image, "htree_dump /indexed");
    assert!(
        dump.contains("Root node dump") && dump.contains("rsext4-split-0999.bin"),
        "Linux did not decode the split HTree\n{dump}"
    );
    e2fsck_readonly_clean(&image, "rsext4-split Linux HTree fixture");
    fs::remove_dir_all(temp_dir).expect("remove HTree split temp dir");
}

#[test]
fn owned_insert_grows_a_full_linux_htree_root() {
    for tool in ["mkfs.ext4", "debugfs", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_test_image("rsext4-linux-owned-htree-root-growth", "128M");
    let source = temp_dir.join("payload.bin");
    fs::write(&source, b"linux htree root growth payload").expect("write HTree payload");
    let mut script = String::from("mkdir /indexed\n");
    for index in 0..800 {
        script.push_str(&format!(
            "write {} /indexed/entry-{index:04}.bin\n",
            source.display()
        ));
    }
    run_debugfs_script(
        &image,
        &script,
        "populate indexed directory for root growth",
    );
    let output = Command::new("e2fsck")
        .args(["-fyD"])
        .arg(&image)
        .output()
        .expect("run e2fsck directory optimizer");
    assert!(
        e2fsck_status_ok(&output, true),
        "e2fsck failed to create HTree index\n{}",
        command_text(&output)
    );

    {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let services = MountServices::new(TestClock(Cell::new(1_800_000_000)), (), NoopObserver);
        let mut filesystem =
            Ext4::mount(device, services, MountOptions::read_write()).expect("mount Linux image");
        let indexed = filesystem
            .lookup_child(
                filesystem.root_inode(),
                FileName::new(b"indexed").expect("valid indexed name"),
            )
            .expect("lookup indexed directory")
            .expect("indexed directory exists");
        let context = MutationContext::new(1000, 1001, 0, 0o022);
        let permissions = FilePermissions::new(0o644).expect("valid permissions");
        for index in 0..9_000 {
            let name = format!("rsext4-root-growth-{index:05}-{}", "x".repeat(220));
            filesystem
                .create_regular_file(
                    context,
                    indexed.number,
                    FileName::new(name.as_bytes()).expect("valid root-growth name"),
                    permissions,
                )
                .unwrap_or_else(|error| panic!("insert HTree root-growth entry {index}: {error}"));
        }
        let last_name = format!("rsext4-root-growth-08999-{}", "x".repeat(220));
        assert!(
            filesystem
                .lookup_child(
                    indexed.number,
                    FileName::new(last_name.as_bytes()).expect("valid last root-growth name"),
                )
                .expect("lookup after HTree root growth")
                .is_some()
        );
        let mut cursor = DirectoryCursor::Start;
        let mut entry_count = 0_usize;
        let mut found_last = false;
        let mut reader = filesystem
            .open_directory_reader(indexed.number)
            .expect("open multilevel HTree reader");
        loop {
            let batch = filesystem
                .read_directory_with_reader(&mut reader, cursor, 127)
                .expect("enumerate grown multilevel HTree");
            if batch.is_empty() {
                break;
            }
            for entry in batch {
                found_last |= entry.name == last_name.as_bytes();
                entry_count += 1;
                cursor = entry.next_cursor;
            }
        }
        assert_eq!(entry_count, 9_802, "multilevel HTree readdir lost entries");
        assert!(
            found_last,
            "multilevel HTree readdir missed the last insert"
        );
        filesystem.unmount().expect("unmount root-growth image");
    }

    let dump = debugfs_query(&image, "htree_dump /indexed");
    assert!(
        dump.contains("Indirect levels: 1"),
        "Linux did not decode the grown HTree root\n{dump}"
    );
    e2fsck_readonly_clean(&image, "rsext4-grown Linux HTree fixture");
    fs::remove_dir_all(temp_dir).expect("remove HTree root-growth temp dir");
}

#[test]
fn failed_owned_htree_leaf_split_rolls_back_after_data_write() {
    for tool in ["mkfs.ext4", "debugfs", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_test_image("rsext4-linux-owned-htree-rollback", "64M");
    let source = temp_dir.join("payload.bin");
    fs::write(&source, b"linux htree rollback payload").expect("write HTree payload");
    let mut script = String::from("mkdir /indexed\n");
    for index in 0..800 {
        script.push_str(&format!(
            "write {} /indexed/entry-{index:04}.bin\n",
            source.display()
        ));
    }
    run_debugfs_script(
        &image,
        &script,
        "populate indexed directory for split rollback",
    );
    let output = Command::new("e2fsck")
        .args(["-fyD"])
        .arg(&image)
        .output()
        .expect("run e2fsck directory optimizer");
    assert!(
        e2fsck_status_ok(&output, true),
        "e2fsck failed to create HTree index\n{}",
        command_text(&output)
    );

    let calibration = temp_dir.join("calibration.img");
    fs::copy(&image, &calibration).expect("copy HTree split calibration image");
    let split_index = {
        let device = FileBlockDevice::open_with_sector_size(calibration.clone(), 512);
        let services = MountServices::new(TestClock(Cell::new(1_800_000_000)), (), NoopObserver);
        let mut filesystem =
            Ext4::mount(device, services, MountOptions::read_write()).expect("mount calibration");
        let indexed = filesystem
            .lookup_child(
                filesystem.root_inode(),
                FileName::new(b"indexed").expect("valid indexed name"),
            )
            .expect("lookup calibration directory")
            .expect("calibration directory exists");
        let original_size = indexed.size;
        let context = MutationContext::new(1000, 1001, 0, 0o022);
        let permissions = FilePermissions::new(0o644).expect("valid permissions");
        let mut split_index = None;
        for index in 0..1_000 {
            let name = format!("rsext4-split-{index:04}.bin");
            filesystem
                .create_regular_file(
                    context,
                    indexed.number,
                    FileName::new(name.as_bytes()).expect("valid calibration name"),
                    permissions,
                )
                .unwrap_or_else(|error| panic!("insert calibration entry {index}: {error}"));
            if filesystem
                .inode(indexed.number)
                .expect("inspect calibration directory")
                .size
                > original_size
            {
                split_index = Some(index);
                break;
            }
        }
        filesystem.unmount().expect("unmount calibration image");
        split_index.expect("calibration did not trigger an HTree leaf split")
    };
    fs::remove_file(calibration).expect("remove HTree split calibration image");

    let failed_name = format!("rsext4-split-{split_index:04}.bin");
    let (device, armed) = PatternFailureDevice::open(image.clone(), failed_name.as_bytes());
    let before_size;
    let before_blocks;
    let before_flags;
    let before_stats;
    {
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount Linux image");
        for index in 0..split_index {
            let name = format!("rsext4-split-{index:04}.bin");
            mkfile(
                &mut journal,
                &mut filesystem,
                &format!("/indexed/{name}"),
                None,
                None,
            )
            .unwrap_or_else(|error| panic!("insert pre-split entry {index}: {error}"));
        }
        filesystem
            .sync_filesystem(&mut journal)
            .expect("sync pre-split fixture");
        journal.flush().expect("checkpoint pre-split fixture");
        journal
            .set_journal_use(false)
            .expect("disable journal for deterministic post-write fault");

        let (_, directory) =
            rsext4::dir::get_inode_with_num(&mut filesystem, &mut journal, "/indexed")
                .expect("lookup directory before failed split")
                .expect("indexed directory exists");
        before_size = directory.size();
        before_blocks = directory.blocks_count(
            filesystem.superblock.block_size() as u32,
            filesystem.superblock.has_feature_ro_compat(
                rsext4::superblock::Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE,
            ),
        );
        before_flags = directory.i_flags;
        before_stats = filesystem.statfs();

        armed.set(true);
        let error = mkfile(
            &mut journal,
            &mut filesystem,
            &format!("/indexed/{failed_name}"),
            None,
            None,
        )
        .expect_err("post-write failure must abort the HTree split transaction");
        assert_eq!(error.kind(), rsext4::Ext4ErrorKind::Io);
        assert!(
            !armed.get(),
            "fault pattern was not observed on the write path"
        );
        filesystem
            .umount(&mut journal)
            .expect("unmount after failed direct split");
    }

    {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let services = MountServices::new(TestClock(Cell::new(1_900_000_000)), (), NoopObserver);
        let mut filesystem =
            Ext4::mount(device, services, MountOptions::read_write()).expect("remount after fault");
        let indexed = filesystem
            .lookup_child(
                filesystem.root_inode(),
                FileName::new(b"indexed").expect("valid indexed name"),
            )
            .expect("lookup indexed directory after fault")
            .expect("indexed directory survives fault");
        let after_directory = filesystem
            .inode(indexed.number)
            .expect("inspect directory after failed split");
        assert_eq!(after_directory.size, before_size);
        assert_eq!(after_directory.blocks, before_blocks);
        assert_eq!(
            after_directory.flags,
            InodeFlags::from_bits_retain(before_flags)
        );
        let after_stats = filesystem.statfs();
        assert_eq!(after_stats.free_blocks, before_stats.free_blocks);
        assert_eq!(after_stats.free_inodes, before_stats.free_inodes);
        assert!(
            filesystem
                .lookup_child(
                    indexed.number,
                    FileName::new(failed_name.as_bytes()).expect("valid failed split name"),
                )
                .expect("lookup failed split name after remount")
                .is_none()
        );
        assert!(
            filesystem
                .lookup_child(
                    indexed.number,
                    FileName::new(format!("rsext4-split-{:04}.bin", split_index - 1).as_bytes(),)
                        .expect("valid retained name"),
                )
                .expect("lookup retained entry after remount")
                .is_some()
        );
        filesystem.unmount().expect("unmount rollback image");
    }

    e2fsck_readonly_clean(&image, "failed rsext4 HTree split rollback");
    fs::remove_dir_all(temp_dir).expect("remove HTree rollback temp dir");
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
        let mut fs = Ext4FileSystem::mount(&mut dev).expect("mount Linux-created geometry image");
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
            RenameOptions::REPLACE,
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
        let mut fs = Ext4FileSystem::mount(&mut dev).expect("mount rsext4-created geometry image");
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

fn file_extent_map_geometry_round_trip(filesystem_block_size: u32) {
    for tool in ["mkfs.ext4", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) =
        create_ext4_geometry_image("rsext4-file-extent-map", "64M", filesystem_block_size);
    let block_size = u64::from(filesystem_block_size);
    let inode_number = {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let services = MountServices::new(TestClock(Cell::new(1_800_000_000)), (), NoopObserver);
        let mut filesystem =
            Ext4::mount(device, services, MountOptions::read_write()).expect("mount Linux image");
        let context = MutationContext::new(1000, 1000, 0, 0o022);
        let file = filesystem
            .create_regular_file(
                context,
                filesystem.root_inode(),
                FileName::new(b"fiemap.bin").expect("valid file name"),
                FilePermissions::new(0o600).expect("valid permissions"),
            )
            .expect("create FIEMAP fixture");
        filesystem
            .write_inode(file.number, 0, &vec![0x11; block_size as usize])
            .expect("write first initialized extent");
        filesystem
            .write_inode(
                file.number,
                2 * block_size,
                &vec![0x22; block_size as usize],
            )
            .expect("write sparse initialized extent");
        filesystem
            .preallocate_inode(
                file.number,
                4 * block_size,
                block_size,
                PreallocationOptions::EXTEND_SIZE,
            )
            .expect("create unwritten extent");

        let mappings = filesystem
            .inode_extents(file.number, 0, u64::MAX, FileExtentTarget::Data, 8)
            .expect("inspect Linux-image file extents");
        assert_eq!(mappings.mapped_extents, 3);
        assert!(mappings.complete);
        assert_eq!(mappings.extents[0].logical_start, 0);
        assert_eq!(mappings.extents[1].logical_start, 2 * block_size);
        assert_eq!(mappings.extents[2].logical_start, 4 * block_size);
        assert!(
            mappings
                .extents
                .iter()
                .all(|extent| extent.length == block_size)
        );
        assert_eq!(mappings.extents[2].state, FileExtentState::Unwritten);

        let partial = filesystem
            .inode_extents(
                file.number,
                block_size / 2,
                2 * block_size,
                FileExtentTarget::Data,
                8,
            )
            .expect("inspect non-aligned extent range");
        assert_eq!(partial.mapped_extents, 2);
        assert!(partial.complete);
        assert_eq!(partial.extents[0].logical_start, 0);
        assert_eq!(partial.extents[0].length, block_size);
        assert_eq!(partial.extents[1].logical_start, 2 * block_size);
        assert_eq!(partial.extents[1].length, block_size);

        filesystem.unmount().expect("unmount FIEMAP image");
        file.number
    };

    e2fsck_readonly_clean(
        &image,
        &format!("{filesystem_block_size}-byte file extent map"),
    );
    {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let services = MountServices::new(TestClock(Cell::new(1_900_000_000)), (), NoopObserver);
        let mut filesystem = Ext4::mount(device, services, MountOptions::read_write())
            .expect("remount FIEMAP image");
        let mappings = filesystem
            .inode_extents(inode_number, 0, u64::MAX, FileExtentTarget::Data, 0)
            .expect("count remounted file extents");
        assert_eq!(mappings.mapped_extents, 3);
        assert!(mappings.extents.is_empty());
        filesystem
            .unmount()
            .expect("unmount remounted FIEMAP image");
    }
    e2fsck_readonly_clean(
        &image,
        &format!("remounted {filesystem_block_size}-byte file extent map"),
    );
    fs::remove_dir_all(temp_dir).expect("remove FIEMAP temp dir");
}

fn file_xattr_extent_map_geometry_round_trip(filesystem_block_size: u32) {
    for tool in ["mkfs.ext4", "debugfs", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) =
        create_ext4_geometry_image("rsext4-file-xattr-extent-map", "64M", filesystem_block_size);
    let source = temp_dir.join("source.bin");
    fs::write(&source, b"xattr FIEMAP fixture").expect("write xattr FIEMAP source");
    let external_value = "x".repeat(200);
    run_debugfs_script(
        &image,
        &format!(
            concat!(
                "write {} /fiemap-xattr.bin\n",
                "ea_set /fiemap-xattr.bin user.fiemap inline-value\n",
                "write {} /fiemap-external-xattr.bin\n",
                "ea_set /fiemap-external-xattr.bin user.fiemap {}\n",
                "set_inode_field /fiemap-external-xattr.bin extra_isize 0\n",
                "write {} /fiemap-no-xattr.bin\n"
            ),
            source.display(),
            source.display(),
            external_value,
            source.display(),
        ),
        "create xattr FIEMAP fixtures",
    );
    e2fsck_readonly_clean(
        &image,
        &format!("{filesystem_block_size}-byte inline-xattr FIEMAP fixture"),
    );

    let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
    let services = MountServices::new(TestClock(Cell::new(1_950_000_000)), (), NoopObserver);
    let mut filesystem =
        Ext4::mount(device, services, MountOptions::read_write()).expect("mount xattr image");
    let file = filesystem
        .lookup_child(
            filesystem.root_inode(),
            FileName::new(b"fiemap-xattr.bin").expect("valid file name"),
        )
        .expect("lookup xattr FIEMAP fixture")
        .expect("xattr FIEMAP fixture must exist");
    assert_eq!(
        filesystem
            .get_xattr(file.number, XattrNamespace::User, b"fiemap")
            .expect("read inline xattr"),
        b"inline-value"
    );
    assert_eq!(
        filesystem
            .list_xattrs(file.number)
            .expect("list inline xattrs"),
        vec![XattrName {
            namespace: XattrNamespace::User,
            name: b"fiemap".to_vec(),
        }]
    );
    let mappings = filesystem
        .inode_extents(
            file.number,
            0,
            u64::MAX,
            FileExtentTarget::ExtendedAttributes,
            1,
        )
        .expect("inspect inline-xattr extent");
    assert_eq!(mappings.mapped_extents, 1);
    assert!(mappings.complete);
    assert_eq!(mappings.extents.len(), 1);
    let mapping = mappings.extents[0];
    assert_eq!(mapping.logical_start, 0);
    assert_eq!(mapping.state, FileExtentState::Inline);
    assert_eq!(mapping.length, 96);
    assert_eq!(
        mapping.physical_start % u64::from(filesystem_block_size),
        160,
        "Linux 7.1 omits the inode-table slot offset from inline-xattr FIEMAP physical addresses"
    );
    assert!(!mapping.merged);

    let count_only = filesystem
        .inode_extents(
            file.number,
            0,
            u64::MAX,
            FileExtentTarget::ExtendedAttributes,
            0,
        )
        .expect("count inline-xattr extent");
    assert_eq!(count_only.mapped_extents, 1);
    assert!(count_only.extents.is_empty());
    assert!(count_only.complete);

    let after_inline = filesystem
        .inode_extents(
            file.number,
            mapping.length,
            u64::MAX,
            FileExtentTarget::ExtendedAttributes,
            1,
        )
        .expect("query after inline-xattr extent");
    assert_eq!(after_inline.mapped_extents, 0);
    assert!(after_inline.extents.is_empty());
    assert!(after_inline.complete);

    let external_file = filesystem
        .lookup_child(
            filesystem.root_inode(),
            FileName::new(b"fiemap-external-xattr.bin").expect("valid file name"),
        )
        .expect("lookup external-xattr FIEMAP fixture")
        .expect("external-xattr FIEMAP fixture must exist");
    assert_eq!(
        filesystem
            .get_xattr(external_file.number, XattrNamespace::User, b"fiemap")
            .expect("read external xattr"),
        external_value.as_bytes()
    );
    assert_eq!(
        filesystem
            .list_xattrs(external_file.number)
            .expect("list external xattrs"),
        vec![XattrName {
            namespace: XattrNamespace::User,
            name: b"fiemap".to_vec(),
        }]
    );
    let external = filesystem
        .inode_extents(
            external_file.number,
            0,
            u64::MAX,
            FileExtentTarget::ExtendedAttributes,
            1,
        )
        .expect("inspect external-xattr extent");
    assert_eq!(external.mapped_extents, 1);
    assert!(external.complete);
    assert_eq!(external.extents.len(), 1);
    assert_eq!(external.extents[0].logical_start, 0);
    assert_eq!(external.extents[0].state, FileExtentState::Initialized);
    assert_eq!(external.extents[0].length, u64::from(filesystem_block_size));
    assert_eq!(
        external.extents[0].physical_start % u64::from(filesystem_block_size),
        0
    );
    assert!(!external.extents[0].merged);

    let no_xattr_file = filesystem
        .lookup_child(
            filesystem.root_inode(),
            FileName::new(b"fiemap-no-xattr.bin").expect("valid file name"),
        )
        .expect("lookup no-xattr FIEMAP fixture")
        .expect("no-xattr FIEMAP fixture must exist");
    assert!(
        filesystem
            .list_xattrs(no_xattr_file.number)
            .expect("list absent xattrs")
            .is_empty()
    );
    assert_eq!(
        filesystem
            .get_xattr(no_xattr_file.number, XattrNamespace::User, b"fiemap")
            .expect_err("missing xattr must be reported")
            .kind(),
        Ext4ErrorKind::NotFound
    );
    let no_xattr = filesystem
        .inode_extents(
            no_xattr_file.number,
            0,
            u64::MAX,
            FileExtentTarget::ExtendedAttributes,
            1,
        )
        .expect("inspect inode without xattrs");
    assert_eq!(no_xattr.mapped_extents, 0);
    assert!(no_xattr.extents.is_empty());
    assert!(no_xattr.complete);

    assert_eq!(
        filesystem
            .set_xattr(
                file.number,
                XattrNamespace::User,
                b"fiemap",
                b"duplicate",
                XattrSetMode::Create,
            )
            .expect_err("CREATE must reject an existing attribute")
            .kind(),
        Ext4ErrorKind::AlreadyExists
    );
    assert_eq!(
        filesystem
            .set_xattr(
                file.number,
                XattrNamespace::User,
                b"missing",
                b"value",
                XattrSetMode::Replace,
            )
            .expect_err("REPLACE must reject a missing attribute")
            .kind(),
        Ext4ErrorKind::NotFound
    );

    filesystem
        .set_xattr(
            file.number,
            XattrNamespace::User,
            b"rsext4",
            b"small",
            XattrSetMode::Create,
        )
        .expect("create inline xattr");
    assert_eq!(
        filesystem
            .get_xattr(file.number, XattrNamespace::User, b"rsext4")
            .expect("read created inline xattr"),
        b"small"
    );

    let free_before_external = filesystem.statfs().free_blocks;
    // This value fits in one external block by itself, but does not fit there
    // together with the pre-existing inline sibling. Linux keeps that sibling
    // in the inode body instead of migrating the whole xattr set.
    let large_value = vec![b'z'; filesystem_block_size as usize - 80];
    filesystem
        .set_xattr(
            file.number,
            XattrNamespace::User,
            b"rsext4",
            &large_value,
            XattrSetMode::Replace,
        )
        .expect("move the enlarged xattr to an external block");
    assert_eq!(
        filesystem
            .get_xattr(file.number, XattrNamespace::User, b"rsext4")
            .expect("read externalized xattr"),
        large_value
    );
    assert_eq!(filesystem.statfs().free_blocks + 1, free_before_external);
    assert_eq!(
        filesystem
            .inode_extents(
                file.number,
                0,
                u64::MAX,
                FileExtentTarget::ExtendedAttributes,
                1,
            )
            .expect("inspect split inline and external xattrs")
            .extents[0]
            .state,
        FileExtentState::Inline,
        "Linux FIEMAP_XATTR reports the inode-body store before i_file_acl"
    );

    filesystem
        .set_xattr(
            file.number,
            XattrNamespace::User,
            b"rsext4",
            b"inline-again",
            XattrSetMode::Replace,
        )
        .expect("move the reduced xattr back into the inode body");
    assert_eq!(filesystem.statfs().free_blocks, free_before_external);
    assert_eq!(
        filesystem
            .get_xattr(file.number, XattrNamespace::User, b"rsext4")
            .expect("read re-inlined xattr"),
        b"inline-again"
    );
    assert_eq!(
        filesystem
            .get_xattr(file.number, XattrNamespace::User, b"fiemap")
            .expect("preserve sibling xattr during migration"),
        b"inline-value"
    );
    assert_eq!(
        filesystem
            .inode_extents(
                file.number,
                0,
                u64::MAX,
                FileExtentTarget::ExtendedAttributes,
                1,
            )
            .expect("inspect migrated inline xattr")
            .extents[0]
            .state,
        FileExtentState::Inline
    );
    filesystem
        .remove_xattr(file.number, XattrNamespace::User, b"rsext4")
        .expect("remove re-inlined xattr");
    assert_eq!(
        filesystem
            .remove_xattr(file.number, XattrNamespace::User, b"rsext4",)
            .expect_err("remove must report a missing xattr")
            .kind(),
        Ext4ErrorKind::NotFound
    );

    filesystem
        .write_inode(file.number, 0, b"updated")
        .expect("mutate inode with inline xattr");

    filesystem.unmount().expect("unmount xattr image");
    let inline_attrs = debugfs_query(&image, "ea_list /fiemap-xattr.bin");
    assert!(
        inline_attrs.contains("user.fiemap") && inline_attrs.contains("inline-value"),
        "ordinary inode updates must preserve inline xattrs\n{inline_attrs}"
    );
    e2fsck_readonly_clean(
        &image,
        &format!("remounted {filesystem_block_size}-byte inline-xattr FIEMAP"),
    );
    fs::remove_dir_all(temp_dir).expect("remove xattr FIEMAP temp dir");
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
fn file_extent_map_round_trip_with_1k_filesystem_blocks() {
    file_extent_map_geometry_round_trip(1024);
}

#[test]
fn file_extent_map_round_trip_with_2k_filesystem_blocks() {
    file_extent_map_geometry_round_trip(2048);
}

#[test]
fn file_extent_map_round_trip_with_4k_filesystem_blocks() {
    file_extent_map_geometry_round_trip(4096);
}

#[test]
fn file_xattr_extent_map_round_trip_with_1k_filesystem_blocks() {
    file_xattr_extent_map_geometry_round_trip(1024);
}

#[test]
fn file_xattr_extent_map_round_trip_with_2k_filesystem_blocks() {
    file_xattr_extent_map_geometry_round_trip(2048);
}

#[test]
fn file_xattr_extent_map_round_trip_with_4k_filesystem_blocks() {
    file_xattr_extent_map_geometry_round_trip(4096);
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
        let services = MountServices::new(TestClock(Cell::new(1_800_000_000)), (), NoopObserver);
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

#[test]
fn owned_project_metadata_and_inheritance_are_linux_readable() {
    for tool in ["mkfs.ext4", "debugfs", "e2fsck", "truncate"] {
        require_tool(tool);
    }
    let (temp_dir, image) = create_ext4_test_image_with_args(
        "rsext4-project-metadata",
        "64M",
        &["-I", "256", "-O", "project"],
    );

    {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let services = MountServices::new(TestClock(Cell::new(1_800_000_000)), (), NoopObserver);
        let mut filesystem =
            Ext4::mount(device, services, MountOptions::read_write()).expect("mount Linux image");
        let context = MutationContext::new(1000, 1001, 0, 0o022);
        let project_directory = filesystem
            .create_directory(
                context,
                filesystem.root_inode(),
                FileName::new(b"project-root").expect("valid project directory name"),
                FilePermissions::new(0o755).expect("valid project directory permissions"),
            )
            .expect("create project directory");
        let project_directory = filesystem
            .update_inode_metadata(
                project_directory.number,
                InodeMetadataUpdate {
                    project_id: Some(1234),
                    flags: Some(InodeFlags::PROJECT_INHERIT),
                    ..Default::default()
                },
            )
            .expect("set project metadata");
        assert_eq!(project_directory.project_id, 1234);
        assert!(
            project_directory
                .flags
                .contains(InodeFlags::PROJECT_INHERIT)
        );

        let child = filesystem
            .create_regular_file(
                context,
                project_directory.number,
                FileName::new(b"child").expect("valid project child name"),
                FilePermissions::new(0o600).expect("valid project child permissions"),
            )
            .expect("create project child");
        assert_eq!(child.project_id, 1234);
        filesystem
            .unmount()
            .expect("unmount project metadata image");
    }

    e2fsck_readonly_clean(&image, "rsext4 project metadata");
    let directory_stat = debugfs_query(&image, "stat /project-root");
    assert!(
        directory_stat.contains("Project:  1234"),
        "{directory_stat}"
    );
    assert!(
        directory_stat.contains("Flags: 0x20080000"),
        "Linux did not decode PROJINHERIT on the project directory\n{directory_stat}"
    );
    let child_stat = debugfs_query(&image, "stat /project-root/child");
    assert!(child_stat.contains("Project:  1234"), "{child_stat}");
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

fn inject_checksum_journal(
    image: &Path,
    target_blocks: &[u64],
    payload: &Path,
    checksum_version: u8,
) {
    assert!(matches!(checksum_version, 2 | 3));
    let blocks = target_blocks
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        "journal_open -c -v {checksum_version}\njournal_write -b {blocks} \
         {}\njournal_close\nquit\n",
        payload.display()
    );
    run_debugfs_script(image, &script, "inject checksummed journal");
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

fn replay_checksum_journal_from_debugfs(checksum_version: u8) {
    for tool in ["mkfs.ext4", "debugfs", "e2fsck"] {
        require_tool(tool);
    }

    let temp_dir = std::env::temp_dir().join(format!(
        "rsext4-checksum-v{checksum_version}-journal-repro-{}",
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
            command.args(["-F", "-q", "-b", "4096"]);
            if checksum_version == 2 {
                command.args(["-O", "^metadata_csum,^64bit"]);
            }
            command.arg(&image);
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
    inject_checksum_journal(&image, &changed_blocks, &payload, checksum_version);
    let dirty_header = dumpe2fs_header(&image, "pending journal fixture");
    assert!(
        dirty_header.contains("needs_recovery"),
        "debugfs journal fixture should require recovery\n{dirty_header}"
    );
    if checksum_version == 2 {
        assert!(
            dirty_header.contains("Journal features:         journal_checksum")
                && dirty_header.contains("Journal checksum type:    crc32"),
            "e2fsprogs v2 fixture should use FEATURE_COMPAT_CHECKSUM\n{dirty_header}"
        );
    }

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
        let fs =
            Ext4FileSystem::mount(&mut dev).expect("mount image with pending checksummed journal");
        umount(fs, &mut dev).expect("umount image after replay");
    }

    assert_debugfs_path_exists(&image, "/replay-repro/a");
    assert_debugfs_path_exists(&image, "/replay-repro/b");
    let recovered_header = dumpe2fs_header(&image, "rsext4 journal replay");
    assert!(
        !recovered_header.contains("needs_recovery"),
        "rsext4 should clear needs_recovery after successful replay\n{recovered_header}"
    );
    e2fsck_readonly_clean(&image, "rsext4 checksummed journal replay");
    fs::remove_dir_all(temp_dir).expect("remove temp dir");
}

#[test]
fn replay_compat_checksum_multi_block_journal_from_debugfs() {
    replay_checksum_journal_from_debugfs(2);
}

#[test]
fn replay_csum_v3_multi_block_journal_from_debugfs() {
    replay_checksum_journal_from_debugfs(3);
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
        let mut fs = Ext4FileSystem::mount(&mut dev).expect("mount image");

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
        let mut fs = Ext4FileSystem::mount(&mut dev).expect("mount image");

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
        let mut fs = Ext4FileSystem::mount(&mut dev).expect("remount sparse growth image");
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
        let mut fs = Ext4FileSystem::mount(&mut dev).expect("mount image");
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
        let mut fs = Ext4FileSystem::mount(&mut dev).expect("remount legacy image");
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
fn legacy_sparse_truncate_prunes_single_double_and_triple_roots() {
    for tool in ["mkfs.ext4", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_test_image("rsext4-legacy-truncate-roots", "64M");
    let pointers_per_block = (BLOCK_SIZE / size_of::<u32>()) as u64;
    let double_capacity = pointers_per_block * pointers_per_block;
    let cases = [
        ("single", 1usize, 12u64),
        ("double", 2, 12 + pointers_per_block),
        ("triple", 3, 12 + pointers_per_block + double_capacity),
    ];

    {
        let dev = FileBlockDevice::open(image.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = Ext4FileSystem::mount(&mut dev).expect("mount image");
        let huge_file = fs
            .superblock
            .has_feature_ro_compat(superblock::Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);

        for (name, depth, root_start) in cases {
            let path = format!("/legacy-{name}-truncate.bin");
            mkfile(&mut dev, &mut fs, &path, None, None).expect("create legacy truncate file");
            let inode_number = dir::get_inode_with_num(&mut fs, &mut dev, &path)
                .expect("lookup legacy truncate file")
                .expect("legacy truncate inode")
                .0;
            fs.modify_inode(&mut dev, inode_number, |inode| {
                inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
                inode.i_block = [0; 15];
                inode.i_blocks_lo = 0;
                inode.l_i_blocks_high = 0;
            })
            .expect("convert empty inode to legacy mapping");

            let retained_marker = [0x40 | depth as u8];
            let removed_marker = [0x80 | depth as u8];
            write_file(&mut dev, &mut fs, &path, 0, &retained_marker)
                .expect("write retained direct marker");
            write_file(
                &mut dev,
                &mut fs,
                &path,
                root_start * BLOCK_SIZE as u64,
                &removed_marker,
            )
            .expect("write sparse indirect marker");

            let mut marker = [0u8; 1];
            assert_eq!(
                read_inode_data_into(&mut dev, &mut fs, inode_number, 0, &mut marker)
                    .expect("read direct marker before truncate"),
                1
            );
            assert_eq!(marker, retained_marker);
            assert_eq!(
                read_inode_data_into(
                    &mut dev,
                    &mut fs,
                    inode_number,
                    root_start * BLOCK_SIZE as u64,
                    &mut marker,
                )
                .expect("read indirect marker before truncate"),
                1
            );
            assert_eq!(marker, removed_marker);

            let inode = fs
                .get_inode_by_num(&mut dev, inode_number)
                .expect("read allocated legacy inode");
            assert!(!inode.uses_extents());
            assert_ne!(inode.i_block[11 + depth], 0, "{name} root must exist");
            assert_eq!(
                inode.blocks_count(BLOCK_SIZE as u32, huge_file),
                (depth as u64 + 2) * (BLOCK_SIZE / 512) as u64,
                "two data blocks plus the {depth}-level metadata path must be accounted"
            );

            truncate(&mut dev, &mut fs, &path, root_start * BLOCK_SIZE as u64)
                .expect("truncate sparse legacy root");
            let inode = fs
                .get_inode_by_num(&mut dev, inode_number)
                .expect("read truncated legacy inode");
            assert_eq!(inode.i_block[11 + depth], 0, "{name} root must be pruned");
            assert_eq!(
                inode.blocks_count(BLOCK_SIZE as u32, huge_file),
                (BLOCK_SIZE / 512) as u64,
                "only the retained direct data block may remain"
            );
            assert_eq!(
                read_inode_data_into(&mut dev, &mut fs, inode_number, 0, &mut marker)
                    .expect("read direct marker after truncate"),
                1
            );
            assert_eq!(marker, retained_marker);
        }

        umount(fs, &mut dev).expect("umount legacy truncate image");
    }

    e2fsck_readonly_clean(&image, "legacy indirect root truncation");

    {
        let dev = FileBlockDevice::open(image.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = Ext4FileSystem::mount(&mut dev).expect("remount legacy truncate image");
        for (name, depth, root_start) in cases {
            let path = format!("/legacy-{name}-truncate.bin");
            let (inode_number, inode) = dir::get_inode_with_num(&mut fs, &mut dev, &path)
                .expect("lookup remounted legacy truncate file")
                .expect("remounted legacy truncate inode");
            assert!(!inode.uses_extents());
            assert_eq!(inode.size(), root_start * BLOCK_SIZE as u64);
            assert_eq!(inode.i_block[11 + depth], 0);

            let mut marker = [0u8; 1];
            assert_eq!(
                read_inode_data_into(&mut dev, &mut fs, inode_number, 0, &mut marker)
                    .expect("read retained direct marker"),
                1
            );
            assert_eq!(marker, [0x40 | depth as u8]);
            assert_eq!(
                read_inode_data_into(
                    &mut dev,
                    &mut fs,
                    inode_number,
                    root_start * BLOCK_SIZE as u64,
                    &mut marker,
                )
                .expect("read at truncated EOF"),
                0
            );
        }
        umount(fs, &mut dev).expect("umount remounted legacy truncate image");
    }

    e2fsck_readonly_clean(&image, "remounted legacy indirect root truncation");
    fs::remove_dir_all(temp_dir).expect("remove temp dir");
}

#[test]
fn legacy_orphan_recovery_truncates_linked_and_reaps_unlinked_inode() {
    for tool in ["mkfs.ext4", "dumpe2fs", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, image) = create_ext4_test_image("rsext4-legacy-orphan-recovery", "64M");
    let linked_path = "/legacy-linked-orphan.bin";
    let unlinked_path = "/legacy-unlinked-orphan.bin";
    let (linked_inode, unlinked_inode, free_blocks_after_recovery) = {
        let dev = FileBlockDevice::open(image.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = Ext4FileSystem::mount(&mut dev).expect("mount legacy orphan image");

        let mut inode_numbers = [None; 2];
        let free_blocks_before_mapping = fs.superblock.free_blocks_count();
        for (index, (path, marker)) in [(linked_path, 0x51u8), (unlinked_path, 0x61u8)]
            .into_iter()
            .enumerate()
        {
            mkfile(&mut dev, &mut fs, path, None, None).expect("create legacy orphan file");
            let inode_number = dir::get_inode_with_num(&mut fs, &mut dev, path)
                .expect("lookup legacy orphan file")
                .expect("legacy orphan inode")
                .0;
            inode_numbers[index] = Some(inode_number);
            fs.modify_inode(&mut dev, inode_number, |inode| {
                inode.i_flags &= !disknode::Ext4Inode::EXT4_EXTENTS_FL;
                inode.i_block = [0; 15];
                inode.i_blocks_lo = 0;
                inode.l_i_blocks_high = 0;
            })
            .expect("convert orphan fixture to legacy mapping");
            write_file(&mut dev, &mut fs, path, 0, &[marker])
                .expect("write retained direct marker");
            write_file(
                &mut dev,
                &mut fs,
                path,
                12 * BLOCK_SIZE as u64,
                &[marker | 0x80],
            )
            .expect("write removable single-indirect marker");
        }
        let [linked_inode, unlinked_inode] = inode_numbers;
        let linked_inode = linked_inode.expect("linked fixture inode");
        let unlinked_inode = unlinked_inode.expect("unlinked fixture inode");
        assert_eq!(
            fs.superblock.free_blocks_count(),
            free_blocks_before_mapping - 6,
            "each fixture owns two data blocks and one pointer block"
        );

        fs.modify_inode(&mut dev, linked_inode, |inode| {
            inode.i_size_lo = BLOCK_SIZE as u32;
            inode.i_size_high = 0;
            inode.i_dtime = 0;
        })
        .expect("publish shortened linked orphan size");
        fs.superblock.s_last_orphan = linked_inode.raw();
        let outcome =
            unlink(&mut fs, &mut dev, unlinked_path).expect("publish zero-link legacy orphan");
        assert_eq!(outcome.inode, unlinked_inode);
        assert!(outcome.requires_reap());
        assert_eq!(fs.superblock.s_last_orphan, unlinked_inode.raw());
        assert_eq!(
            fs.get_inode_by_num(&mut dev, unlinked_inode)
                .expect("read unlinked orphan")
                .i_dtime,
            linked_inode.raw()
        );

        fs.sync_filesystem(&mut dev)
            .expect("persist dirty legacy orphan transaction");
        dev.umount_commit()
            .expect("commit dirty legacy orphan journal");
        drop(fs);
        drop(dev);

        (linked_inode, unlinked_inode, free_blocks_before_mapping - 1)
    };

    let dirty_header = dumpe2fs_header(&image, "legacy orphan recovery fixture");
    assert!(
        dirty_header.contains("needs_recovery"),
        "unclean orphan fixture must require journal recovery\n{dirty_header}"
    );

    {
        let dev = FileBlockDevice::open(image.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = Ext4FileSystem::mount(&mut dev).expect("recover legacy orphan image");
        assert_eq!(fs.superblock.s_last_orphan, 0);
        assert_eq!(
            fs.superblock.free_blocks_count(),
            free_blocks_after_recovery
        );
        assert!(
            fs.inode_num_already_allocated(&mut dev, linked_inode)
                .expect("linked inode allocation lookup")
        );
        assert!(
            !fs.inode_num_already_allocated(&mut dev, unlinked_inode)
                .expect("unlinked inode allocation lookup")
        );
        assert!(
            dir::get_inode_with_num(&mut fs, &mut dev, unlinked_path)
                .expect("lookup recovered unlinked path")
                .is_none()
        );

        let inode = fs
            .get_inode_by_num(&mut dev, linked_inode)
            .expect("read recovered linked inode");
        assert_eq!(inode.i_links_count, 1);
        assert_eq!(inode.i_dtime, 0);
        assert_eq!(inode.size(), BLOCK_SIZE as u64);
        assert_eq!(inode.i_block[12], 0);
        assert_eq!(inode.i_blocks_lo, (BLOCK_SIZE / 512) as u32);
        let mut marker = [0u8; 1];
        assert_eq!(
            read_inode_data_into(&mut dev, &mut fs, linked_inode, 0, &mut marker)
                .expect("read recovered linked marker"),
            1
        );
        assert_eq!(marker, [0x51]);
        assert_eq!(
            read_inode_data_into(
                &mut dev,
                &mut fs,
                linked_inode,
                BLOCK_SIZE as u64,
                &mut marker,
            )
            .expect("read recovered linked EOF"),
            0
        );
        umount(fs, &mut dev).expect("unmount recovered legacy orphan image");
    }

    e2fsck_readonly_clean(&image, "legacy linked and unlinked orphan recovery");
    assert_debugfs_path_exists(&image, linked_path);
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
        let mut fs = Ext4FileSystem::mount(&mut dev).expect("mount image");

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
        let mut fs = Ext4FileSystem::mount(&mut dev).expect("mount image");

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
fn repro_linux_image_create_write_rename_then_e2fsck() {
    for tool in ["mkfs.ext4", "debugfs", "e2fsck", "truncate"] {
        require_tool(tool);
    }

    let (temp_dir, dst) = match std::env::var_os("RSEXT4_TEST_IMAGE").map(PathBuf::from) {
        Some(src) => {
            assert!(src.exists(), "test image does not exist: {}", src.display());
            let temp_dir = std::env::temp_dir()
                .join(format!("rsext4-linux-image-fixture-{}", std::process::id()));
            if temp_dir.exists() {
                fs::remove_dir_all(&temp_dir).expect("remove stale fixture temp dir");
            }
            fs::create_dir(&temp_dir).expect("create fixture temp dir");
            let dst = temp_dir.join("fs.img");
            fs::copy(&src, &dst).expect("copy test image");
            (temp_dir, dst)
        }
        None => create_ext4_test_image("rsext4-linux-image-repro", "64M"),
    };
    repair_baseline_image(&dst);

    {
        let dev = FileBlockDevice::open(dst.clone());
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev, true);
        let mut fs = Ext4FileSystem::mount(&mut dev).expect("mount image");

        if dir::get_inode_with_num(&mut fs, &mut dev, "/root")
            .expect("lookup root fixture directory")
            .is_none()
        {
            mkdir(&mut dev, &mut fs, "/root").expect("create root fixture directory");
        }

        let probe = "/root/rsext4-fsck-probe";
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
            RenameOptions::REPLACE,
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

    fs::remove_dir_all(temp_dir).expect("remove Linux image temp dir");
}

#[test]
fn unwritten_preallocation_partial_write_remounts_and_passes_e2fsck() {
    for tool in ["mkfs.ext4", "e2fsck", "truncate"] {
        require_tool(tool);
    }
    let (temp_dir, image) = create_ext4_test_image("rsext4-unwritten-preallocation", "64M");
    let inode_number = {
        let device = FileBlockDevice::open(image.clone());
        let mut device = Jbd2Dev::initial_jbd2dev(0, device, true);
        let mut filesystem = Ext4FileSystem::mount(&mut device).expect("mount image");
        mkfile(&mut device, &mut filesystem, "/preallocated", None, None)
            .expect("create preallocated file");
        let inode_number = dir::get_inode_with_num(&mut filesystem, &mut device, "/preallocated")
            .expect("lookup preallocated file")
            .expect("preallocated file missing")
            .0;
        preallocate_inode(
            &mut device,
            &mut filesystem,
            inode_number,
            0,
            3 * BLOCK_SIZE as u64,
            PreallocationOptions::EXTEND_SIZE,
        )
        .expect("preallocate file");
        write_inode_data(
            &mut device,
            &mut filesystem,
            inode_number,
            BLOCK_SIZE as u64 + 97,
            b"linux-unwritten-parity",
        )
        .expect("write middle unwritten block");
        let data = read_file(&mut device, &mut filesystem, "/preallocated")
            .expect("read preallocated file");
        assert_eq!(data.len(), 3 * BLOCK_SIZE);
        assert!(data[..BLOCK_SIZE + 97].iter().all(|byte| *byte == 0));
        assert_eq!(
            &data[BLOCK_SIZE + 97..BLOCK_SIZE + 119],
            b"linux-unwritten-parity"
        );
        assert!(data[BLOCK_SIZE + 119..].iter().all(|byte| *byte == 0));
        umount(filesystem, &mut device).expect("unmount preallocated image");
        inode_number
    };

    e2fsck_readonly_clean(&image, "unwritten preallocation after partial write");

    {
        let device = FileBlockDevice::open(image.clone());
        let mut device = Jbd2Dev::initial_jbd2dev(0, device, true);
        let mut filesystem =
            Ext4FileSystem::mount(&mut device).expect("remount preallocated image");
        let data = read_file(&mut device, &mut filesystem, "/preallocated")
            .expect("read remounted preallocated file");
        assert_eq!(data.len(), 3 * BLOCK_SIZE);
        assert!(data[..BLOCK_SIZE + 97].iter().all(|byte| *byte == 0));
        assert_eq!(
            &data[BLOCK_SIZE + 97..BLOCK_SIZE + 119],
            b"linux-unwritten-parity"
        );
        assert!(data[BLOCK_SIZE + 119..].iter().all(|byte| *byte == 0));

        let mut inode = filesystem
            .get_inode_by_num(&mut device, inode_number)
            .expect("read remounted inode");
        let mut tree =
            extents_tree::ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number);
        assert!(
            tree.find_extent(&mut device, 0)
                .expect("left lookup")
                .expect("left extent")
                .is_unwritten()
        );
        assert!(
            tree.find_extent(&mut device, 1)
                .expect("middle lookup")
                .expect("middle extent")
                .is_initialized()
        );
        assert!(
            tree.find_extent(&mut device, 2)
                .expect("right lookup")
                .expect("right extent")
                .is_unwritten()
        );
        umount(filesystem, &mut device).expect("unmount remounted image");
    }
    e2fsck_readonly_clean(&image, "remounted unwritten preallocation");
    fs::remove_dir_all(temp_dir).expect("remove unwritten temp dir");
}

#[test]
fn linux_uninit_bg_image_mounts_writable_and_remains_clean() {
    for tool in ["mkfs.ext4", "dumpe2fs", "e2fsck", "truncate"] {
        require_tool(tool);
    }
    let (temp_dir, image) = create_ext4_test_image_with_args(
        "rsext4-uninit-bg",
        "256M",
        &["-O", "^metadata_csum,uninit_bg", "-E", "lazy_itable_init=1"],
    );
    let dump = run_command(
        {
            let mut command = Command::new("dumpe2fs");
            command.arg(&image);
            command
        },
        "dumpe2fs uninit_bg fixture",
    );
    let dump = command_text(&dump);
    assert!(
        dump.contains("uninit_bg"),
        "fixture lacks uninit_bg\n{dump}"
    );
    assert!(
        !dump.contains("metadata_csum"),
        "fixture unexpectedly enables metadata_csum\n{dump}"
    );
    assert!(
        dump.contains("[INODE_UNINIT, ITABLE_ZEROED]"),
        "fixture lacks a lazily initialized group\n{dump}"
    );

    {
        let device = FileBlockDevice::open(image.clone());
        let mut device = Jbd2Dev::initial_jbd2dev(0, device, true);
        let mut filesystem =
            Ext4FileSystem::mount(&mut device).expect("mount legacy uninit_bg image writable");
        mkfile(&mut device, &mut filesystem, "/uninit-bg", None, None)
            .expect("create file on uninit_bg image");
        write_file(
            &mut device,
            &mut filesystem,
            "/uninit-bg",
            0,
            b"linux-gdt-csum-parity",
        )
        .expect("write file on uninit_bg image");
        umount(filesystem, &mut device).expect("unmount uninit_bg image");
    }

    e2fsck_readonly_clean(&image, "legacy uninit_bg write");
    {
        let device = FileBlockDevice::open(image.clone());
        let mut device = Jbd2Dev::initial_jbd2dev(0, device, true);
        let mut filesystem =
            Ext4FileSystem::mount(&mut device).expect("remount legacy uninit_bg image");
        assert_eq!(
            read_file(&mut device, &mut filesystem, "/uninit-bg")
                .expect("read file from remounted uninit_bg image"),
            b"linux-gdt-csum-parity"
        );
        umount(filesystem, &mut device).expect("unmount remounted uninit_bg image");
    }
    e2fsck_readonly_clean(&image, "remounted legacy uninit_bg image");
    fs::remove_dir_all(temp_dir).expect("remove uninit_bg temp dir");
}

fn shifted_range_geometry_round_trip(filesystem_block_size: u32) {
    for tool in ["mkfs.ext4", "e2fsck", "truncate"] {
        require_tool(tool);
    }
    let (temp_dir, image) = create_ext4_geometry_image(
        "rsext4-shifted-range-geometry",
        "64M",
        filesystem_block_size,
    );
    let block_size = filesystem_block_size as usize;
    let mut original = vec![0; 4 * block_size];
    for (index, block) in original.chunks_exact_mut(block_size).enumerate() {
        block.fill(index as u8 + 1);
    }
    let mut expected = vec![0; 4 * block_size];
    expected[..block_size].copy_from_slice(&original[..block_size]);
    expected[2 * block_size..].copy_from_slice(&original[2 * block_size..]);

    {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let mut device = Jbd2Dev::initial_jbd2dev(0, device, true);
        let mut filesystem = Ext4FileSystem::mount(&mut device).expect("mount shifted-range image");
        mkfile(&mut device, &mut filesystem, "/shifted.bin", None, None)
            .expect("create shifted-range file");
        write_file(&mut device, &mut filesystem, "/shifted.bin", 0, &original)
            .expect("write shifted-range fixture");
        let inode_number = dir::get_inode_with_num(&mut filesystem, &mut device, "/shifted.bin")
            .expect("lookup shifted-range file")
            .expect("shifted-range file missing")
            .0;
        operate_inode_range(
            &mut device,
            &mut filesystem,
            inode_number,
            u64::from(filesystem_block_size),
            u64::from(filesystem_block_size),
            RangeOperation::Collapse,
        )
        .expect("collapse one filesystem block");
        operate_inode_range(
            &mut device,
            &mut filesystem,
            inode_number,
            u64::from(filesystem_block_size),
            u64::from(filesystem_block_size),
            RangeOperation::Insert,
        )
        .expect("insert one filesystem block");
        assert_eq!(
            read_file(&mut device, &mut filesystem, "/shifted.bin")
                .expect("read shifted-range result"),
            expected
        );
        umount(filesystem, &mut device).expect("unmount shifted-range image");
    }

    e2fsck_readonly_clean(
        &image,
        &format!("{filesystem_block_size}-byte shifted ranges"),
    );
    {
        let device = FileBlockDevice::open_with_sector_size(image.clone(), 512);
        let mut device = Jbd2Dev::initial_jbd2dev(0, device, true);
        let mut filesystem =
            Ext4FileSystem::mount(&mut device).expect("remount shifted-range image");
        assert_eq!(
            read_file(&mut device, &mut filesystem, "/shifted.bin")
                .expect("read remounted shifted-range result"),
            expected
        );
        umount(filesystem, &mut device).expect("unmount remounted shifted-range image");
    }
    e2fsck_readonly_clean(
        &image,
        &format!("remounted {filesystem_block_size}-byte shifted ranges"),
    );
    fs::remove_dir_all(temp_dir).expect("remove shifted-range temp dir");
}

#[test]
fn shifted_ranges_round_trip_with_1k_filesystem_blocks() {
    shifted_range_geometry_round_trip(1024);
}

#[test]
fn shifted_ranges_round_trip_with_2k_filesystem_blocks() {
    shifted_range_geometry_round_trip(2048);
}

#[test]
fn shifted_ranges_round_trip_with_4k_filesystem_blocks() {
    shifted_range_geometry_round_trip(4096);
}

#[test]
fn shifted_ranges_rebuild_multiple_leaves_and_preserve_unwritten_state() {
    for tool in ["mkfs.ext4", "e2fsck", "truncate"] {
        require_tool(tool);
    }
    const MARKERS: usize = 360;
    const FILE_BLOCKS: usize = 722;
    let (temp_dir, image) = create_ext4_test_image("rsext4-shifted-range-multileaf", "64M");
    let path = "/shifted-multileaf.bin";
    let mut expected = vec![0; FILE_BLOCKS * BLOCK_SIZE];
    for marker in 0..MARKERS {
        let original_lbn = marker * 2;
        if original_lbn == 100 {
            continue;
        }
        let final_lbn = if original_lbn < 100 {
            original_lbn
        } else if original_lbn < 202 {
            original_lbn - 2
        } else {
            original_lbn
        };
        expected[final_lbn * BLOCK_SIZE] = (marker % 251 + 1) as u8;
    }

    let inode_number = {
        let device = FileBlockDevice::open(image.clone());
        let mut device = Jbd2Dev::initial_jbd2dev(0, device, true);
        let mut filesystem = Ext4FileSystem::mount(&mut device).expect("mount multi-leaf image");
        mkfile(&mut device, &mut filesystem, path, None, None).expect("create multi-leaf file");
        for marker in 0..MARKERS {
            write_file(
                &mut device,
                &mut filesystem,
                path,
                (marker * 2 * BLOCK_SIZE) as u64,
                &[(marker % 251 + 1) as u8],
            )
            .expect("write sparse extent marker");
        }
        let inode_number = dir::get_inode_with_num(&mut filesystem, &mut device, path)
            .expect("lookup multi-leaf file")
            .expect("multi-leaf file missing")
            .0;
        preallocate_inode(
            &mut device,
            &mut filesystem,
            inode_number,
            721 * BLOCK_SIZE as u64,
            BLOCK_SIZE as u64,
            PreallocationOptions::EXTEND_SIZE,
        )
        .expect("append unwritten extent");
        operate_inode_range(
            &mut device,
            &mut filesystem,
            inode_number,
            100 * BLOCK_SIZE as u64,
            2 * BLOCK_SIZE as u64,
            RangeOperation::Collapse,
        )
        .expect("collapse sparse extent range");
        operate_inode_range(
            &mut device,
            &mut filesystem,
            inode_number,
            200 * BLOCK_SIZE as u64,
            2 * BLOCK_SIZE as u64,
            RangeOperation::Insert,
        )
        .expect("insert sparse extent range");
        assert_eq!(
            read_file(&mut device, &mut filesystem, path).expect("read multi-leaf result"),
            expected
        );

        let mut inode = filesystem
            .get_inode_by_num(&mut device, inode_number)
            .expect("read shifted multi-leaf inode");
        let mut tree =
            extents_tree::ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number);
        let root = tree.load_root_from_inode().expect("parse multi-leaf root");
        match root {
            extents_tree::ExtentNode::Index { entries, .. } => assert!(
                entries.len() >= 2,
                "shifted tree must span multiple external leaves"
            ),
            extents_tree::ExtentNode::Leaf { .. } => {
                panic!("shifted tree unexpectedly fit in the inline root")
            }
        }
        assert!(
            tree.find_extent(&mut device, 721)
                .expect("lookup shifted unwritten extent")
                .expect("shifted unwritten extent missing")
                .is_unwritten()
        );
        umount(filesystem, &mut device).expect("unmount multi-leaf image");
        inode_number
    };

    e2fsck_readonly_clean(&image, "shifted multi-leaf extent tree");
    {
        let device = FileBlockDevice::open(image.clone());
        let mut device = Jbd2Dev::initial_jbd2dev(0, device, true);
        let mut filesystem = Ext4FileSystem::mount(&mut device).expect("remount multi-leaf image");
        assert_eq!(
            read_file(&mut device, &mut filesystem, path).expect("read remounted multi-leaf file"),
            expected
        );
        let mut inode = filesystem
            .get_inode_by_num(&mut device, inode_number)
            .expect("read remounted multi-leaf inode");
        assert!(
            extents_tree::ExtentTree::with_filesystem(&mut inode, &filesystem, inode_number,)
                .find_extent(&mut device, 721)
                .expect("lookup remounted unwritten extent")
                .expect("remounted unwritten extent missing")
                .is_unwritten()
        );
        umount(filesystem, &mut device).expect("unmount remounted multi-leaf image");
    }
    e2fsck_readonly_clean(&image, "remounted shifted multi-leaf extent tree");
    fs::remove_dir_all(temp_dir).expect("remove multi-leaf temp dir");
}
