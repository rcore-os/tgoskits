//! One real-image journey through native FAT metadata and its completion boundary.

use core::time::Duration;

use axfs_ng_vfs::{
    Filesystem, Location, Metadata, MetadataChange, Mountpoint, MutationCredentials,
    NamespaceOperationScope, NamespaceOutcome, NamespaceState, NodePermission, NodeType,
    PreparedWrite, TimeChange, VfsError,
};
use chrono::{TimeZone, Utc};

use super::{
    Arc, BlockRegion, FsBlockError, FsBlockResult, MemoryBlockDevice, Mutex, SeekableDisk,
};
use crate::fs::fat::FatFilesystem;

const CONTENTS: &[u8] = b"native FAT metadata";

/// Per-device state: it injects storage failures, never metadata results.
#[derive(Default)]
pub(super) struct FlushControl {
    attempts: usize,
    failures: usize,
    fail_next: bool,
}

impl FlushControl {
    pub(super) fn record_flush(&mut self) -> FsBlockResult<()> {
        self.attempts += 1;
        if core::mem::take(&mut self.fail_next) {
            self.failures += 1;
            Err(FsBlockError::Io)
        } else {
            Ok(())
        }
    }
}

fn utc(date: (i32, u32, u32), time: (u32, u32, u32)) -> Duration {
    let timestamp = Utc
        .with_ymd_and_hms(date.0, date.1, date.2, time.0, time.1, time.2)
        .single()
        .unwrap()
        .timestamp();
    Duration::from_secs(timestamp.try_into().unwrap())
}

fn remount(storage: &Arc<Mutex<Vec<u8>>>, control: &Arc<Mutex<FlushControl>>) -> Filesystem {
    FatFilesystem::new(
        Box::new(MemoryBlockDevice {
            storage: storage.clone(),
            fail_flush: false,
            read_only: false,
            flush_control: Some(control.clone()),
        }),
        BlockRegion::from_num_blocks(4096),
        None,
    )
    .unwrap()
}

/// Read birth directly from a directory entry, only after every previous owner
/// has gone away. No raw patch or second live editor participates in the test.
fn persisted_birth(storage: &Arc<Mutex<Vec<u8>>>) -> fatfs::DateTime {
    assert_eq!(Arc::strong_count(storage), 1, "old FAT holders remain live");
    let disk = SeekableDisk::new(
        Box::new(MemoryBlockDevice {
            storage: storage.clone(),
            fail_flush: false,
            read_only: false,
            flush_control: None,
        }),
        BlockRegion::from_num_blocks(4096),
    );
    let filesystem = crate::fs::fat::ff::FileSystem::new(disk, fatfs::FsOptions::new()).unwrap();
    let birth = {
        let root = filesystem.root_dir();
        root.iter()
            .map(Result::unwrap)
            .find(|entry| entry.eq_name("journey.txt"))
            .expect("persisted FAT directory entry")
            .created()
    };
    filesystem.unmount().unwrap();
    birth
}

fn times(location: &Location) -> (Duration, Duration, Duration) {
    let metadata = location.entry().metadata().unwrap();
    (metadata.atime, metadata.mtime, metadata.ctime)
}

fn assert_fixed(metadata: &Metadata) {
    assert_eq!(
        (metadata.uid, metadata.gid, metadata.mode.bits()),
        (0, 0, 0o666)
    );
}

fn assert_same_metadata(actual: &Metadata, expected: &Metadata) {
    assert_eq!(actual.inode, expected.inode);
    assert_eq!(actual.node_type, expected.node_type);
    assert_eq!(actual.size, expected.size);
    assert_eq!(actual.mode.bits(), expected.mode.bits());
    assert_eq!((actual.uid, actual.gid), (expected.uid, expected.gid));
    assert_eq!(
        (actual.atime, actual.mtime, actual.ctime),
        (expected.atime, expected.mtime, expected.ctime)
    );
}

/// Use the production operation owner to retain typed state through finish.
fn change_once(
    target: &Location,
    change: MetadataChange,
    credentials: &MutationCredentials<'_>,
) -> NamespaceOutcome<Metadata> {
    let write = PreparedWrite::acquire(target).unwrap();
    let scope = NamespaceOperationScope::<Metadata>::acquire(&write).unwrap();
    {
        let context = scope.context();
        scope
            .change_metadata(&context, target, change, credentials)
            .unwrap()
            .expect("nonempty FAT command must produce an observation");
    }
    scope
        .finish()
        .unwrap_or_else(|_| panic!("FAT metadata completion changed its admitted owner"))
}

fn assert_unchanged(
    target: &Location,
    change: MetadataChange,
    credentials: &MutationCredentials<'_>,
    result: Result<(), VfsError>,
) {
    let before = target.entry().metadata().unwrap();
    let completed = change_once(target, change, credentials);
    assert_eq!(completed.state(), NamespaceState::Unchanged);
    assert_eq!(completed.result(), result);
    assert_same_metadata(completed.observation().unwrap(), &before);
    assert_same_metadata(&target.entry().metadata().unwrap(), &before);
}

#[test]
fn fat_metadata_journey_preserves_identity_authorization_and_persistent_times() {
    // The existing host fixture installs the real wall_time provider used by
    // MetadataRequest::execute, including requests containing only explicit Set.
    ax_block::os::task::install_test_runtime_ops();
    let control = Arc::new(Mutex::new(FlushControl::default()));
    let (filesystem, storage) = super::formatted_fat_with_flush_control(Some(control.clone()));
    let privileged = MutationCredentials::root();

    // Establish birth and contents through normal VFS creation, then release
    // all owners before observing the actual on-disk birth timestamp.
    let old_filesystem = filesystem.downgrade();
    {
        let root = Mountpoint::new_root(&filesystem).root_location();
        let file = root
            .create(
                "journey.txt",
                NodeType::RegularFile,
                NodePermission::default(),
                0,
                0,
                &privileged,
            )
            .unwrap();
        let opened = file.entry().as_file().unwrap();
        assert_eq!(opened.write_at(CONTENTS, 0).unwrap(), CONTENTS.len());
        opened.sync(false).unwrap();
        root.entry().filesystem().flush().unwrap();
        root.entry().as_dir().unwrap().clear_cached_entries();
    }
    drop(filesystem);
    assert!(!old_filesystem.is_alive());
    let birth = persisted_birth(&storage);

    let filesystem = remount(&storage, &control);
    let root = Mountpoint::new_root(&filesystem).root_location();
    let file = root.lookup_no_follow("journey.txt").unwrap();
    let atime = utc((2023, 5, 6), (17, 18, 19)) + Duration::from_nanos(987_654_321);
    let mtime = utc((2024, 7, 8), (13, 14, 15)) + Duration::from_nanos(123_456_789);

    // This is the first metadata behavior oracle: the unmigrated NodeOps
    // default returns OperationNotSupported, rather than reaching a helper.
    assert_eq!(
        file.change_metadata(
            MetadataChange::Times {
                atime: TimeChange::Set(atime),
                mtime: TimeChange::Set(mtime),
            },
            &privileged,
        ),
        Ok(()),
        "native FAT file Times Set must succeed through Location"
    );
    let alias = root.lookup_no_follow("JOURNEY.TXT").unwrap();
    assert!(alias.entry().ptr_eq(file.entry()));
    assert_eq!(alias.entry().identity(), file.entry().identity());
    assert!(file.entry().identity().is_some());
    let midnight = utc((2023, 5, 6), (0, 0, 0));
    let even_mtime = utc((2024, 7, 8), (13, 14, 14));
    assert_eq!(times(&alias), (midnight, even_mtime, even_mtime));
    assert_same_metadata(
        &alias.entry().metadata().unwrap(),
        &file.entry().metadata().unwrap(),
    );
    assert_ne!(
        birth,
        fatfs::DateTime::new(
            fatfs::Date::new(2024, 7, 8),
            fatfs::Time::new(13, 14, 14, 0),
        )
    );

    let next_mtime = utc((2025, 3, 4), (5, 6, 7)) + Duration::from_nanos(999_999_999);
    alias
        .change_metadata(
            MetadataChange::Times {
                atime: TimeChange::Keep,
                mtime: TimeChange::Set(next_mtime),
            },
            &privileged,
        )
        .unwrap();
    let even_mtime = utc((2025, 3, 4), (5, 6, 6));
    assert_eq!(times(&file), (midnight, even_mtime, even_mtime));
    file.change_metadata(
        MetadataChange::Times {
            atime: TimeChange::Set(utc((2026, 8, 9), (10, 11, 13))),
            mtime: TimeChange::Keep,
        },
        &privileged,
    )
    .unwrap();
    assert_eq!(
        times(&alias),
        (utc((2026, 8, 9), (0, 0, 0)), even_mtime, even_mtime)
    );

    let directory = root
        .create(
            "directory",
            NodeType::Directory,
            NodePermission::default(),
            0,
            0,
            &privileged,
        )
        .unwrap();
    let opened = file.entry().as_file().unwrap();
    // Leave the canonical file dirty: fixed projection commands must not flush
    // unrelated pending content just to return a successful no-op.
    assert_eq!(opened.write_at(CONTENTS, 0).unwrap(), CONTENTS.len());
    let flushes = control.lock().unwrap().attempts;
    control.lock().unwrap().fail_next = true;
    let owner = MutationCredentials {
        fsuid: 0,
        fsgid: 0,
        supplementary_gids: &[],
        cap_dac_override: false,
        cap_dac_read_search: false,
        cap_fowner: false,
        cap_mknod: false,
        cap_chown: false,
        cap_fsetid: false,
    };
    let stranger = MutationCredentials {
        fsuid: 42,
        fsgid: 42,
        ..owner
    };
    for target in [&file, &directory, &root] {
        assert_fixed(&target.entry().metadata().unwrap());
        assert_unchanged(
            target,
            MetadataChange::Chmod(NodePermission::default()),
            &owner,
            Ok(()),
        );
        assert_unchanged(
            target,
            MetadataChange::Chown {
                uid: Some(0),
                gid: Some(0),
            },
            &owner,
            Ok(()),
        );
        assert_unchanged(
            target,
            MetadataChange::Chown {
                uid: None,
                gid: None,
            },
            &stranger,
            Ok(()),
        );
        for change in [
            MetadataChange::Chmod(NodePermission::default()),
            MetadataChange::Chown {
                uid: Some(0),
                gid: None,
            },
            MetadataChange::Chown {
                uid: None,
                gid: Some(0),
            },
        ] {
            assert_unchanged(
                target,
                change,
                &stranger,
                Err(VfsError::OperationNotPermitted),
            );
        }
        for change in [
            MetadataChange::Chown {
                uid: Some(1),
                gid: None,
            },
            MetadataChange::Chown {
                uid: None,
                gid: Some(1),
            },
        ] {
            assert_unchanged(
                target,
                change,
                &privileged,
                Err(VfsError::OperationNotPermitted),
            );
        }
        assert_unchanged(
            target,
            MetadataChange::Chmod(NodePermission::from_bits_truncate(0o600)),
            &privileged,
            Err(VfsError::OperationNotSupported),
        );
        target
            .change_metadata(
                MetadataChange::Times {
                    atime: TimeChange::Keep,
                    mtime: TimeChange::Keep,
                },
                &stranger,
            )
            .unwrap();
    }
    assert_eq!(
        times(&root),
        (Duration::ZERO, Duration::ZERO, Duration::ZERO)
    );
    for target in [&directory, &root] {
        let current = target.entry().metadata().unwrap();
        assert_unchanged(
            target,
            MetadataChange::Times {
                atime: TimeChange::Set(current.atime),
                mtime: TimeChange::Set(current.mtime),
            },
            &privileged,
            Err(VfsError::OperationNotSupported),
        );
    }
    assert_eq!(
        control.lock().unwrap().attempts,
        flushes,
        "metadata reads/no-ops flushed"
    );
    assert!(
        control.lock().unwrap().fail_next,
        "an implicit flush swallowed the fault"
    );

    // The one pending storage fault must be consumed by this explicit native
    // flush. The real editor is already changed, so Io cannot mean Unchanged,
    // Uncertain, or a replayable Retry.
    let failed_mtime = utc((2030, 10, 11), (12, 13, 15)) + Duration::from_nanos(123);
    let failed_atime = utc((2029, 9, 8), (7, 6, 5));
    let completed = change_once(
        &alias,
        MetadataChange::Times {
            atime: TimeChange::Set(failed_atime),
            mtime: TimeChange::Set(failed_mtime),
        },
        &privileged,
    );
    let actual_mtime = utc((2030, 10, 11), (12, 13, 14));
    let actual_atime = utc((2029, 9, 8), (0, 0, 0));
    assert_eq!(completed.state(), NamespaceState::Applied);
    assert_eq!(completed.result(), Err(VfsError::Io));
    let actual = completed.observation().unwrap();
    assert_eq!(
        (actual.atime, actual.mtime, actual.ctime),
        (actual_atime, actual_mtime, actual_mtime)
    );
    assert_same_metadata(actual, &file.entry().metadata().unwrap());
    assert_same_metadata(actual, &alias.entry().metadata().unwrap());
    assert_eq!(
        control.lock().unwrap().attempts,
        flushes + 1,
        "failed command was replayed"
    );
    assert_eq!(control.lock().unwrap().failures, 1);
    assert!(!control.lock().unwrap().fail_next);
    assert_eq!(completed.into_result(), Err(VfsError::Io));

    // Clamp whole instants before calendar conversion, not just their years.
    // Both limits pass through the real command, including the overflow input.
    for (requested, expected_atime, expected_mtime) in [
        (
            utc((1975, 7, 8), (12, 13, 15)) + Duration::from_nanos(999),
            utc((1980, 1, 1), (0, 0, 0)),
            utc((1980, 1, 1), (0, 0, 0)),
        ),
        (
            Duration::MAX,
            utc((2107, 12, 31), (0, 0, 0)),
            utc((2107, 12, 31), (23, 59, 58)),
        ),
    ] {
        file.change_metadata(
            MetadataChange::Times {
                atime: TimeChange::Set(requested),
                mtime: TimeChange::Set(requested),
            },
            &privileged,
        )
        .unwrap();
        assert_eq!(
            times(&alias),
            (expected_atime, expected_mtime, expected_mtime)
        );
    }
    opened.sync(false).unwrap();
    root.entry().filesystem().flush().unwrap();
    let expected = file.entry().metadata().unwrap();
    let old_filesystem = filesystem.downgrade();
    // Positive retention is independent of the mount. Release this journey's
    // cached children as well as its explicit handles before reopening storage.
    root.entry().as_dir().unwrap().clear_cached_entries();
    drop(opened);
    drop(alias);
    drop(file);
    drop(directory);
    drop(root);
    drop(filesystem);
    assert!(!old_filesystem.is_alive());
    assert_eq!(
        persisted_birth(&storage),
        birth,
        "ctime must not overwrite birth"
    );

    // Fresh FAT instance and fresh holders: old editor reads are not durability.
    let filesystem = remount(&storage, &control);
    let root = Mountpoint::new_root(&filesystem).root_location();
    let reopened = root.lookup_no_follow("JOURNEY.TXT").unwrap();
    let actual = reopened.entry().metadata().unwrap();
    assert_fixed(&actual);
    assert_eq!(
        (actual.atime, actual.mtime, actual.ctime),
        (expected.atime, expected.mtime, expected.ctime)
    );
    assert_eq!(actual.size, CONTENTS.len() as u64);
    let mut contents = [0; CONTENTS.len()];
    assert_eq!(
        reopened
            .entry()
            .as_file()
            .unwrap()
            .read_at(&mut contents, 0),
        Ok(CONTENTS.len())
    );
    assert_eq!(&contents, CONTENTS);
    root.entry().as_dir().unwrap().clear_cached_entries();
}
