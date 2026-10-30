//! Root growth policy, applied before filesystem and guest publication.
//!
//! Axvisor and StarryOS reach this shared path after root selection. Probe the
//! filesystem through the old region, commit GPT, then reopen the new region
//! for ext4 growth. No filesystem owner may survive across those region views.

use super::*;

#[cfg(feature = "ext4")]
impl<T: FsBlockDevice + ?Sized> crate::volume::BlockWriter for VolumeReader<'_, T> {
    /// Writes physical LBA `block` from `buffer`, preserving the device block size;
    /// translates backend failures to the volume mutation error boundary.
    fn write_block(&mut self, block: u64, buffer: &[u8]) -> crate::volume::Result<()> {
        self.inner
            .write_block(block, buffer)
            .map_err(|_| VolumeError::Writer)
    }
    /// Requests durable completion of preceding writes on the wrapped device.
    fn flush(&mut self) -> crate::volume::Result<()> {
        self.inner.flush().map_err(|_| VolumeError::Writer)
    }
}

/// Applies automatic growth only to an explicitly selected writable GPT/ext4 root.
/// `selected` is refreshed after successful growth; `partition_index` uses the
/// discovery layer's zero-based entry index. `specification` authorizes the root
/// and `bootargs` supplies ro/rw policy. Ineligible roots return success unchanged;
/// metadata or I/O failures propagate before the root can be published.
pub(super) fn grow_selected_root(
    selected: &mut DiscoveredDisk,
    partition_index: Option<usize>,
    specification: &RootSpec,
    bootargs: Option<&str>,
) -> axfs_ng_vfs::VfsResult<()> {
    // An implicit fallback root must never authorize writes.
    if !specification.has_explicit_selector() || root_is_readonly(bootargs) {
        info!("root growth skipped: explicit writable root selection required");
        return Ok(());
    }
    let Some(partition) = partition_index.and_then(|index| {
        selected
            .partitions
            .iter()
            .find(|partition| partition.info.index == index)
    }) else {
        info!("root growth skipped: a GPT ext4 partition is required");
        return Ok(());
    };
    if partition.info.table_kind != PartitionTableKind::Gpt
        || partition.filesystem != Some(FilesystemKind::Ext4)
    {
        info!("root growth skipped: selected root is not GPT/ext4");
        return Ok(());
    }
    #[cfg(feature = "ext4")]
    return grow_ext4_root(selected, partition.info.index);
    #[cfg(not(feature = "ext4"))]
    {
        info!("root growth skipped: ext4 support is unavailable");
        Ok(())
    }
}

/// Grows the zero-based GPT entry `index` on `selected` under mount exclusion,
/// then replaces its discovered filesystem/partition views after both commits.
#[cfg(feature = "ext4")]
fn grow_ext4_root(selected: &mut DiscoveredDisk, index: usize) -> axfs_ng_vfs::VfsResult<()> {
    let handle = selected.handle.clone();
    // Use the mount registry's exclusion, rather than a second growth lock:
    // ordinary mount construction must not race the geometry change.
    fs::with_unmounted_device(&handle, || {
        let mut disk = boxed_native_handle_block_device(handle.clone())
            .map_err(crate::block_error_to_vfs_error)?;
        if let Some(scan) = grow_root_device(&mut *disk, selected.disk_index, index)? {
            let (raw, _, _, partitions) = collect_partitions(&mut *disk, scan.volumes);
            selected.raw_filesystem = raw;
            selected.partitions = partitions;
        }
        Ok(())
    })
}

/// Runs the two commits without publishing a mount. Unsupported layouts leave
/// GPT untouched; validation or I/O failures abort root preparation.
/// `disk` is an exclusive whole-device view, `disk_index` labels diagnostics and
/// discovered volumes, and `index` is a zero-based GPT entry index. Returns None
/// for an ineligible device/layout or a fresh scan after completing both stages.
#[cfg(feature = "ext4")]
fn grow_root_device(
    disk: &mut dyn FsBlockDevice,
    disk_index: usize,
    index: usize,
) -> axfs_ng_vfs::VfsResult<Option<crate::volume::VolumeScan>> {
    use crate::volume::{PartitionId, grow_partition, partition_growth};
    if disk.is_read_only() || !disk.supports_flush() {
        debug!("root growth skipped: device is read-only or cannot flush");
        return Ok(None);
    }
    let started = crate::os::monotonic_time();
    let partition = PartitionId(u32::try_from(index + 1).map_err(|_| VfsError::InvalidInput)?);
    let plan = match partition_growth(&mut VolumeReader::new(disk), partition) {
        Ok(plan) => plan,
        Err(VolumeError::Unsupported) => {
            debug!("root growth skipped: unsupported GPT layout or non-tail root");
            return Ok(None);
        }
        Err(error) => {
            warn!(
                "root growth GPT preflight failed on disk {disk_index} partition {}: {error}",
                index + 1
            );
            return Err(VfsError::InvalidData);
        }
    };
    let target_bytes = plan
        .target
        .num_blocks
        .checked_mul(disk.block_size() as u64)
        .ok_or(VfsError::InvalidInput)?;
    let current_bytes = plan
        .current
        .num_blocks
        .checked_mul(disk.block_size() as u64)
        .ok_or(VfsError::InvalidInput)?;
    let current_region = BlockRegion::new(plan.current.start_block, plan.current.num_blocks);
    // A clean, already full filesystem needs no temporary writable mounts.
    // GPT still goes through its idempotent repair path: equal partition bounds
    // alone do not prove that both table copies are current and valid.
    let complete = plan.current == plan.target
        && root_filesystem_complete(disk, current_region, target_bytes)?;
    // This probe may recover an existing journal; failures cannot universally
    // become a read-only skip merely because no growth has started yet.
    if !complete {
        let probe = disk.fork_io().map_err(crate::block_error_to_vfs_error)?;
        match fs::check_root_growth(probe, current_region, target_bytes) {
            Ok(capacity) => info!(
                "root growth disk {disk_index} partition {}: current partition {current_bytes} \
                 bytes, target partition {target_bytes} bytes, ext4 {capacity} bytes",
                index + 1
            ),
            Err(error) if matches!(error.kind(), rsext4::Ext4ErrorKind::Unsupported) => {
                debug!("root growth skipped: ext4 preflight rejected target: {error}");
                return Ok(None);
            }
            Err(error) => {
                warn!(
                    "root growth ext4 preflight failed on disk {disk_index} partition {}: {error}",
                    index + 1
                );
                return Err(VfsError::InvalidData);
            }
        }
    }
    let expanded = grow_partition(&mut VolumeReader::new(disk), partition).map_err(|error| {
        warn!(
            "root growth GPT commit failed on disk {disk_index} partition {}: {error}",
            index + 1
        );
        VfsError::Io
    })?;
    // The preflight region, inode and journal owners have been destroyed.
    // Physical cache addresses remain coherent across the new region view.
    let scan = scan_volumes(&mut VolumeReader::new(disk), DiskId(disk_index as u64))
        .map_err(|_| VfsError::InvalidData)?;
    let volume = scan
        .volumes
        .iter()
        .find(|volume| volume.partition_id == partition)
        .ok_or(VfsError::InvalidData)?;
    if volume.region != expanded {
        return Err(VfsError::InvalidData);
    }
    let region = region_from_volume(volume);
    if !complete {
        let filesystem_device = disk.fork_io().map_err(crate::block_error_to_vfs_error)?;
        fs::grow_root_filesystem(filesystem_device, region).map_err(|error| {
            warn!(
                "root growth ext4 commit failed on disk {disk_index} partition {}: {error}",
                index + 1
            );
            VfsError::Io
        })?;
    }
    if complete {
        debug!(
            "root growth already complete on disk {disk_index} partition {}",
            index + 1
        );
        return Ok(Some(scan));
    }
    info!(
        "root growth complete on disk {disk_index} partition {} in {} ms",
        index + 1,
        crate::os::monotonic_time()
            .saturating_sub(started)
            .as_millis()
    );
    Ok(Some(scan))
}

/// Checks only the clean on-disk capacity for the completed-root fast path.
/// A pending journal, error state or remaining space keeps the full growth
/// path, so partially published geometry cannot bypass recovery or preflight.
/// Reads within the validated current partition and never changes metadata.
#[cfg(feature = "ext4")]
fn root_filesystem_complete(
    disk: &mut dyn FsBlockDevice,
    region: BlockRegion,
    target_bytes: u64,
) -> axfs_ng_vfs::VfsResult<bool> {
    let raw = read_filesystem_bytes(disk, region, 1024, rsext4::SUPERBLOCK_SIZE)?;
    let sb = Ext4Superblock::from_disk_bytes(&raw);
    sb.verify_superblock().map_err(|_| VfsError::InvalidData)?;
    if sb.blocks_count()
        > target_bytes / u64::from(sb.checked_block_size().map_err(|_| VfsError::InvalidData)?)
    {
        return Ok(false);
    }
    // Use the same exact tail truncation as growth, rather than accepting an
    // arbitrary shortfall that could hide a filesystem still needing growth.
    let target_blocks = sb
        .growth_target_blocks(target_bytes)
        .map_err(|_| VfsError::InvalidData)?;
    Ok(sb.s_state & Ext4Superblock::EXT4_VALID_FS != 0
        && sb.s_state & Ext4Superblock::EXT4_ERROR_FS == 0
        && !sb.has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER)
        && sb.blocks_count() == target_blocks)
}

#[cfg(all(test, feature = "ext4"))]
mod tests {
    //! Host-image coverage of the GPT-to-ext4 growth sequence and device preflight.
    //! These tests exercise the shared device helper, not boot-time root selection
    //! or the mount registry. Linux e2fsck independently checks the resulting ext4.

    use alloc::boxed::Box;
    use std::{
        fs::{self, File, OpenOptions},
        io::{Read, Seek, SeekFrom, Write},
        path::PathBuf,
        process::{Command, Stdio},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::*;
    use crate::{BlockError, BlockResult};

    /// Counts actual device operations across independent views of one fixture.
    #[derive(Default)]
    struct IoTrace {
        /// Number of sector writes requested by the growth path.
        writes: AtomicUsize,
        /// Number of durability barriers requested by the growth path.
        flushes: AtomicUsize,
    }

    impl IoTrace {
        /// Samples operations after the synchronous growth helper has returned.
        fn snapshot(&self) -> (usize, usize) {
            (
                self.writes.load(Ordering::Relaxed),
                self.flushes.load(Ordering::Relaxed),
            )
        }
    }

    /// File-backed whole disk whose advertised capabilities exercise growth skips.
    struct Image {
        /// Open file used by this device view for positional block I/O.
        file: File,
        /// Source image path used to create a fresh independent I/O view.
        path: PathBuf,
        /// Advertised read-only capability checked before any growth operation.
        readonly: bool,
        /// Advertised flush capability; false makes automatic growth ineligible.
        flush: bool,
        /// Shared counters include writes and barriers from forked mount views.
        trace: Arc<IoTrace>,
    }

    impl Image {
        /// Opens `path` as a writable image advertising durable flush support.
        fn open(path: PathBuf) -> Self {
            Self {
                file: OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .unwrap(),
                path,
                readonly: false,
                flush: true,
                trace: Arc::default(),
            }
        }
    }

    impl FsBlockDevice for Image {
        /// Returns a stable diagnostic name for the fixture device.
        fn name(&self) -> &str {
            "root-grow-image"
        }
        /// Returns current file length in 512-byte sectors.
        fn num_blocks(&self) -> u64 {
            self.file.metadata().unwrap().len() / 512
        }
        /// Returns the fixed 512-byte logical sector size.
        fn block_size(&self) -> usize {
            512
        }
        /// Exposes the injected read-only flag to the production preflight.
        fn is_read_only(&self) -> bool {
            self.readonly
        }
        /// Exposes the injected durability capability to the production preflight.
        fn supports_flush(&self) -> bool {
            self.flush
        }
        /// Reads one sector at LBA `block` into `output`, forwarding file errors.
        fn read_block(&mut self, block: u64, output: &mut [u8]) -> BlockResult {
            self.file
                .seek(SeekFrom::Start(block * 512))
                .and_then(|_| self.file.read_exact(output))
                .map_err(|_| BlockError::InvalidState)
        }
        /// Writes one-sector `input` at LBA `block`; eligibility is checked by the
        /// growth helper before this fixture's writable file can be used.
        fn write_block(&mut self, block: u64, input: &[u8]) -> BlockResult {
            self.trace.writes.fetch_add(1, Ordering::Relaxed);
            self.file
                .seek(SeekFrom::Start(block * 512))
                .and_then(|_| self.file.write_all(input))
                .map_err(|_| BlockError::InvalidState)
        }
        /// Synchronizes file contents and propagates host durability failures.
        fn flush(&mut self) -> BlockResult {
            self.trace.flushes.fetch_add(1, Ordering::Relaxed);
            self.file.sync_all().map_err(|_| BlockError::InvalidState)
        }
        /// Reopens the same image with default capabilities and an independent
        /// cursor; skip tests must return before attempting to fork this view.
        fn fork_io(&self) -> BlockResult<Box<dyn FsBlockDevice>> {
            let mut view = Self::open(self.path.clone());
            view.trace = self.trace.clone();
            Ok(Box::new(view))
        }
    }

    /// Runs host `program` with `args` and fails with captured output on nonzero exit.
    fn command(program: &str, args: &[&str]) {
        let output = Command::new(program).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{program}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Grows only the root tail, checks read-only/no-flush skips, and verifies other
    /// partition identity, ext4 contents and a repeated run using Linux host tools.
    #[test]
    fn root_growth_preserves_other_partitions_and_ext4_contents() {
        // Cover both a usable partial group and a tail too small for metadata.
        for disk_bytes in [128 * 1024 * 1024, 130 * 1024 * 1024 + 4096 + 33 * 512] {
            let directory =
                std::env::temp_dir().join(format!("axfs-root-growth-{}", std::process::id()));
            fs::create_dir_all(&directory).unwrap();
            let disk_path = directory.join("disk.img");
            let root_path = directory.join("root.img");
            File::create(&root_path)
                .unwrap()
                .set_len(48 * 1024 * 1024)
                .unwrap();
            command(
                "mkfs.ext4",
                &[
                    "-q",
                    "-F",
                    "-b",
                    "4096",
                    "-g",
                    "1024",
                    root_path.to_str().unwrap(),
                ],
            );
            let payload = directory.join("keep");
            fs::write(&payload, b"preserved-root-payload").unwrap();
            command(
                "debugfs",
                &[
                    "-w",
                    "-R",
                    &format!("write {} /keep", payload.display()),
                    root_path.to_str().unwrap(),
                ],
            );
            File::create(&disk_path)
                .unwrap()
                .set_len(64 * 1024 * 1024)
                .unwrap();
            let mut sfdisk = Command::new("sfdisk")
                .arg(&disk_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            sfdisk.stdin.take().unwrap().write_all(b"label: gpt\nstart=2048,size=2048,type=linux,name=boot\nstart=4096,size=98304,type=linux,name=rootfs\n").unwrap();
            let output = sfdisk.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let mut disk = Image::open(disk_path.clone());
            disk.file.seek(SeekFrom::Start(2048 * 512)).unwrap();
            disk.file.write_all(&[0x5a; 512]).unwrap();
            disk.file.seek(SeekFrom::Start(4096 * 512)).unwrap();
            std::io::copy(&mut File::open(&root_path).unwrap(), &mut disk.file).unwrap();
            disk.file.set_len(disk_bytes).unwrap();
            let original = scan_volumes(&mut VolumeReader::new(&mut disk), DiskId(3)).unwrap();
            let output = Command::new("blkid")
                .args(["-p", "-s", "UUID", "-o", "value"])
                .arg(&root_path)
                .output()
                .unwrap();
            assert!(output.status.success());
            let expected_uuid = String::from_utf8(output.stdout).unwrap().trim().to_string();
            let region = original.volumes[1].region;
            assert_eq!(
                read_filesystem_uuid(
                    &mut disk,
                    BlockRegion::new(region.start_block, region.num_blocks),
                    Some(FilesystemKind::Ext4),
                )
                .unwrap()
                .as_deref(),
                Some(expected_uuid.as_str())
            );
            // Device prerequisites skip before touching either GPT or filesystem metadata.
            let before = fs::read(&disk_path).unwrap();
            disk.readonly = true;
            assert!(grow_root_device(&mut disk, 3, 1).unwrap().is_none());
            disk.readonly = false;
            disk.flush = false;
            assert!(grow_root_device(&mut disk, 3, 1).unwrap().is_none());
            assert_eq!(fs::read(&disk_path).unwrap(), before);
            disk.flush = true;
            let scan = grow_root_device(&mut disk, 3, 1)
                .unwrap()
                .expect("complete GPT and ext4 growth");
            assert_eq!(scan.volumes[0], original.volumes[0]);
            assert_eq!(scan.volumes[1].partuuid, original.volumes[1].partuuid);
            assert!(scan.volumes[1].region.num_blocks > original.volumes[1].region.num_blocks);
            command("sfdisk", &["--verify", disk_path.to_str().unwrap()]);
            disk.file.seek(SeekFrom::Start(2048 * 512)).unwrap();
            let mut marker = [0; 512];
            disk.file.read_exact(&mut marker).unwrap();
            assert_eq!(marker, [0x5a; 512]);
            let region = scan.volumes[1].region;
            // The second device ends one filesystem block into a new group, too
            // small for its metadata. Growth must keep the complete-group prefix.
            if disk_bytes != 128 * 1024 * 1024 {
                let raw = read_filesystem_bytes(
                    &mut disk,
                    BlockRegion::new(region.start_block, region.num_blocks),
                    1024,
                    rsext4::SUPERBLOCK_SIZE,
                )
                .unwrap();
                let sb = Ext4Superblock::from_disk_bytes(&raw);
                assert_eq!(sb.blocks_count(), 32 * 1024);
                assert_eq!(region.num_blocks * 512 / 4096, sb.blocks_count() + 1);
            }
            // Recreate the interruption between GPT publication and ext4 growth:
            // equal GPT bounds must not treat the old filesystem as complete.
            disk.file
                .seek(SeekFrom::Start(region.start_block * 512))
                .unwrap();
            std::io::copy(&mut File::open(&root_path).unwrap(), &mut disk.file).unwrap();
            assert!(grow_root_device(&mut disk, 3, 1).unwrap().is_some());
            assert_eq!(
                read_filesystem_uuid(
                    &mut disk,
                    BlockRegion::new(region.start_block, region.num_blocks),
                    Some(FilesystemKind::Ext4),
                )
                .unwrap()
                .as_deref(),
                Some(expected_uuid.as_str())
            );
            disk.file
                .seek(SeekFrom::Start(region.start_block * 512))
                .unwrap();
            let mut extracted = File::create(&root_path).unwrap();
            std::io::copy(
                &mut (&mut disk.file).take(region.num_blocks * 512),
                &mut extracted,
            )
            .unwrap();
            command("e2fsck", &["-fn", root_path.to_str().unwrap()]);
            let output = Command::new("debugfs")
                .args(["-R", "cat /keep"])
                .arg(&root_path)
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"preserved-root-payload");
            // A completed root must not repeat temporary mount maintenance.
            let completed = fs::read(&disk_path).unwrap();
            let operations = disk.trace.snapshot();
            assert!(grow_root_device(&mut disk, 3, 1).unwrap().is_some());
            assert_eq!(
                disk.trace.snapshot(),
                operations,
                "completed growth must add no writes or flushes"
            );
            assert!(
                fs::read(&disk_path).unwrap() == completed,
                "completed growth must add no mount writes"
            );
            // A complete filesystem must not prevent repair of an invalid GPT copy.
            disk.file.seek(SeekFrom::Start(512 + 16)).unwrap();
            let mut checksum = [0];
            disk.file.read_exact(&mut checksum).unwrap();
            checksum[0] ^= 1;
            disk.file.seek(SeekFrom::Start(512 + 16)).unwrap();
            disk.file.write_all(&checksum).unwrap();
            assert!(grow_root_device(&mut disk, 3, 1).unwrap().is_some());
            assert!(
                fs::read(&disk_path).unwrap() == completed,
                "GPT repair must preserve ext4 bytes"
            );
            fs::remove_dir_all(directory).unwrap();
        }
    }
}
