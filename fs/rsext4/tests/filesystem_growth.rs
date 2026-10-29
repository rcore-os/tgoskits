//! Native ext4 growth checked against Linux-created images and e2fsck.
//! Round trips allocate beyond the old end; interrupted commits are recovered
//! through a fresh device wrapper. Large sparse targets test preflight capacity,
//! rather than claiming a completed full-size growth or a physical power-cut test.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    process::Command,
};

use rsext4::{runtime::GrowthEvent, *};

/// File-backed sector device with host durability support.
struct Image(
    /// Open host image file owned for reads, writes, capacity queries and flushes.
    File,
);

impl BlockIo for Image {
    /// Returns 512-byte sectors and the current image length in those sectors.
    fn geometry(&self) -> DeviceGeometry {
        DeviceGeometry::new(512, self.0.metadata().unwrap().len() / 512)
    }

    /// Advertises writable I/O with a real host-file flush implementation.
    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            flush: true,
            ..Default::default()
        }
    }

    /// Reads the caller-validated sector span at `sector` into `bytes`;
    /// `_count` is already represented by the exact buffer length.
    fn read(&mut self, bytes: &mut [u8], sector: SectorId, _count: u32) -> Ext4Result<()> {
        self.0
            .seek(SeekFrom::Start(sector.raw() * 512))
            .and_then(|_| self.0.read_exact(bytes))
            .map_err(|_| Ext4Error::io())
    }

    /// Writes the caller-validated `bytes` span at `sector`, forwarding host
    /// errors; `_count` is already represented by the exact buffer length.
    fn write(&mut self, bytes: &[u8], sector: SectorId, _count: u32) -> Ext4Result<()> {
        self.0
            .seek(SeekFrom::Start(sector.raw() * 512))
            .and_then(|_| self.0.write_all(bytes))
            .map_err(|_| Ext4Error::io())
    }

    /// Synchronizes the image file so later fault snapshots include its writes.
    fn flush(&mut self) -> Ext4Result<()> {
        self.0.sync_all().map_err(|_| Ext4Error::io())
    }
}

impl Clock for Image {
    /// Supplies a deterministic timestamp for reproducible mount metadata.
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        Ok(Ext4Timestamp::new(1_700_000_000, 0))
    }
}

/// Runtime clock for an owned mount, independent of its block device.
struct GrowthClock;

impl Clock for GrowthClock {
    /// Supplies the same fixed timestamp as the file-backed device clock.
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        Ok(Ext4Timestamp::new(1_700_000_000, 0))
    }
}

/// Records growth lifecycle events outside the mounted owner.
struct GrowthObserver(
    /// Ordered start and durable-commit observations from this mount.
    std::rc::Rc<std::cell::RefCell<Vec<GrowthEvent>>>,
);

impl Observer for GrowthObserver {
    /// Retains capacity progress while ignoring unrelated mount/journal events.
    fn event(&mut self, event: Event) {
        if let Event::Growth(event) = event {
            self.0.borrow_mut().push(event);
        }
    }
}

/// Runs host `program` with `args`; a failed command includes captured diagnostics.
fn command(program: &str, args: &[&str]) {
    let output = Command::new(program)
        .env("LC_ALL", "C")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{program}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if program == "e2fsck" {
        // e2fsck 1.47 can report incompatible lazy-inode accounting yet exit
        // successfully with -n; the diagnostic is still a format violation.
        assert!(
            !String::from_utf8_lossy(&output.stdout)
                .contains("marked uninitialized without feature set"),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

/// Opens `path` with an independent cursor and a fresh journal-enabled wrapper,
/// ensuring no cache or journal state survives an earlier simulated failure.
fn open(path: &PathBuf) -> Jbd2Dev<Image> {
    Jbd2Dev::initial_jbd2dev(
        0,
        Image(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap(),
        ),
        true,
    )
}

/// Covers 1 KiB/4 KiB blocks and both checksum/descriptor formats through the
/// same file-preservation, new-space allocation and Linux consistency contract.
#[test]
fn growth_preserves_files_and_exposes_new_groups_to_linux() {
    for (block_size, features) in [
        ("1024", "metadata_csum,64bit"),
        ("4096", "metadata_csum,64bit"),
        ("1024", "^metadata_csum,^64bit,uninit_bg"),
        ("4096", "^metadata_csum,^64bit,^uninit_bg"),
    ] {
        growth_round_trip(block_size, features);
    }
}

/// Creates the mkfs layout selected by `block_size` (bytes) and `features`, grows
/// it across a GDT boundary, then remounts and checks contents beyond the old end.
fn growth_round_trip(block_size: &str, features: &str) {
    let directory =
        std::env::temp_dir().join(format!("rsext4-native-growth-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("disk.img");
    let path_str = path.to_str().unwrap();
    let file = File::create(&path).unwrap();
    let original_bytes = 48 * 1024 * 1024 + 512 * 1024;
    file.set_len(original_bytes).unwrap();
    // Small groups force reserved-GDT consumption without a multi-gigabyte fixture.
    command(
        "mkfs.ext4",
        &[
            "-q", "-F", "-b", block_size, "-g", "1024", "-O", features, path_str,
        ],
    );
    let mut device = open(&path);
    let mut fs = Ext4FileSystem::mount(&mut device).unwrap();
    let uuid = fs.superblock.s_uuid;
    mkfile(&mut device, &mut fs, "/keep", None, None).unwrap();
    write_file(&mut device, &mut fs, "/keep", 0, b"preserve-native-growth").unwrap();
    let (keep_number, keep_inode) = rsext4::dir::get_inode_with_num(&mut fs, &mut device, "/keep")
        .unwrap()
        .unwrap();
    umount(fs, &mut device).unwrap();
    drop(device);
    file.set_len(97 * 1024 * 1024 + 512 * 1024).unwrap();
    let progress = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let image = Image(
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap(),
    );
    let services = MountServices::new(GrowthClock, (), GrowthObserver(progress.clone()));
    let mut mount = Ext4::mount_growing(image, services, MountOptions::read_write()).unwrap();
    mount.unmount().unwrap();
    drop(mount);
    let target = 97 * 1024 * 1024 + 512 * 1024;
    assert_eq!(
        progress.borrow().first(),
        Some(&GrowthEvent::Started {
            current_bytes: original_bytes,
            target_bytes: target,
        })
    );
    let mut previous = original_bytes;
    for event in &progress.borrow()[1..] {
        match *event {
            GrowthEvent::Committed { current_bytes } => {
                assert!(previous < current_bytes && current_bytes <= target);
                previous = current_bytes;
            }
            GrowthEvent::Started { .. } => panic!("growth must start exactly once"),
        }
    }
    assert_eq!(previous, target);
    assert_eq!(
        progress.borrow().last(),
        Some(&GrowthEvent::Committed {
            current_bytes: target
        })
    );
    let mut device = open(&path);
    let mut fs = Ext4FileSystem::mount_growing(&mut device).unwrap();
    assert_eq!(
        fs.superblock.blocks_count(),
        (97 * 1024 * 1024 + 512 * 1024) / block_size.parse::<u64>().unwrap()
    );
    assert_eq!(fs.superblock.s_uuid, uuid);
    let (number, inode) = rsext4::dir::get_inode_with_num(&mut fs, &mut device, "/keep")
        .unwrap()
        .unwrap();
    assert_eq!(number, keep_number);
    assert_eq!(
        (inode.i_mode, inode.i_uid, inode.i_gid, inode.i_links_count),
        (
            keep_inode.i_mode,
            keep_inode.i_uid,
            keep_inode.i_gid,
            keep_inode.i_links_count
        )
    );
    assert_eq!(
        read_file(&mut device, &mut fs, "/keep").unwrap(),
        b"preserve-native-growth"
    );
    mkfile(&mut device, &mut fs, "/new-space", None, None).unwrap();
    let number = rsext4::dir::get_inode_with_num(&mut fs, &mut device, "/new-space")
        .unwrap()
        .unwrap()
        .0;
    let mut payload = vec![0; 512 * 1024];
    let mut written = 0u64;
    let mut reached_new_space = false;
    for index in 0..100 {
        payload.fill((index % 251) as u8);
        write_file(&mut device, &mut fs, "/new-space", written, &payload).unwrap();
        written += payload.len() as u64;
        let mapping = inspect_inode_extents(
            &mut device,
            &mut fs,
            number,
            0,
            written,
            rsext4::file::FileExtentTarget::Data,
            4096,
        )
        .unwrap();
        if mapping
            .extents
            .iter()
            .any(|extent| extent.physical_start + extent.length > original_bytes)
        {
            reached_new_space = true;
            break;
        }
    }
    assert!(
        reached_new_space,
        "file allocation must reach newly published groups"
    );
    umount(fs, &mut device).unwrap();
    drop(device);
    let mut device = open(&path);
    let mut fs = Ext4FileSystem::mount_growing(&mut device).unwrap();
    assert_eq!(
        read_file(&mut device, &mut fs, "/keep").unwrap(),
        b"preserve-native-growth"
    );
    umount(fs, &mut device).unwrap();
    let mut device = open(&path);
    let mut fs = Ext4FileSystem::mount(&mut device).unwrap();
    let mut restored = vec![0; payload.len()];
    assert_eq!(
        read_inode_data_into(
            &mut device,
            &mut fs,
            number,
            written - payload.len() as u64,
            &mut restored
        )
        .unwrap(),
        payload.len()
    );
    assert_eq!(restored, payload);
    umount(fs, &mut device).unwrap();
    command("e2fsck", &["-fn", path_str]);
    std::fs::remove_dir_all(directory).unwrap();
}

/// Observed mutation boundary used to choose deterministic simulated power cuts.
#[derive(Clone, Copy, Debug)]
enum Operation {
    /// A device write, classified by its first journal header when present.
    Write {
        /// First 512-byte device sector written by this request.
        sector: u64,
        /// Number of consecutive 512-byte sectors carried by the write.
        sector_count: u32,
        /// Big-endian JBD2 record type, or None for ordinary filesystem blocks.
        journal_type: Option<u32>,
        /// Geometry carried by a primary-superblock home or journal payload.
        filesystem_blocks: Option<u64>,
    },
    /// A device flush following prior writes.
    Flush,
}

/// File device that freezes all mutations after one uncertain write/flush result.
struct FaultImage {
    /// Underlying image holding the durable snapshot used by recovery.
    inner: Image,
    /// Shared ordered mutation trace, retained by the test after device drop.
    trace: std::rc::Rc<std::cell::RefCell<Vec<Operation>>>,
    /// One-based trace position at which completion becomes an injected error.
    fail_at: Option<usize>,
    /// Latched loss of write access; cleanup writes must not alter the snapshot.
    dead: bool,
}

impl FaultImage {
    /// Appends `operation` while alive and reports whether it is the configured
    /// failure boundary; an already dead device always requests failure.
    fn record(&mut self, operation: Operation) -> bool {
        if self.dead {
            return true;
        }
        self.trace.borrow_mut().push(operation);
        self.fail_at == Some(self.trace.borrow().len())
    }
}

impl BlockIo for FaultImage {
    /// Preserves the wrapped image's sector size and capacity.
    fn geometry(&self) -> DeviceGeometry {
        self.inner.geometry()
    }
    /// Preserves the wrapped image's writable/flush capability advertisement.
    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.capabilities()
    }
    /// Reads `count` sectors at `sector` into `bytes`, including after the cut
    /// so only mutation is frozen and the snapshot remains inspectable.
    fn read(&mut self, bytes: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        self.inner.read(bytes, sector, count)
    }
    /// Records and writes `count` sectors at `sector`. At the selected cut the
    /// complete write is flushed before returning an uncertain-completion error;
    /// all later mutation attempts fail without changing the image.
    fn write(&mut self, bytes: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        if self.dead {
            return Err(Ext4Error::io());
        }
        let journal_type = (bytes.get(..4) == Some(&0xc03b_3998u32.to_be_bytes()))
            .then(|| u32::from_be_bytes(bytes[4..8].try_into().unwrap()));
        let fail = self.record(Operation::Write {
            sector: sector.raw(),
            sector_count: count,
            journal_type,
            filesystem_blocks: (bytes.get(56..58) == Some(&[0x53, 0xef]))
                .then(|| u64::from(u32::from_le_bytes(bytes[4..8].try_into().unwrap()))),
        });
        // An uncertain completion may already have reached the medium. Once
        // power is lost, cleanup and abort writes cannot change the snapshot.
        self.inner.write(bytes, sector, count)?;
        if fail {
            self.inner.flush()?;
            self.dead = true;
            return Err(Ext4Error::io());
        }
        Ok(())
    }
    /// Records and executes a host flush, then freezes mutation if this barrier
    /// is the selected cut. Already frozen devices return an I/O error.
    fn flush(&mut self) -> Ext4Result<()> {
        if self.dead {
            return Err(Ext4Error::io());
        }
        let fail = self.record(Operation::Flush);
        self.inner.flush()?;
        if fail {
            self.dead = true;
            return Err(Ext4Error::io());
        }
        Ok(())
    }
}

impl Clock for FaultImage {
    /// Uses the inner image's deterministic clock without adding a trace entry.
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        self.inner.now()
    }
}

/// Opens `path` with optional one-based mutation cut `fail_at`, returning both a
/// fresh journal wrapper and the shared trace used to identify commit boundaries.
fn fault_device(
    path: &PathBuf,
    fail_at: Option<usize>,
) -> (
    Jbd2Dev<FaultImage>,
    std::rc::Rc<std::cell::RefCell<Vec<Operation>>>,
) {
    let trace = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let inner = Image(
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap(),
    );
    (
        Jbd2Dev::initial_jbd2dev(
            0,
            FaultImage {
                inner,
                trace: trace.clone(),
                fail_at,
                dead: false,
            },
            true,
        ),
        trace,
    )
}

/// Selects cuts around journal descriptors, commits, home superblock/GDT writes
/// and flushes, then proves a fresh growing mount preserves files and passes
/// e2fsck. Also compares already-grown I/O with an ordinary mount.
#[test]
fn growth_recovers_commit_checkpoint_and_flush_interruptions() {
    for features in ["metadata_csum,64bit", "^metadata_csum,^64bit,^uninit_bg"] {
        interrupted_growth_round_trip(features, false);
    }
    interrupted_growth_round_trip("metadata_csum,64bit", true);
}

/// Exercises the same durable growth/recovery contract with `features` selecting
/// checksummed descriptors or legacy reserved-GDT pointer records. Every cut
/// reopens the image and must preserve files, capacity and Linux consistency.
fn interrupted_growth_round_trip(features: &str, journal_pressure: bool) {
    let directory =
        std::env::temp_dir().join(format!("rsext4-growth-fault-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let original = directory.join("original.img");
    let baseline = directory.join("baseline.img");
    let attempt = directory.join("attempt.img");
    File::create(&original)
        .unwrap()
        .set_len(15 * 1024 * 1024)
        .unwrap();
    command(
        "mkfs.ext4",
        &[
            "-q",
            "-F",
            "-b",
            "1024",
            "-g",
            "1024",
            "-O",
            features,
            original.to_str().unwrap(),
        ],
    );
    let mut device = open(&original);
    let mut filesystem = Ext4FileSystem::mount(&mut device).unwrap();
    mkfile(&mut device, &mut filesystem, "/keep", None, None).unwrap();
    write_file(
        &mut device,
        &mut filesystem,
        "/keep",
        0,
        b"stable-before-growth",
    )
    .unwrap();
    let original_groups = filesystem.superblock.block_groups_count();
    let descriptor_size = filesystem.superblock.get_desc_size();
    umount(filesystem, &mut device).unwrap();
    drop(device);
    let mut expanded = OpenOptions::new().write(true).open(&original).unwrap();
    let old_bytes = expanded.metadata().unwrap().len();
    // A larger sparse target exhausts this mkfs image's 1 MiB journal and
    // forces both credit-limited batches and intermediate log reclamation.
    let target_bytes = if journal_pressure { 600 } else { 34 } * 1024 * 1024 + 512 * 1024;
    expanded.set_len(target_bytes).unwrap();
    expanded.seek(SeekFrom::Start(old_bytes)).unwrap();
    // New inode tables must actually be cleared, even on reused nonzero media.
    let poison = vec![0xa5; 1024 * 1024];
    let mut remaining = if journal_pressure {
        0
    } else {
        target_bytes - old_bytes
    };
    while remaining != 0 {
        let count = remaining.min(poison.len() as u64) as usize;
        expanded.write_all(&poison[..count]).unwrap();
        remaining -= count as u64;
    }
    drop(expanded);
    std::fs::copy(&original, &baseline).unwrap();
    let (mut device, trace) = fault_device(&baseline, None);
    let filesystem = Ext4FileSystem::mount_growing(&mut device).unwrap();
    let expected_blocks = filesystem.superblock.blocks_count();
    let block_size = filesystem.superblock.block_size();
    let table_blocks = (u64::from(filesystem.superblock.s_inodes_per_group)
        * u64::from(filesystem.superblock.s_inode_size))
    .div_ceil(block_size);
    let new_inode_tables: Vec<_> = filesystem.group_descs[original_groups as usize..]
        .iter()
        .map(|descriptor| {
            let start = descriptor.inode_table() * block_size / 512;
            start..start + table_blocks * block_size / 512
        })
        .collect();
    let mut raw = File::open(&baseline).unwrap();
    for table in &new_inode_tables {
        raw.seek(SeekFrom::Start(table.start * 512)).unwrap();
        let mut initialized = vec![0xff; (table.end - table.start) as usize * 512];
        raw.read_exact(&mut initialized).unwrap();
        assert!(
            initialized.iter().all(|byte| *byte == 0),
            "new inode tables must be fully zeroed before publishing geometry"
        );
    }
    umount(filesystem, &mut device).unwrap();
    drop(device);
    let operations = trace.borrow();
    // Durable geometry must advance by multiple groups per transaction. The
    // same fixture crosses two GDT boundaries and adds sparse backups 25/27,
    // so batching must preserve their resize-inode references as well.
    let mut published_blocks: Vec<_> = operations
        .iter()
        .filter_map(|operation| match operation {
            Operation::Write {
                filesystem_blocks, ..
            } => *filesystem_blocks,
            Operation::Flush => None,
        })
        .collect();
    published_blocks.dedup();
    assert!(
        published_blocks
            .windows(2)
            .any(|pair| pair[1] > pair[0] + 1024),
        "growth must publish multiple block groups in one durable transaction"
    );
    // Coalesce repeated changes while their transactions remain journal-owned.
    // This early GDT block stops changing before pressure reclaims its older
    // log prefix, so only its final image needs to reach the home location.
    let commits = operations
        .iter()
        .filter(|operation| {
            matches!(
                operation,
                Operation::Write {
                    journal_type: Some(2),
                    ..
                }
            )
        })
        .count();
    let flushes = operations
        .iter()
        .filter(|operation| matches!(operation, Operation::Flush))
        .count();
    let gdt_writes = operations
        .iter()
        .filter(|operation| matches!(operation, Operation::Write { sector: 4, .. }))
        .count();
    println!(
        "growth I/O {features} pressure={journal_pressure}: commits={commits}, flushes={flushes}, \
         first_gdt_home_writes={gdt_writes}, descriptor_size={descriptor_size}"
    );
    assert_eq!(
        gdt_writes, 1,
        "growth must coalesce home images across committed batches"
    );
    if journal_pressure {
        let last_initialization = operations.iter().rposition(|operation| matches!(operation,
            Operation::Write { sector, .. } if new_inode_tables.iter().any(|table| table.contains(sector)))).unwrap();
        // Reclamation can skip an old superblock image while a later commit
        // still owns its successor. This first GDT block stops changing early
        // and is therefore observable when that older log prefix is reclaimed.
        let first_checkpoint = operations
            .iter()
            .position(|operation| matches!(operation, Operation::Write { sector: 4, .. }))
            .unwrap();
        assert!(
            first_checkpoint < last_initialization,
            "journal pressure must reclaim committed log space before growth ends"
        );
        let committed_before_checkpoint = operations[..first_checkpoint]
            .iter()
            .filter(|operation| {
                matches!(
                    operation,
                    Operation::Write {
                        journal_type: Some(2),
                        ..
                    }
                )
            })
            .count();
        assert!(
            committed_before_checkpoint >= 2,
            "checkpoint must retain multiple durable growth transactions"
        );
        assert!(
            published_blocks.windows(2).any(|pair| {
                pair[1] < expected_blocks
                    && pair[1]
                        .checked_sub(pair[0])
                        .is_some_and(|span| span > 16 * 1024 && span < 256 * 1024)
            }),
            "a large candidate must shrink to a useful credit-limited batch"
        );
    }
    // Small consecutive tables are submitted whole, and several tables share
    // the initialization barrier before their batch is journalled.
    assert!(new_inode_tables.len() > 1, "must initialize several tables");
    let mut previous_table = None;
    let mut shared_barrier = false;
    for table in new_inode_tables {
        let writes: Vec<_> = operations
            .iter()
            .enumerate()
            .filter_map(|(index, operation)| match operation {
                Operation::Write { sector, .. } if table.contains(sector) => Some(index),
                _ => None,
            })
            .collect();
        assert_eq!(
            writes.len(),
            1,
            "a small consecutive inode table must use one aggregated write"
        );
        if let Operation::Write {
            sector,
            sector_count,
            ..
        } = operations[writes[0]]
        {
            assert_eq!(sector, table.start);
            assert_eq!(sector + u64::from(sector_count), table.end);
        }
        if let Some(previous) = previous_table {
            shared_barrier |= !operations[previous + 1..writes[0]]
                .iter()
                .any(|operation| matches!(operation, Operation::Flush));
        }
        let last = *writes.last().expect("new inode table must be initialized");
        previous_table = Some(last);
        let next_journal = operations[last + 1..]
            .iter()
            .position(|operation| {
                matches!(
                    operation,
                    Operation::Write {
                        journal_type: Some(2),
                        ..
                    }
                )
            })
            .expect("new inode table must be followed by its journal transaction")
            + last
            + 1;
        assert!(
            operations[last + 1..next_journal]
                .iter()
                .any(|operation| matches!(operation, Operation::Flush)),
            "new inode table must be durable before its journal commit"
        );
    }
    assert!(
        shared_barrier,
        "new inode tables must share a durability barrier"
    );
    let mut cuts = std::collections::BTreeSet::new();
    for (index, operation) in operations.iter().enumerate() {
        if matches!(
            operation,
            Operation::Write {
                journal_type: Some(1 | 2),
                ..
            } | Operation::Write { sector: 2 | 4, .. }
        ) {
            cuts.insert(index + 1);
            if index > 0 {
                cuts.insert(index);
            }
            if index + 1 < operations.len() && matches!(operations[index + 1], Operation::Flush) {
                cuts.insert(index + 2);
            }
        }
    }
    cuts.insert(operations.len());
    assert!(
        cuts.len() >= 12,
        "must exercise partial-group, new-group and GDT-boundary transactions"
    );
    drop(operations);
    for cut in cuts {
        std::fs::copy(&original, &attempt).unwrap();
        let (mut device, _) = fault_device(&attempt, Some(cut));
        if let Ok(filesystem) = Ext4FileSystem::mount_growing(&mut device) {
            assert!(
                umount(filesystem, &mut device).is_err(),
                "cut {cut} must interrupt growth or unmount"
            );
        }
        drop(device);
        let mut device = open(&attempt);
        let mut filesystem = Ext4FileSystem::mount_growing(&mut device)
            .unwrap_or_else(|error| panic!("recover cut {cut}: {error:?}"));
        assert_eq!(
            filesystem.superblock.blocks_count(),
            expected_blocks,
            "cut {cut}"
        );
        assert_eq!(
            read_file(&mut device, &mut filesystem, "/keep").unwrap(),
            b"stable-before-growth",
            "cut {cut}"
        );
        umount(filesystem, &mut device).unwrap();
        command("e2fsck", &["-fn", attempt.to_str().unwrap()]);
    }
    // Normal mount maintenance is allowed; an already grown constructor must
    // add no writes compared with the same plain mount on an identical image.
    std::fs::copy(&baseline, &attempt).unwrap();
    let (mut device, normal) = fault_device(&attempt, None);
    let filesystem = Ext4FileSystem::mount(&mut device).unwrap();
    umount(filesystem, &mut device).unwrap();
    drop(device);
    std::fs::copy(&baseline, &attempt).unwrap();
    let (mut device, grown) = fault_device(&attempt, None);
    let filesystem = Ext4FileSystem::mount_growing(&mut device).unwrap();
    umount(filesystem, &mut device).unwrap();
    drop(device);
    assert_eq!(
        normal.borrow().len(),
        grown.borrow().len(),
        "idempotent growth adds no I/O"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

/// Checks capacity rounding and the on-disk GDT reserve boundary through the
/// public preflight API. Accepted and rejected targets must leave the mounted
/// geometry unchanged and perform no device writes or flushes.
#[test]
fn growth_preflight_checks_capacity_boundaries_without_mutation() {
    let directory =
        std::env::temp_dir().join(format!("rsext4-large-target-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("sparse.img");
    File::create(&path)
        .unwrap()
        .set_len(4 * 1024 * 1024 * 1024)
        .unwrap();
    // Bound the descriptor reserve explicitly so it, rather than journal
    // credits, limits growth. Lazy initialization keeps the fixture sparse.
    let resize_blocks = 64 * std::fs::metadata(&path).unwrap().len() / 4096;
    command(
        "mkfs.ext4",
        &[
            "-q",
            "-F",
            "-b",
            "4096",
            "-O",
            "^orphan_file,^metadata_csum_seed",
            "-E",
            &format!("lazy_itable_init=1,lazy_journal_init=1,resize={resize_blocks}"),
            path.to_str().unwrap(),
        ],
    );
    let (mut device, trace) = fault_device(&path, None);
    let mut filesystem = Ext4FileSystem::mount(&mut device).unwrap();
    let superblock = filesystem.superblock;
    let block_size = superblock.block_size();
    let current_bytes = superblock.blocks_count() * block_size;
    let descriptors_per_block = u64::from(superblock.descs_per_block());
    let current_gdt_blocks =
        u64::from(superblock.block_groups_count()).div_ceil(descriptors_per_block);
    let available_groups =
        (current_gdt_blocks + u64::from(superblock.s_reserved_gdt_blocks)) * descriptors_per_block;
    let capacity = (u64::from(superblock.s_first_data_block)
        + available_groups * u64::from(superblock.blocks_per_group()))
        * block_size;
    assert!(capacity > current_bytes, "fixture must permit growth");
    let before = trace.borrow().len();
    for (target, expected) in [
        (current_bytes - block_size, Ok(current_bytes)),
        (capacity - 1, Ok(capacity - block_size)),
        (capacity + block_size, Ok(capacity)),
        (
            capacity + u64::from(superblock.blocks_per_group()) * block_size,
            Err(Ext4ErrorKind::Unsupported),
        ),
        (capacity, Ok(capacity)),
    ] {
        assert_eq!(
            filesystem
                .growth_capacity(&mut device, target)
                .map_err(Ext4Error::kind),
            expected,
            "preflight target {target} bytes"
        );
        assert_eq!(
            trace.borrow().len(),
            before,
            "preflight target {target} must not write or flush"
        );
        assert_eq!(
            filesystem.superblock.blocks_count(),
            superblock.blocks_count(),
            "preflight must not publish a new capacity"
        );
        assert_eq!(
            filesystem.superblock.block_groups_count(),
            superblock.block_groups_count(),
            "preflight must not publish new block groups"
        );
    }
    umount(filesystem, &mut device).unwrap();
    // A large existing GDT does not make a tail-only update rewrite it all.
    let tail = directory.join("many-groups.img");
    let blocks = 2048u64 * 256;
    File::create(&tail)
        .unwrap()
        .set_len((blocks - 16) * 4096)
        .unwrap();
    command(
        "mkfs.ext4",
        &[
            "-q",
            "-F",
            "-b",
            "4096",
            "-g",
            "256",
            "-i",
            "65536",
            "-O",
            "^resize_inode,^orphan_file,^metadata_csum_seed",
            "-J",
            "size=4",
            "-E",
            "lazy_itable_init=1,lazy_journal_init=1",
            tail.to_str().unwrap(),
        ],
    );
    let (mut device, trace) = fault_device(&tail, None);
    let mut filesystem = Ext4FileSystem::mount(&mut device).unwrap();
    assert_eq!(filesystem.superblock.block_groups_count(), 2048);
    assert_eq!(filesystem.superblock.get_desc_size(), 64);
    assert_eq!(filesystem.superblock.s_reserved_gdt_blocks, 0);
    let before = trace.borrow().len();
    for target in [blocks * 4096, (blocks + 1) * 4096] {
        assert_eq!(
            filesystem.growth_capacity(&mut device, target).unwrap(),
            blocks * 4096
        );
    }
    assert_eq!(
        trace.borrow().len(),
        before,
        "tail preflight must not write"
    );
    umount(filesystem, &mut device).unwrap();
    drop(device);
    OpenOptions::new()
        .write(true)
        .open(&tail)
        .unwrap()
        .set_len(blocks * 4096)
        .unwrap();
    let mut mounted = Ext4::mount_growing(
        Image(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&tail)
                .unwrap(),
        ),
        MountServices::new(GrowthClock, (), NoopObserver),
        MountOptions::read_write(),
    )
    .unwrap();
    assert_eq!(mounted.statfs().total_blocks, blocks);
    mounted.unmount().unwrap();
    command("e2fsck", &["-fn", tail.to_str().unwrap()]);
    std::fs::remove_dir_all(directory).unwrap();
}
