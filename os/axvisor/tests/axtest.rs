#![cfg_attr(target_os = "none", no_std)]
#![no_main]

extern crate alloc;

use ax_hal as _;
use ax_std as _;
use axvm as _;

// Compile the production guest-console mux with narrow host/manager adapters
// so its application-layer state machine is exercised by the kernel harness.
#[path = "../src/network_console/delivery.rs"]
mod browser_console_delivery;
#[path = "../src/network_console/layout.rs"]
mod browser_console_layout;
mod guest_console_harness;
mod manager;
mod network_console;

#[axtest::tests]
mod tests {
    use axtest::prelude::*;
    #[cfg(feature = "fs")]
    use std::{
        fs,
        io::ErrorKind,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    #[cfg(feature = "fs")]
    use axvisor::shell_support::{
        CopyMode, RemoveOptions, copy_path, ensure_recursive_destination_outside_source,
        metadata_for_remove, move_file_or_dir, remove_path, touch_file_at,
    };

    #[test]
    fn guest_output_reaches_only_its_network_console() {
        use crate::{guest_console_harness::mux, network_console};

        network_console::reset();
        network_console::set_guest_connected(1);
        network_console::set_guest_connected(2);
        let backend_1 = mux::serial_backend_factory(1).create();
        let backend_2 = mux::serial_backend_factory(2).create();
        mux::mark_running(1);
        mux::mark_running(2);

        backend_1.write(b"starry output\n");
        backend_2.write(b"zephyr output\n");

        ax_assert_eq!(network_console::take_guest_output(1), b"starry output\n");
        ax_assert_eq!(network_console::take_guest_output(2), b"zephyr output\n");
        ax_assert!(network_console::take_guest_output(3).is_empty());
        mux::remove(1);
        mux::remove(2);
    }

    #[test]
    fn guest_output_skips_network_path_without_a_browser_session() {
        use crate::{guest_console_harness::mux, network_console};

        network_console::reset();
        let backend = mux::serial_backend_factory(1).create();
        mux::mark_running(1);

        backend.write(b"physical console only\n");

        ax_assert!(network_console::take_guest_output(1).is_empty());
        mux::remove(1);
    }

    #[test]
    fn unterminated_guest_echo_reaches_browser_without_another_input() {
        use crate::{guest_console_harness::mux, network_console};

        network_console::reset();
        network_console::set_guest_connected(1);
        let backend = mux::serial_backend_factory(1).create();
        mux::mark_running(1);

        backend.write(b"./run_dual_pick.sh");

        ax_assert_eq!(network_console::take_guest_output(1), b"./run_dual_pick.sh");
        mux::remove(1);
    }

    #[test]
    fn guest_byte_writes_reach_network_output_in_order() {
        use crate::{guest_console_harness::mux, network_console};

        network_console::reset();
        network_console::set_guest_connected(2);
        let backend = mux::serial_backend_factory(2).create();
        mux::mark_running(2);

        for byte in b"zephyr log line\n" {
            backend.write(core::slice::from_ref(byte));
        }

        ax_assert_eq!(network_console::take_guest_output(2), b"zephyr log line\n");
        mux::remove(2);
    }

    #[test]
    fn browser_delivery_coalesces_ordered_dispatcher_batches() {
        use crate::browser_console_delivery::DeliveryFrame;

        let mut delivery = DeliveryFrame::with_capacity(16);

        delivery.append(b"starry ", 0);
        delivery.append(b"continues", 0);

        ax_assert_eq!(delivery.len(), 16);
        ax_assert_eq!(delivery.into_bytes(), b"starry continues");
    }

    #[test]
    fn browser_delivery_reports_source_queue_overflow_before_preserved_bytes() {
        use crate::browser_console_delivery::DeliveryFrame;

        let mut delivery = DeliveryFrame::with_capacity(96);

        delivery.append(b"preserved", 11);

        let output = delivery.into_bytes();
        ax_assert!(
            output.starts_with(b"\r\n[Axvisor browser console dropped 11 queued bytes]\r\n")
        );
        ax_assert!(output.ends_with(b"preserved"));
    }

    #[test]
    fn browser_delivery_queue_preserves_old_output_and_reports_new_overflow() {
        use crate::browser_console_delivery::DeliveryQueue;

        let mut delivery = DeliveryQueue::<8>::new();
        delivery.enqueue(b"old");
        delivery.enqueue(b"overflow");

        let mut output = [0; 8];
        let (len, dropped_bytes) = delivery.dequeue(&mut output);
        ax_assert_eq!(&output[..len], b"old");
        ax_assert_eq!(dropped_bytes, 8);
    }

    #[test]
    fn browser_delivery_waits_for_notification_without_timer_polling() {
        use core::sync::atomic::{AtomicBool, Ordering};
        use std::{sync::Arc, thread, time::Duration};

        use crate::browser_console_delivery::BlockingSignal;

        let signal = Arc::new(BlockingSignal::new());
        signal.notify_irq();
        signal.drain();
        let waiting = Arc::new(AtomicBool::new(false));
        let woke = Arc::new(AtomicBool::new(false));
        let worker_signal = Arc::clone(&signal);
        let worker_waiting = Arc::clone(&waiting);
        let worker_woke = Arc::clone(&woke);
        let worker = thread::spawn(move || {
            worker_waiting.store(true, Ordering::Release);
            worker_signal.wait();
            worker_woke.store(true, Ordering::Release);
        });

        while !waiting.load(Ordering::Acquire) {
            thread::yield_now();
        }
        thread::sleep(Duration::from_millis(30));
        ax_assert!(!woke.load(Ordering::Acquire));

        signal.notify();
        worker
            .join()
            .expect("delivery waiter must exit after notify");
        ax_assert!(woke.load(Ordering::Acquire));
    }

    #[test]
    fn browser_console_layout_uses_at_most_three_sorted_guests() {
        use crate::browser_console_layout::{ConsoleLane, MAX_GUEST_CONSOLES, plan_endpoints};

        let endpoints = plan_endpoints(
            [7, 5, 9, 3]
                .into_iter()
                .map(|vm_id| (vm_id, vm_id.to_string()))
                .collect(),
        );

        ax_assert_eq!(endpoints.len(), MAX_GUEST_CONSOLES + 1);
        ax_assert_eq!(ConsoleLane::COUNT, 4);
        ax_assert_eq!(endpoints[0].route, "axvisor");
        ax_assert_eq!(endpoints[1].vm_id, Some(3));
        ax_assert_eq!(endpoints[2].vm_id, Some(5));
        ax_assert_eq!(endpoints[3].vm_id, Some(7));
        ax_assert_eq!(endpoints[3].lane.index(), 3);
    }

    #[test]
    fn browser_console_layout_uses_configured_names_with_vm_fallback() {
        use crate::browser_console_layout::plan_endpoints;

        let endpoints = plan_endpoints(vec![(2, "zephyr".into()), (1, String::new())]);

        ax_assert_eq!(endpoints[1].display_name, "VM 1");
        ax_assert_eq!(endpoints[2].display_name, "zephyr");
        ax_assert_eq!(endpoints[2].route, "vm-2");
    }

    #[cfg(feature = "fs")]
    #[test]
    fn touch_preserves_content_and_updates_times() {
        let path = "/tmp/axvisor-touch-regression";
        let touch_time = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let _ = fs::remove_file(path);
        fs::write(path, b"preserve me").expect("create touch fixture");

        touch_file_at(path, touch_time).expect("touch fixture");

        let metadata = fs::metadata(path).expect("read touched metadata");
        ax_assert_eq!(fs::read(path).expect("read touched file"), b"preserve me");
        let accessed = unix_seconds(metadata.accessed().expect("read atime"));
        let modified = unix_seconds(metadata.modified().expect("read mtime"));
        ax_assert_eq!(accessed, unix_seconds(touch_time));
        ax_assert_eq!(modified, unix_seconds(touch_time));

        let unsupported_time = UNIX_EPOCH + Duration::from_secs(u32::MAX as u64 + 1);
        let error = touch_file_at(path, unsupported_time)
            .expect_err("timestamps that would be truncated must fail");
        ax_assert_eq!(error.kind(), ErrorKind::InvalidInput);
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
    fn rm_does_not_follow_a_directory_symlink() {
        let metadata = metadata_for_remove("/var/run").expect("inspect rootfs directory symlink");

        ax_assert!(metadata.file_type().is_symlink());
        ax_assert!(!metadata.is_dir());
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
