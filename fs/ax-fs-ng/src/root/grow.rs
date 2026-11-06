//! Independent boot-time partition and filesystem growth before publication.
//!
//! Partition growth runs first, like growpart. Filesystem growth then uses the
//! validated final volume, like resize2fs, without keeping an old-range owner.

use super::*;

/// Applies the boot growth policy to the actual selected writable root.
/// Mount access is independent of this policy; ordinary mount callers never
/// enter this path. Missing resizer capabilities preserve the existing volume.
pub(super) fn grow_selected_root(
    selected: &mut DiscoveredDisk,
    expected: &RootCandidate,
    selector: Option<&RootSelector>,
    action: RootAction,
) -> axfs_ng_vfs::VfsResult<Option<axfs_ng_vfs::Filesystem>> {
    if action == RootAction::MountReadOnly {
        return Ok(None);
    }
    #[cfg(feature = "ext4")]
    return grow_native_root(selected, expected, selector);
    #[cfg(not(feature = "ext4"))]
    {
        let _ = (selected, expected, selector);
        Ok(None)
    }
}

/// Revalidates geometry and identity after failed optional preparation.
/// The failed session and maintenance owner must already have been dropped.
pub(super) fn refresh_root_layout(
    selected: &mut DiscoveredDisk,
    expected: &RootCandidate,
    selector: Option<&RootSelector>,
) -> axfs_ng_vfs::VfsResult<()> {
    let layout = selected
        .handle
        .describe_volume(BlockRegion::from_num_blocks(
            selected.handle.device_info().num_blocks,
        ))
        .map_err(crate::block_error_to_vfs_error)?;
    let mut probe = selected
        .handle
        .open_volume(&layout, ax_block::VolumeAccess::ReadOnly)
        .map_err(crate::block_error_to_vfs_error)?;
    let (raw, partitions) = refresh_root_device(&mut probe, expected, selector)?;
    selected.layout = layout;
    selected.raw_filesystem = raw;
    selected.partitions = partitions;
    Ok(())
}

/// Rereads media rather than trusting a layout cached before failed preparation.
/// The caller retains a read lease through both the table scan and identity probes.
fn refresh_root_device(
    device: &mut dyn FsBlockDevice,
    expected: &RootCandidate,
    selector: Option<&RootSelector>,
) -> axfs_ng_vfs::VfsResult<(Option<FilesystemKind>, Vec<DetectedPartition>)> {
    let scan = crate::volume::scan_volumes(
        &mut crate::volume::DeviceReader::new(device),
        DiskId(expected.disk_index as u64),
    )
    .map_err(root_volume_error)?;
    validate_root_selection(device, &scan, expected, selector)
}

/// Rechecks the original choice while the caller retains maintenance or a read lease.
/// Only an unchanged origin and nonshrinking capacity may survive preparation;
/// table identity, labels, format and the explicit selector must still match.
fn validate_root_selection(
    device: &mut dyn FsBlockDevice,
    scan: &crate::volume::VolumeScan,
    expected: &RootCandidate,
    selector: Option<&RootSelector>,
) -> axfs_ng_vfs::VfsResult<(Option<FilesystemKind>, Vec<DetectedPartition>)> {
    let (raw, _, _, partitions) = collect_partitions(device, scan.volumes.clone());
    let index = expected.partition.as_ref().map(|part| part.info.index);
    // Default selection was a global decision. Retain its chosen identity;
    // a per-device snapshot must not rerun that policy or select a new root.
    if let Some(selector) = selector {
        let probe = matches!(selector, RootSelector::FsUuid(_)).then_some(device);
        let candidates = collect_device_root_candidates(
            probe,
            expected.disk_index,
            &expected.device_name,
            raw,
            &partitions,
        )?;
        if select_explicit_root(&candidates, selector) != Some((expected.disk_index, index)) {
            return Err(VfsError::NoSuchDevice);
        }
    }
    if let Some(previous) = &expected.partition {
        let current = partitions
            .iter()
            .find(|part| part.info.index == previous.info.index)
            .ok_or(VfsError::NoSuchDevice)?;
        let mut identity = previous.info.clone();
        // A completed or interrupted resizer may enlarge this same partition.
        // Its start, table kind, UUID, label and filesystem type cannot change.
        identity.region.end_lba = current.info.region.end_lba;
        if current.info.region.end_lba < previous.info.region.end_lba
            || current.info != identity
            || current.filesystem != previous.filesystem
        {
            return Err(VfsError::InvalidData);
        }
    } else if scan.volumes.len() != 1 || scan.volumes[0].table_kind != VolumeTableKind::Raw {
        return Err(VfsError::InvalidData);
    }
    Ok((raw, partitions))
}

#[cfg(feature = "ext4")]
fn grow_native_root(
    selected: &mut DiscoveredDisk,
    expected: &RootCandidate,
    selector: Option<&RootSelector>,
) -> axfs_ng_vfs::VfsResult<Option<axfs_ng_vfs::Filesystem>> {
    let index = expected.partition.as_ref().map(|part| part.info.index);
    let handle = selected.handle.clone();
    // Maintenance drains the cache, so reject ineligible devices before taking it.
    if handle.device_info().read_only || !handle.supports_flush() {
        return Ok(None);
    }
    let kind = selected_filesystem_kind(selected.raw_filesystem, &selected.partitions, index);
    let mut maintenance = handle.maintain().map_err(crate::block_error_to_vfs_error)?;
    let Some((scan, mounted)) = grow_root_device(&mut maintenance, expected, selector, kind)?
    else {
        return Ok(None);
    };
    let partition_id = index.map_or(0, |index| index + 1);
    let region = scan
        .volumes
        .iter()
        .find(|volume| volume.partition_id.0 as usize == partition_id)
        .map(region_from_volume)
        .ok_or(VfsError::InvalidData)?;
    let (raw, _, _, partitions) = collect_partitions(maintenance.device_mut(), scan.volumes);
    selected.raw_filesystem = raw;
    selected.partitions = partitions;
    selected.layout = handle
        .describe_volume(BlockRegion::from_num_blocks(
            handle.device_info().num_blocks,
        ))
        .map_err(crate::block_error_to_vfs_error)?;
    let Some(mounted) = mounted else {
        return Ok(None);
    };
    maintenance
        .publish(region, ax_block::VolumeAccess::ReadWrite)
        .map_err(crate::block_error_to_vfs_error)?;
    fs::new_from_prepared_ext4(handle, region, mounted).map(Some)
}

/// Performs independent partition growth, then prepares a filesystem within its
/// validated final bounds. A failed partition resize is reported separately;
/// filesystem recovery still decides whether that volume can be mounted.
#[cfg(feature = "ext4")]
fn grow_root_device(
    maintenance: &mut ax_block::DiskMaintenance,
    expected: &RootCandidate,
    selector: Option<&RootSelector>,
    kind: Option<FilesystemKind>,
) -> axfs_ng_vfs::VfsResult<Option<(crate::volume::VolumeScan, Option<fs::MountedExt4>)>> {
    use crate::volume::PartitionId;
    let disk_index = expected.disk_index;
    let index = expected.partition.as_ref().map(|part| part.info.index);
    if maintenance.device_mut().is_read_only() || !maintenance.device_mut().supports_flush() {
        return Ok(None);
    }
    let started = crate::os::monotonic_time();
    let partition = PartitionId(
        u32::try_from(index.map_or(0, |index| index + 1)).map_err(|_| VfsError::InvalidInput)?,
    );
    let before = maintenance
        .scan_layout(DiskId(disk_index as u64))
        .map_err(root_volume_error)?;
    validate_root_selection(maintenance.device_mut(), &before, expected, selector)?;
    let source = before
        .volumes
        .iter()
        .find(|volume| volume.partition_id == partition)
        .ok_or(VfsError::InvalidData)?;
    if index.is_some() {
        match maintenance.grow_partition(partition) {
            Ok(()) => {}
            Err(VolumeError::Unsupported) => debug!(
                "automatic partition growth unsupported on disk {disk_index} partition {}",
                partition.0
            ),
            Err(error) => warn!(
                "automatic partition growth failed on disk {disk_index} partition {}: {error}; \
                 validating existing volume",
                partition.0
            ),
        }
    }
    let scan = maintenance
        .scan_layout(DiskId(disk_index as u64))
        .map_err(root_volume_error)?;
    let volume = scan
        .volumes
        .iter()
        .find(|volume| volume.partition_id == partition)
        .ok_or(VfsError::InvalidData)?;
    if volume.region.start_block != source.region.start_block
        || volume.region.num_blocks < source.region.num_blocks
        || volume.table_kind != source.table_kind
        || volume.partuuid != source.partuuid
    {
        return Err(VfsError::InvalidData);
    }
    validate_root_selection(maintenance.device_mut(), &scan, expected, selector)?;
    if kind != Some(FilesystemKind::Ext4) {
        debug!("automatic filesystem growth has no resizer for {kind:?}");
        return Ok(Some((scan, None)));
    }
    let region = region_from_volume(volume);
    let disk = maintenance.device_mut();
    let target_bytes = region
        .num_blocks()
        .checked_mul(disk.block_size() as u64)
        .ok_or(VfsError::InvalidInput)?;
    let backend = disk.fork_io().map_err(crate::block_error_to_vfs_error)?;
    let mut session = fs::prepare_root_growth(backend, region).map_err(root_ext4_error)?;
    let stats = session.statfs();
    let current_bytes = stats
        .total_blocks
        .checked_mul(stats.block_size)
        .ok_or(VfsError::InvalidData)?;
    if let Some(capacity_bytes) = session
        .growth_capacity_if_supported(target_bytes)
        .map_err(root_ext4_error)?
        && capacity_bytes > current_bytes
    {
        info!(
            "root filesystem growth on disk {disk_index} partition {}: {current_bytes} to \
             {capacity_bytes} bytes",
            partition.0
        );
        session = session
            .grow_to_current_capacity()
            .map_err(root_ext4_error)?;
        info!(
            "root filesystem growth ready on disk {disk_index} partition {} in {} ms",
            partition.0,
            crate::os::monotonic_time()
                .saturating_sub(started)
                .as_millis()
        );
    }
    let mounted = session.publish().map_err(root_ext4_error)?;
    Ok(Some((scan, Some(mounted))))
}

/// Preserves volume error categories at the filesystem preparation boundary.
fn root_volume_error(error: crate::volume::Error) -> VfsError {
    use crate::volume::Error as VolumeError;
    match error {
        VolumeError::NoMemory => VfsError::NoMemory,
        VolumeError::Reader | VolumeError::Writer => VfsError::Io,
        VolumeError::Unsupported => VfsError::OperationNotSupported,
        _ => VfsError::InvalidData,
    }
}

#[cfg(feature = "ext4")]
/// Uses the same ext4-to-VFS mapping as ordinary filesystem operations.
fn root_ext4_error(error: rsext4::Ext4Error) -> VfsError {
    fs::ext4_error_to_vfs_error(error)
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
            atomic::{AtomicBool, AtomicUsize, Ordering},
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
        /// One injected table-write failure; subsequent filesystem I/O can recover.
        fail_next_write: AtomicBool,
        /// Absolute one-based write to fail after a durable GPT backup.
        fail_write_at: AtomicUsize,
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
            let write = self.trace.writes.fetch_add(1, Ordering::Relaxed) + 1;
            if self.trace.fail_next_write.swap(false, Ordering::Relaxed)
                || self.trace.fail_write_at.load(Ordering::Relaxed) == write
            {
                return Err(BlockError::Io);
            }
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
        /// cursor; capability skips must not issue I/O through the forked view.
        fn fork_io(&self) -> BlockResult<Box<dyn FsBlockDevice>> {
            let mut view = Self::open(self.path.clone());
            view.trace = self.trace.clone();
            view.readonly = self.readonly;
            view.flush = self.flush;
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

    /// The production helper returns the live owner; the fixture explicitly
    /// unmounts that same owner so Linux can check the resulting clean image.
    fn grow_image(
        disk: &mut dyn FsBlockDevice,
        disk_index: usize,
        index: Option<usize>,
    ) -> axfs_ng_vfs::VfsResult<Option<crate::volume::VolumeScan>> {
        let scan = scan_volumes(&mut VolumeReader::new(disk), DiskId(disk_index as u64)).unwrap();
        let (raw, _, _, partitions) = collect_partitions(disk, scan.volumes);
        let expected =
            collect_device_root_candidates(None, disk_index, "fixture", raw, &partitions)?
                .into_iter()
                .find(|candidate| candidate.partition.as_ref().map(|part| part.info.index) == index)
                .ok_or(VfsError::NoSuchDevice)?;
        grow_observed_image(disk, &expected, None)
    }

    /// Enters maintenance after an earlier observation, permitting controlled layout changes.
    fn grow_observed_image(
        disk: &mut dyn FsBlockDevice,
        expected: &RootCandidate,
        selector: Option<&RootSelector>,
    ) -> axfs_ng_vfs::VfsResult<Option<crate::volume::VolumeScan>> {
        let index = expected.partition.as_ref().map(|part| part.info.index);
        let backend = disk.fork_io().map_err(crate::block_error_to_vfs_error)?;
        let mut maintenance = ax_block::DiskMaintenance::from_test_backend(backend);
        let Some((scan, mut mounted)) = super::grow_root_device(
            &mut maintenance,
            expected,
            selector,
            Some(FilesystemKind::Ext4),
        )?
        else {
            return Ok(None);
        };
        let partition =
            crate::volume::PartitionId(u32::try_from(index.map_or(0, |index| index + 1)).unwrap());
        let region = scan
            .volumes
            .iter()
            .find(|volume| volume.partition_id == partition)
            .map(region_from_volume)
            .unwrap();
        maintenance
            .publish(region, ax_block::VolumeAccess::ReadWrite)
            .map_err(crate::block_error_to_vfs_error)?;
        mounted
            .as_mut()
            .unwrap()
            .unmount()
            .map_err(root_ext4_error)?;
        Ok(Some(scan))
    }

    /// Grows only the root tail, skips read-only/no-flush devices, and verifies other
    /// partition identity, ext4 contents and a repeated run using Linux host tools.
    #[test]
    fn root_growth_preserves_other_partitions_and_ext4_contents() {
        // Cover both a usable partial group and a tail too small for metadata.
        for (table, disk_bytes) in [
            ("gpt", 128 * 1024 * 1024),
            ("gpt", 130 * 1024 * 1024 + 4096 + 33 * 512),
            ("dos", 128 * 1024 * 1024),
        ] {
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
            if table == "gpt" && disk_bytes == 128 * 1024 * 1024 {
                // Whole-device ext4 growth must not depend on any partition table.
                let raw_path = directory.join("raw.img");
                fs::copy(&root_path, &raw_path).unwrap();
                let mut raw = Image::open(raw_path.clone());
                raw.file.set_len(96 * 1024 * 1024).unwrap();
                let scan = grow_image(&mut raw, 4, None).unwrap().unwrap();
                assert_eq!(
                    scan.volumes[0].table_kind,
                    crate::volume::PartitionTableKind::Raw
                );
                command("e2fsck", &["-fn", raw_path.to_str().unwrap()]);
                let output = Command::new("debugfs")
                    .args(["-R", "cat /keep"])
                    .arg(&raw_path)
                    .output()
                    .unwrap();
                assert!(output.status.success());
                assert_eq!(output.stdout, b"preserved-root-payload");
                let capacity = raw.num_blocks();
                let superblock = read_filesystem_bytes(
                    &mut raw,
                    BlockRegion::from_num_blocks(capacity),
                    1024,
                    rsext4::SUPERBLOCK_SIZE,
                )
                .unwrap();
                let superblock = Ext4Superblock::from_disk_bytes(&superblock);
                assert_eq!(
                    superblock.blocks_count() * superblock.block_size(),
                    96 * 1024 * 1024
                );
            }
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
            let layout = if table == "gpt" {
                "label: gpt\nstart=2048,size=2048,type=linux,name=boot\nstart=4096,size=98304,\
                 type=linux,name=rootfs\n"
            } else {
                "label: dos\nlabel-id: \
                 0x00000000\nstart=2048,size=2048,type=linux\nstart=4096,size=98304,type=linux\n"
            };
            sfdisk
                .stdin
                .take()
                .unwrap()
                .write_all(layout.as_bytes())
                .unwrap();
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
            if table == "gpt" && disk_bytes == 128 * 1024 * 1024 {
                let saved = fs::read(&disk_path).unwrap();
                for change in ["partuuid", "partlabel", "uuid"] {
                    let (_, _, _, parts) = collect_partitions(&mut disk, original.volumes.clone());
                    let expected =
                        collect_device_root_candidates(Some(&mut disk), 3, "fixture", None, &parts)
                            .unwrap()
                            .into_iter()
                            .find(|candidate| {
                                candidate.partition.as_ref().map(|part| part.info.index) == Some(1)
                            })
                            .unwrap();
                    let selector = match change {
                        "partuuid" => RootSelector::PartUuid(
                            expected
                                .partition
                                .as_ref()
                                .unwrap()
                                .info
                                .part_uuid
                                .clone()
                                .unwrap(),
                        ),
                        "partlabel" => RootSelector::PartLabel("rootfs".into()),
                        _ => RootSelector::FsUuid(expected_uuid.clone()),
                    };
                    assert_eq!(
                        select_root_candidate(core::slice::from_ref(&expected), Some(&selector)),
                        Some((3, Some(1)))
                    );
                    // The same request must remain usable before its identity changes.
                    assert!(
                        validate_root_selection(&mut disk, &original, &expected, Some(&selector))
                            .is_ok()
                    );
                    match change {
                        "partuuid" => command(
                            "sfdisk",
                            &[
                                "--part-uuid",
                                disk_path.to_str().unwrap(),
                                "2",
                                "11111111-2222-3333-4444-555555555555",
                            ],
                        ),
                        "partlabel" => command(
                            "sfdisk",
                            &[
                                "--part-label",
                                disk_path.to_str().unwrap(),
                                "2",
                                "replacement",
                            ],
                        ),
                        _ => {
                            let replacement = directory.join("replacement.img");
                            fs::copy(&root_path, &replacement).unwrap();
                            command(
                                "tune2fs",
                                &[
                                    "-U",
                                    "11111111-2222-3333-4444-555555555555",
                                    replacement.to_str().unwrap(),
                                ],
                            );
                            disk.file
                                .seek(SeekFrom::Start(region.start_block * 512))
                                .unwrap();
                            std::io::copy(&mut File::open(&replacement).unwrap(), &mut disk.file)
                                .unwrap();
                        }
                    }
                    let writes = disk.trace.writes.load(Ordering::Relaxed);
                    let flushes = disk.trace.flushes.load(Ordering::Relaxed);
                    assert!(
                        grow_observed_image(&mut disk, &expected, Some(&selector)).is_err(),
                        "{change}: a replaced root must fail before growth or publication"
                    );
                    // Metadata identity is pinned for default selection too, even
                    // when the new volume would still win the same rootfs slot.
                    if change != "uuid" {
                        assert!(grow_observed_image(&mut disk, &expected, None).is_err());
                    }
                    // The same read-only validation protects ordinary-mount fallback.
                    assert!(refresh_root_device(&mut disk, &expected, Some(&selector)).is_err());
                    assert_eq!(disk.trace.writes.load(Ordering::Relaxed), writes);
                    assert_eq!(disk.trace.flushes.load(Ordering::Relaxed), flushes);
                    disk.file.seek(SeekFrom::Start(0)).unwrap();
                    disk.file.write_all(&saved).unwrap();
                }
            }
            // Automatic growth skips missing capabilities before changing metadata.
            let before = fs::read(&disk_path).unwrap();
            disk.readonly = true;
            assert!(grow_image(&mut disk, 3, Some(1)).unwrap().is_none());
            disk.readonly = false;
            disk.flush = false;
            assert!(grow_image(&mut disk, 3, Some(1)).unwrap().is_none());
            assert_eq!(fs::read(&disk_path).unwrap(), before);
            disk.flush = true;
            // Failure of the optional partition stage must not reject a root
            // that its independent normal recovery can still mount.
            disk.trace.fail_next_write.store(true, Ordering::Relaxed);
            let unchanged = grow_image(&mut disk, 3, Some(1)).unwrap().unwrap();
            assert_eq!(unchanged.volumes, original.volumes);
            if table == "gpt" {
                // A durable enlarged backup with the old valid primary must
                // remain retryable through the maintenance capability boundary.
                let mut header = [0; 512];
                disk.file.seek(SeekFrom::Start(512)).unwrap();
                disk.file.read_exact(&mut header).unwrap();
                let count = u32::from_le_bytes(header[80..84].try_into().unwrap());
                let size = u32::from_le_bytes(header[84..88].try_into().unwrap());
                let entries = (u64::from(count) * u64::from(size)).div_ceil(512) as usize;
                let cut = disk.trace.writes.load(Ordering::Relaxed) + entries + 2;
                disk.trace.fail_write_at.store(cut, Ordering::Relaxed);
                let interrupted = grow_image(&mut disk, 3, Some(1)).unwrap().unwrap();
                assert_eq!(interrupted.volumes, original.volumes);
                disk.trace.fail_write_at.store(0, Ordering::Relaxed);
            }
            let mut before_mount_count = [0u8; 2];
            disk.file
                .seek(SeekFrom::Start(4096 * 512 + 1024 + 52))
                .unwrap();
            disk.file.read_exact(&mut before_mount_count).unwrap();
            let scan = grow_image(&mut disk, 3, Some(1)).unwrap().unwrap();
            let mut after_mount_count = [0u8; 2];
            disk.file
                .seek(SeekFrom::Start(4096 * 512 + 1024 + 52))
                .unwrap();
            disk.file.read_exact(&mut after_mount_count).unwrap();
            assert_eq!(
                u16::from_le_bytes(after_mount_count),
                u16::from_le_bytes(before_mount_count) + 1,
                "GPT planning and ext4 growth must recover through one mount session",
            );
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
            assert_eq!(
                grow_image(&mut disk, 3, Some(1)).unwrap().unwrap().volumes,
                scan.volumes
            );
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
            // A repeated boot still recovers once, but preserves
            // committed geometry rather than publishing duplicate growth.
            let completed = fs::read(&disk_path).unwrap();
            assert_eq!(
                grow_image(&mut disk, 3, Some(1)).unwrap().unwrap().volumes,
                scan.volumes
            );
            if table == "gpt" {
                // A complete filesystem must not prevent repair of an invalid GPT copy.
                disk.file.seek(SeekFrom::Start(512 + 16)).unwrap();
                let mut checksum = [0];
                disk.file.read_exact(&mut checksum).unwrap();
                checksum[0] ^= 1;
                disk.file.seek(SeekFrom::Start(512 + 16)).unwrap();
                disk.file.write_all(&checksum).unwrap();
                assert_eq!(
                    grow_image(&mut disk, 3, Some(1)).unwrap().unwrap().volumes,
                    scan.volumes
                );
                assert!(
                    fs::read(&disk_path).unwrap()[..4096 * 512] == completed[..4096 * 512],
                    "GPT repair must preserve partition identity and the sibling partition"
                );
            }
            fs::remove_dir_all(directory).unwrap();
        }
    }
}
