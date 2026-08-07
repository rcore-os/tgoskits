#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use ax_std as _;
use axvm as _;

#[cfg(feature = "fs")]
#[path = "../src/shell/command/fs.rs"]
mod shell_fs;

#[axtest::tests]
mod tests {
    use axtest::prelude::*;
    #[cfg(feature = "fs")]
    use std::{
        ffi::OsString,
        fs::{self, FileTimes, OpenOptions},
        io::{self, ErrorKind},
        string::ToString,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    #[cfg(feature = "fs")]
    use super::shell_fs::{
        CopyMode, RemoveOptions, collect_directory_entry_names, copy_after_rename_failure,
        copy_operands, copy_path, ensure_recursive_destination_outside_source, ignore_remove_error,
        metadata_for_remove, move_file_or_dir, remove_path, touch_file,
    };

    #[test]
    fn axvisor_axtest_smoke() {
        ax_assert!(true);
    }

    #[cfg(feature = "fs")]
    #[test]
    fn touch_preserves_content_and_updates_times() {
        let path = "/tmp/axvisor-touch-regression";
        let old_time = SystemTime::now()
            .checked_add(Duration::from_secs(60))
            .expect("fixture time must be representable");
        let _ = fs::remove_file(path);
        fs::write(path, b"preserve me").expect("create touch fixture");
        OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open touch fixture")
            .set_times(
                FileTimes::new()
                    .set_accessed(old_time)
                    .set_modified(old_time),
            )
            .expect("set old fixture times");

        let earliest_touch_time = unix_seconds(SystemTime::now());
        touch_file(path).expect("touch fixture");
        let latest_touch_time = unix_seconds(SystemTime::now());

        let metadata = fs::metadata(path).expect("read touched metadata");
        ax_assert_eq!(fs::read(path).expect("read touched file"), b"preserve me");
        let accessed = unix_seconds(metadata.accessed().expect("read atime"));
        let modified = unix_seconds(metadata.modified().expect("read mtime"));
        ax_assert!((earliest_touch_time..=latest_touch_time).contains(&accessed));
        ax_assert!((earliest_touch_time..=latest_touch_time).contains(&modified));
        fs::remove_file(path).expect("remove touch fixture");
    }

    #[cfg(feature = "fs")]
    #[test]
    fn cp_file_to_existing_directory_uses_source_basename() {
        let root = "/tmp/axvisor-cp-file-regression";
        reset_test_dir(root);
        let source = format!("{root}/source.txt");
        let destination = format!("{root}/destination");
        fs::write(&source, b"copied payload").expect("create copy source");
        fs::create_dir(&destination).expect("create copy destination");

        copy_path(&source, &destination, CopyMode::File).expect("copy file into directory");

        ax_assert_eq!(
            fs::read(format!("{destination}/source.txt")).expect("read copied file"),
            b"copied payload"
        );
        remove_path(
            root,
            RemoveOptions {
                recursive: true,
                ..RemoveOptions::default()
            },
        )
        .expect("remove copy fixture");
    }

    #[cfg(feature = "fs")]
    #[test]
    fn cp_rejects_copying_file_onto_itself_without_truncating_it() {
        let path = "/tmp/axvisor-cp-self-file-regression";
        let _ = fs::remove_file(path);
        fs::write(path, b"keep this payload").expect("create self-copy fixture");

        let error = copy_path(path, path, CopyMode::File)
            .expect_err("copying a file onto itself must fail");

        ax_assert_eq!(error.kind(), ErrorKind::InvalidInput);
        ax_assert_eq!(
            fs::read(path).expect("read self-copy fixture"),
            b"keep this payload"
        );
        fs::remove_file(path).expect("remove self-copy fixture");
    }

    #[cfg(feature = "fs")]
    #[test]
    fn cp_recursive_directory_to_existing_directory_uses_source_basename() {
        let root = "/tmp/axvisor-cp-dir-regression";
        reset_test_dir(root);
        let source = format!("{root}/source-dir");
        let destination = format!("{root}/destination");
        fs::create_dir(&source).expect("create recursive copy source");
        fs::write(format!("{source}/child.txt"), b"recursive payload")
            .expect("create recursive copy child");
        fs::create_dir(&destination).expect("create recursive copy destination");

        copy_path(&source, &destination, CopyMode::Recursive)
            .expect("copy directory into directory");

        ax_assert_eq!(
            fs::read(format!("{destination}/source-dir/child.txt"))
                .expect("read recursively copied file"),
            b"recursive payload"
        );
        remove_path(
            root,
            RemoveOptions {
                recursive: true,
                ..RemoveOptions::default()
            },
        )
        .expect("remove recursive copy fixture");
    }

    #[cfg(feature = "fs")]
    #[test]
    fn cp_recursive_rejects_copying_directory_into_itself() {
        let root = "/tmp/axvisor-cp-self-regression";
        reset_test_dir(root);
        let source = format!("{root}/dir");
        fs::create_dir(&source).expect("create recursive copy source");
        fs::create_dir(format!("{source}/dir")).expect("create recursion guard");

        let error = copy_path(&source, &source, CopyMode::Recursive)
            .expect_err("recursive copy into itself must fail");

        ax_assert_eq!(error.kind(), ErrorKind::InvalidInput);
        remove_path(
            root,
            RemoveOptions {
                recursive: true,
                ..RemoveOptions::default()
            },
        )
        .expect("remove self-copy fixture");
    }

    #[cfg(feature = "fs")]
    #[test]
    fn cp_recursive_rejects_copying_directory_into_descendant() {
        let root = "/tmp/axvisor-cp-descendant-regression";
        reset_test_dir(root);
        let source = format!("{root}/dir");
        let destination = format!("{source}/subdir");
        fs::create_dir(&source).expect("create recursive copy source");
        fs::create_dir(&destination).expect("create descendant destination");
        fs::create_dir(format!("{destination}/dir")).expect("create recursion guard");

        let error = copy_path(&source, &destination, CopyMode::Recursive)
            .expect_err("recursive copy into a descendant must fail");

        ax_assert_eq!(error.kind(), ErrorKind::InvalidInput);
        remove_path(
            root,
            RemoveOptions {
                recursive: true,
                ..RemoveOptions::default()
            },
        )
        .expect("remove descendant-copy fixture");
    }

    #[cfg(feature = "fs")]
    #[test]
    fn cp_recursive_rejects_nonexistent_descendant_before_creation() {
        let root = "/tmp/axvisor-cp-new-descendant-regression";
        reset_test_dir(root);
        let source = format!("{root}/dir");
        let destination = format!("{source}/subdir");
        fs::create_dir(&source).expect("create recursive copy source");

        let error = ensure_recursive_destination_outside_source(&source, &destination)
            .expect_err("nonexistent descendant must be rejected before creation");

        ax_assert_eq!(error.kind(), ErrorKind::InvalidInput);
        ax_assert!(!fs::exists(&destination).expect("check descendant was not created"));
        remove_path(
            root,
            RemoveOptions {
                recursive: true,
                ..RemoveOptions::default()
            },
        )
        .expect("remove nonexistent-descendant fixture");
    }

    #[cfg(feature = "fs")]
    #[test]
    fn mv_only_falls_back_to_copy_across_devices() {
        ax_assert!(copy_after_rename_failure(ErrorKind::CrossesDevices));
        ax_assert!(!copy_after_rename_failure(ErrorKind::PermissionDenied));
        ax_assert!(!copy_after_rename_failure(ErrorKind::AlreadyExists));
    }

    #[cfg(feature = "fs")]
    #[test]
    fn mv_renames_file_on_same_filesystem() {
        let root = "/tmp/axvisor-mv-regression";
        reset_test_dir(root);
        let source = format!("{root}/source.txt");
        let destination = format!("{root}/destination.txt");
        fs::write(&source, b"moved payload").expect("create move source");

        move_file_or_dir(&source, &destination).expect("move file");

        ax_assert!(!fs::exists(&source).expect("check move source"));
        ax_assert_eq!(
            fs::read(&destination).expect("read move destination"),
            b"moved payload"
        );
        remove_path(
            root,
            RemoveOptions {
                recursive: true,
                ..RemoveOptions::default()
            },
        )
        .expect("remove move fixture");
    }

    #[cfg(feature = "fs")]
    #[test]
    fn rm_force_only_ignores_not_found() {
        ax_assert!(ignore_remove_error(true, ErrorKind::NotFound));
        ax_assert!(!ignore_remove_error(true, ErrorKind::PermissionDenied));
        ax_assert!(!ignore_remove_error(true, ErrorKind::Unsupported));
        ax_assert!(!ignore_remove_error(false, ErrorKind::NotFound));
    }

    #[cfg(feature = "fs")]
    #[test]
    fn rm_does_not_follow_a_directory_symlink() {
        let metadata = metadata_for_remove("/var/run").expect("inspect rootfs directory symlink");

        ax_assert!(metadata.file_type().is_symlink());
        ax_assert!(!metadata.is_dir());
    }

    #[cfg(feature = "fs")]
    #[test]
    fn ls_propagates_directory_iteration_errors() {
        let entries = [
            Ok(OsString::from("visible")),
            Err(io::Error::from(ErrorKind::PermissionDenied)),
        ];

        let error = collect_directory_entry_names(entries, false)
            .expect_err("directory iteration error must be propagated");

        ax_assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    #[cfg(feature = "fs")]
    #[test]
    fn cp_requires_exactly_two_operands() {
        let source = "source".to_string();
        let destination = "destination".to_string();
        let extra = "extra".to_string();
        ax_assert!(copy_operands(&[]).is_err());
        ax_assert!(copy_operands(core::slice::from_ref(&source)).is_err());
        ax_assert!(copy_operands(&[source.clone(), destination.clone()]).is_ok());
        ax_assert!(copy_operands(&[source, destination, extra]).is_err());
    }

    #[cfg(feature = "fs")]
    fn reset_test_dir(path: &str) {
        let _ = remove_path(
            path,
            RemoveOptions {
                recursive: true,
                force: true,
                ..RemoveOptions::default()
            },
        );
        fs::create_dir(path).expect("create test directory");
    }

    #[cfg(feature = "fs")]
    fn unix_seconds(time: SystemTime) -> u64 {
        time.duration_since(UNIX_EPOCH)
            .expect("test time must not predate Unix epoch")
            .as_secs()
    }
}
