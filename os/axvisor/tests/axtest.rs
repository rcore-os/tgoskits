//! Kernel harness for the Axvisor in-source axtest suites.
//!
//! The assertions live at the end of the production source files (gated by
//! `#[cfg(any(test, axtest))]`), registered through the `.axtest_array` linker
//! section. This target only wires the bare-metal modules that the binary owns
//! into the test build, together with narrow host/manager/network stubs, and
//! provides the axtest entry point.

#![cfg_attr(target_os = "none", no_std)]
#![no_main]

extern crate alloc;

// The production pool module under test reports through the host log.
#[macro_use]
extern crate log;

use ax_hal as _;
use ax_std as _;
use axvisor as _;
use axvm as _;

// Compile the production guest-console mux with narrow host/manager adapters
// so its application-layer state machine is exercised by the kernel harness.
// These modules stay reachable from the production binary build; the harness
// compiles them only for their in-file axtest suites.
#[allow(dead_code)]
#[path = "../src/control/network_console/delivery.rs"]
mod browser_console_delivery;
#[allow(dead_code)]
#[path = "../src/control/network_console/layout.rs"]
mod browser_console_layout;
#[allow(dead_code)]
#[path = "../src/control/domain/files.rs"]
mod files;
mod guest_console_harness;
#[allow(dead_code)]
#[path = "../src/guest_images.rs"]
mod guest_images;
#[allow(dead_code)]
#[path = "../src/guest_console/terminal.rs"]
mod host_terminal;
mod manager;
mod network_console;
mod control {
    pub(crate) use crate::network_console;
}
#[path = "../src/control/domain/pool.rs"]
mod pool;
#[path = "../src/sync.rs"]
mod sync;
// The pool suites clean their fixtures through the production filesystem
// helpers, so the harness compiles that module for its in-file suite too.
#[allow(dead_code)]
#[path = "../src/shell_fs.rs"]
mod shell_fs;

// These cases exercise the mux-to-network boundary through the stub above and
// therefore must live beside the harness assembly instead of `mux/tests.rs`
// (the binary is also compiled with `--cfg axtest` and has no network console).
fn prepare_filesystem() {
    for path in ["/tmp", "/var", "/run"] {
        std::fs::create_dir_all(path).expect("prepare memory-root test directory");
    }
    ax_fs_ng::current_fs_context()
        .lock()
        .symlink(
            "/run",
            "/var/run",
            0,
            0,
            &axfs_ng_vfs::MutationCredentials::root(),
        )
        .expect("prepare directory symlink fixture");
}

#[axtest::tests(setup = prepare_filesystem)]
mod tests {
    use ax_fs_ng::vfs::FsContext;
    use axfs_ng_vfs::{Mountpoint, MutationCredentials, NodePermission};
    use axtest::prelude::*;
    use axvisor::builtin::{install_builtin, selected_configs};

    #[test]
    fn placed_file_can_be_reopened_idempotently() {
        let directory = "/tmp/file-transfer-reopen";
        let id = "placed-reopen";
        let _ = std::fs::remove_dir_all(directory);
        std::fs::create_dir_all(directory).unwrap();

        crate::files::open(id, directory, 4).unwrap();
        match crate::files::send(id, 0, Some(5), b"data") {
            Err(crate::files::FileError::Conflict { offset, .. }) => ax_assert_eq!(offset, 0),
            _ => panic!("a range with a different declared total was accepted"),
        }
        crate::files::send(id, 0, None, b"data").unwrap();
        let placed = crate::files::place(id, "kernel").unwrap();
        ax_assert_eq!(
            placed.path.as_deref(),
            Some("/tmp/file-transfer-reopen/kernel")
        );

        let reopened = crate::files::open(id, directory, 4).unwrap();
        ax_assert_eq!(reopened.state, "placed");
        ax_assert_eq!(reopened.written, 4);
        ax_assert_eq!(reopened.path.as_deref(), placed.path.as_deref());
        ax_assert_eq!(
            std::fs::read_to_string(placed.path.unwrap()).unwrap(),
            "data"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn oversized_staging_file_cannot_be_marked_uploaded() {
        let directory = "/tmp/file-transfer-oversized";
        let id = "oversized-staging";
        let _ = std::fs::remove_dir_all(directory);
        std::fs::create_dir_all(format!("{directory}/.files")).unwrap();
        std::fs::write(format!("{directory}/.files/{id}"), b"12345").unwrap();

        match crate::files::open(id, directory, 4) {
            Err(crate::files::FileError::Conflict { offset, .. }) => ax_assert_eq!(offset, 5),
            _ => panic!("a staging file larger than its declaration was accepted"),
        }
        crate::files::drop(id).unwrap();
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn diskless_boot_keeps_memory_root_with_inherited_root_parameter() {
        // The shared QEMU axtest image exposes an NVMe device. This scenario
        // is meaningful only on the diskless target where the runtime has no
        // block device to prepare, so leave it for that target's harness.
        if ax_fs_ng::block::runtime::BlockRuntime::installed_devices()
            .is_some_and(|devices| !devices.is_empty())
        {
            return axtest::AxTestResult::Ok;
        }
        // The bundled kernel exists, but the board DTB is outside the archive.
        // Its absence must affect that VM at load time, not host preparation.
        std::fs::create_dir_all("/guest/builtin/configs").unwrap();
        std::fs::create_dir_all("/guest/builtin/images").unwrap();
        std::fs::write("/guest/builtin/images/kernel", "kernel").unwrap();
        std::fs::write(
            "/guest/builtin/configs/default.toml",
            "[base]\nid=1\nname='diskless'\ncpu_num=1\n[kernel]\nentry_point=0\nkernel_load_addr=0\nkernel_path='/guest/builtin/images/kernel'\ndtb_path='/board/missing.dtb'\n[devices]\n",
        )
        .unwrap();
        axvisor::builtin::prepare_root().expect("external assets must not abort diskless boot");
        // Package-owned resources must still be present and nonempty.
        std::fs::write("/guest/builtin/images/kernel", "").unwrap();
        ax_assert!(axvisor::builtin::prepare_root().is_err());
        std::fs::remove_dir_all("/guest/builtin").unwrap();
        ax_assert_eq!(
            ax_fs_ng::root::root_kind(),
            Some(ax_fs_ng::root::RootKind::Memory)
        );
    }

    #[test]
    fn user_configs_override_defaults_and_invalid_user_configs_stop_loading() {
        let context =
            FsContext::new(Mountpoint::new_root(&ax_fs_ng::MemoryFs::new()).root_location());
        for path in [
            "/guest",
            "/guest/builtin",
            "/guest/builtin/configs",
            "/guest/vm_default",
        ] {
            context
                .create_dir(
                    path,
                    NodePermission::from_bits_truncate(0o755),
                    0,
                    0,
                    &MutationCredentials::root(),
                )
                .unwrap();
        }
        let builtin = "[base]\nid=1\nname='builtin'\ncpu_num=1\n[kernel]\nentry_point=0\nkernel_load_addr=0\nkernel_path='/guest/builtin/images/kernel'\n[devices]\n";
        let user = builtin.replace("builtin'", "user'").replace("id=1", "id=2");
        context
            .write("/guest/builtin/configs/default.toml", builtin)
            .unwrap();
        ax_assert_eq!(
            selected_configs(&context).unwrap(),
            alloc::vec![builtin.to_owned()]
        );
        context
            .write("/guest/vm_default/custom.toml", &user)
            .unwrap();
        ax_assert_eq!(selected_configs(&context).unwrap(), alloc::vec![user]);
        context.write("/guest/vm_default/custom.toml", "").unwrap();
        ax_assert!(selected_configs(&context).is_err());
        context
            .remove_file(
                "/guest/vm_default/custom.toml",
                &MutationCredentials::root(),
            )
            .unwrap();
        ax_assert_eq!(
            selected_configs(&context).unwrap(),
            alloc::vec![builtin.to_owned()]
        );
    }

    #[test]
    fn package_install_validates_external_assets_before_replacing_defaults() {
        let source =
            FsContext::new(Mountpoint::new_root(&ax_fs_ng::MemoryFs::new()).root_location());
        let target =
            FsContext::new(Mountpoint::new_root(&ax_fs_ng::MemoryFs::new()).root_location());
        for context in [&source, &target] {
            for path in [
                "/guest",
                "/guest/builtin",
                "/guest/builtin/configs",
                "/guest/builtin/images",
                "/board",
            ] {
                context
                    .create_dir(
                        path,
                        NodePermission::from_bits_truncate(0o755),
                        0,
                        0,
                        &MutationCredentials::root(),
                    )
                    .unwrap();
            }
        }
        let installed = "[base]\nid=1\nname='installed'\ncpu_num=1\n[kernel]\nentry_point=0\nkernel_load_addr=0\nkernel_path='/guest/builtin/images/kernel'\n[devices]\n";
        let incoming = installed
            .replace("installed'", "incoming'")
            .replace("[devices]", "dtb_path='/board/guest.dtb'\n[devices]");
        source
            .write("/guest/builtin/configs/default.toml", &incoming)
            .unwrap();
        source
            .write("/guest/builtin/images/kernel", "new kernel")
            .unwrap();
        target
            .write("/guest/builtin/configs/default.toml", installed)
            .unwrap();
        target
            .write("/guest/builtin/images/kernel", "old kernel")
            .unwrap();
        target
            .write("/guest/builtin/images/obsolete", "old resource")
            .unwrap();
        let install = || install_builtin(&source, &target);
        for empty_file in [false, true] {
            if empty_file {
                target.write("/board/guest.dtb", "").unwrap();
            }
            let error = install().unwrap_err();
            ax_assert!(alloc::format!("{error:#}").contains("/board/guest.dtb"));
            ax_assert_eq!(
                target
                    .read_to_string("/guest/builtin/configs/default.toml")
                    .unwrap(),
                installed
            );
            ax_assert_eq!(
                target
                    .read_to_string("/guest/builtin/images/kernel")
                    .unwrap(),
                "old kernel"
            );
        }
        target.write("/board/guest.dtb", "guest DTB").unwrap();
        ax_assert!(install().unwrap());
        ax_assert_eq!(
            target
                .read_to_string("/guest/builtin/configs/default.toml")
                .unwrap(),
            incoming
        );
        ax_assert_eq!(
            target
                .read_to_string("/guest/builtin/images/kernel")
                .unwrap(),
            "new kernel"
        );
        ax_assert!(target.resolve("/guest/builtin/images/obsolete").is_err());
    }

    use crate::shell_fs::{RemoveOptions, remove_path};
    use ax_std::fs;

    fn remove_guest_console(vm_id: usize) {
        use crate::guest_console_harness::mux;

        let identity = mux::backend_identity(vm_id).expect("guest backend must be registered");
        assert!(mux::remove_if_backend(identity));
    }

    #[test]
    fn guest_output_reaches_only_its_network_console() {
        use crate::{guest_console_harness::mux, network_console};

        network_console::reset();
        let backend_1 = mux::serial_backend_factory(1).create();
        let backend_2 = mux::serial_backend_factory(2).create();
        mux::mark_running(1);
        mux::mark_running(2);

        backend_1.write(b"starry output\n");
        backend_2.write(b"zephyr output\n");

        ax_assert_eq!(network_console::take_guest_output(1), b"starry output\n");
        ax_assert_eq!(network_console::take_guest_output(2), b"zephyr output\n");
        ax_assert!(network_console::take_guest_output(3).is_empty());
        remove_guest_console(1);
        remove_guest_console(2);
    }

    #[test]
    fn guest_output_is_retained_without_a_browser_session() {
        use crate::{guest_console_harness::mux, network_console};

        network_console::reset();
        let backend = mux::serial_backend_factory(1).create();
        mux::mark_running(1);

        backend.write(b"physical console only\n");

        ax_assert_eq!(
            network_console::take_guest_output(1),
            b"physical console only\n"
        );
        remove_guest_console(1);
    }

    #[test]
    fn unterminated_guest_echo_reaches_browser_without_another_input() {
        use crate::{guest_console_harness::mux, network_console};

        network_console::reset();
        let backend = mux::serial_backend_factory(1).create();
        mux::mark_running(1);

        backend.write(b"./run_dual_pick.sh");

        ax_assert_eq!(network_console::take_guest_output(1), b"./run_dual_pick.sh");
        remove_guest_console(1);
    }

    #[test]
    fn guest_byte_writes_reach_network_output_in_order() {
        use crate::{guest_console_harness::mux, network_console};

        network_console::reset();
        let backend = mux::serial_backend_factory(2).create();
        mux::mark_running(2);

        for byte in b"zephyr log line\n" {
            backend.write(core::slice::from_ref(byte));
        }

        ax_assert_eq!(network_console::take_guest_output(2), b"zephyr log line\n");
        remove_guest_console(2);
    }

    #[test]
    fn blocked_physical_output_keeps_guest_bytes_out_of_network_delivery() {
        use crate::{guest_console_harness, network_console};

        network_console::reset();
        let backend = guest_console_harness::mux::serial_backend_factory(1).create();
        guest_console_harness::mux::mark_running(1);

        guest_console_harness::host::set_output_blocked(true);
        let accepted = backend.try_write(b"retained by uart");
        guest_console_harness::host::set_output_blocked(false);

        ax_assert_eq!(accepted, 0);
        ax_assert!(network_console::take_guest_output(1).is_empty());
        remove_guest_console(1);
    }

    #[test]
    fn ordered_queue_pop_notifies_the_blocked_vm_through_the_mux() {
        use crate::{
            guest_console_harness::{host, mux},
            manager, network_console,
        };

        network_console::reset();
        host::reset_output();
        manager::take_notified_vms();
        host::set_ordered_output_available(true);

        // VM[2]'s record owns the only ordered slot, so the pop that releases
        // capacity belongs to a VM other than the one left with retained bytes.
        let backend_2 = mux::serial_backend_factory(2).create();
        mux::mark_running(2);
        ax_assert_eq!(backend_2.try_write(b"vm2\n"), 4);

        let backend_1 = mux::serial_backend_factory(1).create();
        mux::mark_running(1);
        ax_assert_eq!(backend_1.try_write(b"retained by uart"), 0);
        ax_assert!(network_console::take_guest_output(1).is_empty());
        // Backpressure alone must not wake anything; the wake belongs to the pop.
        ax_assert!(manager::take_notified_vms().is_empty());

        let popped = host::pop_ordered_record().expect("queued record is retained");
        ax_assert_eq!((popped >> 64) as usize, 2);

        // Driving the production pop handler must wake the blocked VM even
        // though the popped record belongs to another VM.
        mux::replay_guest_output(popped, b"vm2\n");
        ax_assert_eq!(manager::take_notified_vms(), alloc::vec![1]);

        // The retry now fits the released slot and reaches the network console.
        ax_assert_eq!(backend_1.try_write(b"retained by uart"), 16);
        ax_assert_eq!(network_console::take_guest_output(1), b"retained by uart");

        remove_guest_console(2);
        remove_guest_console(1);
        host::reset_output();
    }

    #[test]
    fn host_log_pop_notifies_the_blocked_vm_through_the_mux() {
        use crate::{
            guest_console_harness::{host, mux},
            manager,
        };

        host::reset_output();
        manager::take_notified_vms();
        host::set_ordered_output_available(true);

        // An untagged host log record owns the only ordered slot, so the next
        // guest submission is rejected with backpressure.
        ax_assert!(host::queue_host_log_record(b"host log\n"));

        let blocked = mux::serial_backend_factory(1).create();
        mux::mark_running(1);
        ax_assert_eq!(blocked.try_write(b"retained"), 0);
        // Backpressure alone must not wake anything.
        ax_assert!(manager::take_notified_vms().is_empty());

        // Consuming the host record releases the slot; the queue stub itself
        // must not be the thing that wakes the VM.
        let record = host::pop_ordered_host_record().expect("queued host log is retained");
        ax_assert!(manager::take_notified_vms().is_empty());

        // Only routing the popped bytes through the production host-log handler
        // publishes the device-poll request for the blocked VM.
        let _output = mux::route_host_log(&record, 0, 0);
        ax_assert_eq!(manager::take_notified_vms(), alloc::vec![1]);

        remove_guest_console(1);
        host::reset_output();
    }

    #[test]
    fn accepted_console_tail_is_displayed_before_stopping_guest_detaches() {
        use crate::{
            guest_console_harness::{host, mux},
            manager,
        };
        use axvm::VmStatus;

        host::reset_output();
        host::set_ordered_output_available(true);
        manager::set_vm_status(1, Some(VmStatus::Running));
        manager::take_notified_vms();
        let backend = mux::serial_backend_factory(1).create();
        mux::mark_running(1);
        ax_assert!(mux::attach(1).is_ok());
        mux::activate(1);

        let prefix = b"ivc ack seq=5 msg=ack from linux subscribe";
        ax_assert!(host::queue_host_log_record(b""));
        ax_assert_eq!(backend.try_write(prefix), 0);
        let filler = host::pop_ordered_host_record().expect("filler must be queued");
        let _ = mux::route_host_log(&filler, 0, 0);
        ax_assert_eq!(manager::take_notified_vms(), alloc::vec![1]);
        ax_assert_eq!(backend.try_write(prefix), prefix.len());
        let tag = host::pop_ordered_record().expect("accepted prefix must be queued");
        mux::replay_guest_output(tag, prefix);
        ax_assert_eq!(backend.try_write(b"r\n"), 2);

        manager::set_vm_status(1, Some(VmStatus::Stopping));
        ax_assert_eq!(mux::reconcile_vm_states(), None);

        let tag = host::pop_ordered_record().expect("accepted tail must be queued");
        mux::replay_guest_output(tag, b"r\n");
        ax_assert_eq!(
            host::take_host_bytes(),
            b"ivc ack seq=5 msg=ack from linux subscriber\n"
        );

        manager::set_vm_status(1, Some(VmStatus::Stopped));
        ax_assert_eq!(mux::reconcile_vm_states(), Some(1));
        manager::set_vm_status(1, None);
        remove_guest_console(1);
        manager::take_notified_vms();
        host::reset_output();
    }

    #[test]
    fn pop_notifies_only_blocked_backends_that_are_still_current() {
        use crate::{
            guest_console_harness::{host, mux},
            manager,
        };

        host::reset_output();
        manager::take_notified_vms();
        host::set_ordered_output_available(true);

        // Occupy the only ordered slot so later submissions report backpressure.
        let filler = mux::serial_backend_factory(7).create();
        mux::mark_running(7);
        ax_assert_eq!(filler.try_write(b"filler"), 6);

        let stopped = mux::serial_backend_factory(3).create();
        mux::mark_running(3);
        ax_assert_eq!(stopped.try_write(b"stopped"), 0);
        mux::mark_stopped(3);

        let replaced = mux::serial_backend_factory(4).create();
        mux::mark_running(4);
        ax_assert_eq!(replaced.try_write(b"replaced"), 0);
        let _replacement = mux::serial_backend_factory(4).create();
        mux::mark_running(4);

        let live = mux::serial_backend_factory(5).create();
        mux::mark_running(5);
        ax_assert_eq!(live.try_write(b"live"), 0);
        ax_assert!(manager::take_notified_vms().is_empty());

        let popped = host::pop_ordered_record().expect("queued record is retained");
        mux::replay_guest_output(popped, b"filler");

        // Only the still-live blocked VM may be woken; the stopped incarnation
        // and the replaced generation must not leak into the pop notification.
        ax_assert_eq!(manager::take_notified_vms(), alloc::vec![5]);

        remove_guest_console(7);
        remove_guest_console(3);
        remove_guest_console(4);
        remove_guest_console(5);
        host::reset_output();
    }

    #[test]
    fn browser_delivery_coalesces_ordered_dispatcher_batches() {
        use crate::browser_console_delivery::DeliveryFrame;

        let mut delivery = DeliveryFrame::with_capacity(16);

        delivery.append(b"starry ", 0);
        delivery.append(b"continues", 0);

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

        use {
            ax_std::os::arceos::modules::ax_runtime::task::sync::irq::IrqWaitCell,
            ax_std::os::arceos::modules::ax_runtime::task::sync::irq::IrqWorkerWaiter,
            ax_std::os::arceos::modules::ax_runtime::task::thread::current::current_thread_handle,
        };

        let signal = Arc::new(IrqWaitCell::new());
        let waiting = Arc::new(AtomicBool::new(false));
        let woke = Arc::new(AtomicBool::new(false));
        let worker_signal = Arc::clone(&signal);
        let worker_waiting = Arc::clone(&waiting);
        let worker_woke = Arc::clone(&woke);
        let worker = thread::spawn(move || {
            let current =
                current_thread_handle().expect("delivery waiter must bind to its runtime worker");
            let waiter = IrqWorkerWaiter::new(current.wake_handle());
            worker_waiting.store(true, Ordering::Release);
            waiter
                .wait(&worker_signal)
                .expect("delivery waiter must accept one notification cell");
            worker_woke.store(true, Ordering::Release);
        });

        while !waiting.load(Ordering::Acquire) {
            thread::yield_now();
        }
        thread::sleep(Duration::from_millis(30));
        ax_assert!(!woke.load(Ordering::Acquire));

        let _result = signal.notify();
        worker
            .join()
            .expect("delivery waiter must exit after notify");
        ax_assert!(woke.load(Ordering::Acquire));
    }

    #[test]
    fn host_terminal_converts_only_bare_lf_across_batches() {
        use crate::host_terminal::TerminalNewlineNormalizer;

        let mut normalizer = TerminalNewlineNormalizer::new();
        let mut output = Vec::new();
        normalizer
            .write(b"banner\nline\r", |bytes| {
                output.extend_from_slice(bytes);
                Ok::<_, ()>(())
            })
            .unwrap();
        normalizer
            .write(b"\nnext\n", |bytes| {
                output.extend_from_slice(bytes);
                Ok::<_, ()>(())
            })
            .unwrap();

        ax_assert_eq!(output, b"banner\r\nline\r\nnext\r\n");
    }

    #[test]
    fn console_layout_allocates_guest_lanes_in_order_and_frees_them() {
        use crate::browser_console_layout::{LaneAllocation, Layout, MAX_GUEST_CONSOLES};

        let mut layout = Layout::new();
        // The outcome tells the caller whether it may give a lane back later: a
        // lane reported as reused belongs to a VM that already existed, so a
        // creation that is rejected afterwards must not release it.
        ax_assert_eq!(
            layout
                .allocate(2, "zephyr")
                .expect("a free lane must accept a guest"),
            LaneAllocation::Allocated,
        );
        layout
            .allocate(1, "")
            .expect("a free lane must accept a guest");

        let endpoints = layout.endpoints();
        ax_assert_eq!(endpoints.len(), 3);
        ax_assert_eq!(endpoints[0].route, "axvisor");
        ax_assert_eq!(endpoints[0].vm_id, None);
        ax_assert_eq!(endpoints[1].vm_id, Some(2));
        ax_assert_eq!(endpoints[1].display_name, "zephyr");
        ax_assert_eq!(endpoints[1].lane.index(), 1);
        ax_assert_eq!(endpoints[2].vm_id, Some(1));
        ax_assert_eq!(endpoints[2].display_name, "VM 1");
        ax_assert_eq!(endpoints[2].route, "vm-1");

        // Re-registering a VM keeps its lane, and only a free slot is reused.
        ax_assert_eq!(
            layout
                .allocate(2, "zephyr")
                .expect("re-registering a VM is idempotent"),
            LaneAllocation::Reused,
        );
        ax_assert_eq!(layout.endpoints().len(), 3);
        ax_assert_eq!(layout.guest(2).map(|guest| guest.lane.index()), Some(1));
        ax_assert_eq!(layout.release(2).map(|guest| guest.lane.index()), Some(1));
        ax_assert_eq!(
            layout
                .allocate(3, "linux")
                .expect("the freed lane must be reusable"),
            LaneAllocation::Allocated,
        );
        ax_assert_eq!(layout.guest(3).map(|guest| guest.lane.index()), Some(1));
        ax_assert_eq!(layout.release(3).map(|guest| guest.lane.index()), Some(1));
        ax_assert_eq!(layout.release(3).map(|guest| guest.lane.index()), None);

        // A full table rejects the next guest instead of dropping an existing
        // one, and does not disturb the lanes already handed out.
        for vm_id in 0..MAX_GUEST_CONSOLES {
            layout
                .allocate(vm_id, "guest")
                .expect("guest within the lane limit must be accepted");
        }
        ax_assert!(layout.allocate(MAX_GUEST_CONSOLES, "guest").is_err());
        ax_assert_eq!(layout.endpoints().len(), MAX_GUEST_CONSOLES + 1);
        ax_assert_eq!(layout.guest(0).map(|guest| guest.lane.index()), Some(1));
        ax_assert_eq!(
            layout
                .guest(MAX_GUEST_CONSOLES - 1)
                .map(|guest| guest.lane.index()),
            Some(MAX_GUEST_CONSOLES)
        );
    }

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

    #[test]
    fn vm_pool_scan_lists_only_configs_that_can_become_a_vm() {
        use crate::pool::scan_dir;

        let root = "/tmp/axvisor-vm-pool-scan";
        reset_test_dir(root);
        let kernel = format!("{root}/kernel.bin");
        fs::write(&kernel, b"guest kernel").expect("write kernel image fixture");

        let entry_toml = |id: usize, name: &str| {
            format!(
                "[base]\nid = {id}\nname = \"{name}\"\n\n[kernel]\nkernel_path = \"{kernel}\"\n"
            )
        };
        fs::write(&format!("{root}/named.toml"), entry_toml(7, "named"))
            .expect("write filesystem entry");
        // Only the `.toml` suffix makes a file a pool candidate.
        fs::write(&format!("{root}/notes.txt"), b"[base]\nid = 9\n").expect("write note");
        fs::write(&format!("{root}/empty.toml"), b"").expect("write empty file");
        fs::write(&format!("{root}/broken.toml"), b"base = { id = 1,").expect("write broken file");
        fs::write(&format!("{root}/binary.toml"), [0xff, 0xfe, 0xfd]).expect("write binary file");
        let absent = format!("{root}/absent.bin");
        fs::write(
            &format!("{root}/missing.toml"),
            format!("[base]\nid = 8\nname = \"missing\"\n\n[kernel]\nkernel_path = \"{absent}\"\n"),
        )
        .expect("write missing-image entry");

        let pool = scan_dir(root);

        ax_assert_eq!(pool.directory(), root);
        let mut ids: alloc::vec::Vec<usize> =
            pool.entries().iter().map(|entry| entry.id()).collect();
        ids.sort();
        ax_assert_eq!(ids, [7]);
        let named = pool
            .entries()
            .iter()
            .find(|entry| entry.id() == 7)
            .expect("the filesystem entry must be listed");
        ax_assert!(named.path().ends_with("named.toml"));
        ax_assert!(named.toml().contains("id = 7"));

        let mut reported: alloc::vec::Vec<_> = pool
            .issues()
            .iter()
            .map(|issue| format!("{} {}", issue.kind().as_str(), issue.path()))
            .collect();
        reported.sort();
        let mut expected = [
            format!("empty {root}/empty.toml"),
            format!("invalid-toml {root}/broken.toml"),
            format!("missing-image {root}/missing.toml"),
            format!("unreadable {root}/binary.toml"),
        ];
        expected.sort();
        ax_assert_eq!(reported, expected);
        // The reported reason names the file that is missing, not just the
        // config that asked for it.
        let missing = pool
            .issues()
            .iter()
            .find(|issue| issue.path().ends_with("missing.toml"))
            .expect("the missing image must be reported");
        ax_assert!(format!("{missing}").contains(absent.as_str()));

        // Two configs claiming one id: the first one listed wins and the other
        // is reported, whichever the filesystem enumerates first.
        let dupes = format!("{root}/dupes");
        fs::create_dir(&dupes).expect("create duplicate fixture directory");
        fs::write(&format!("{dupes}/a.toml"), entry_toml(5, "a")).expect("write a.toml");
        fs::write(&format!("{dupes}/b.toml"), entry_toml(5, "b")).expect("write b.toml");
        let dupe_pool = scan_dir(&dupes);
        ax_assert_eq!(dupe_pool.entries().len(), 1);
        ax_assert_eq!(dupe_pool.issues().len(), 1);
        ax_assert_eq!(dupe_pool.issues()[0].kind().as_str(), "duplicate-id");
        ax_assert_eq!(dupe_pool.entries()[0].id(), 5);
        ax_assert!(dupe_pool.entries()[0].path() != dupe_pool.issues()[0].path());

        // A directory that is not there yields no entries and one reason.
        let absent_dir = "/tmp/axvisor-vm-pool-absent";
        let _ = remove_path(
            absent_dir,
            RemoveOptions {
                recursive: true,
                force: true,
                ..RemoveOptions::default()
            },
        );
        let absent_pool = scan_dir(absent_dir);
        ax_assert_eq!(absent_pool.entries().len(), 0);
        ax_assert_eq!(absent_pool.issues().len(), 1);
        ax_assert_eq!(
            absent_pool.issues()[0].kind().as_str(),
            "directory-unavailable"
        );

        remove_path(
            root,
            RemoveOptions {
                recursive: true,
                ..RemoveOptions::default()
            },
        )
        .expect("remove pool fixture");
    }

    #[test]
    fn vm_pool_scan_walks_subdirectories_and_ignores_documents_that_are_no_config() {
        use crate::pool::{MAX_SCAN_DEPTH, browse, scan_dir, scan_dirs, sources};

        let root = "/tmp/axvisor-vm-pool-walk";
        reset_test_dir(root);
        let kernel = format!("{root}/kernel.bin");
        fs::write(&kernel, b"guest kernel").expect("write kernel image fixture");
        let entry_toml = |id: usize, name: &str| {
            format!(
                "[base]\nid = {id}\nname = \"{name}\"\n\n[kernel]\nkernel_path = \"{kernel}\"\n"
            )
        };

        // The guest filesystem cannot create directories recursively, so each
        // level of a fixture tree is created on its own.
        let nested = format!("{root}/a/b");
        fs::create_dir(&format!("{root}/a")).expect("create fixture directory a");
        fs::create_dir(&nested).expect("create fixture directory b");
        fs::write(&format!("{nested}/deep.toml"), entry_toml(11, "deep"))
            .expect("write nested entry");
        // A `.toml` that is no guest config. The scan walks a whole filesystem
        // and meets other tools' documents there; reporting them as broken
        // configs would bury the files that really are broken.
        fs::write(
            &format!("{nested}/manifest.toml"),
            b"[workspace]\nmembers = [\"crates/a\"]\n",
        )
        .expect("write unrelated document");
        // A config attempt that does not parse is reported wherever it sits:
        // that is the difference the unrelated document must not blur.
        fs::write(&format!("{nested}/damaged.toml"), b"[base]\nid = 12,\n")
            .expect("write damaged entry");
        let staging = format!("{root}/.files");
        fs::create_dir(&staging).expect("create private staging directory");
        fs::write(&format!("{staging}/staged.toml"), entry_toml(14, "staged"))
            .expect("write staged config fixture");

        let pool = scan_dir(root);
        let ids: alloc::vec::Vec<usize> = pool.entries().iter().map(|entry| entry.id()).collect();
        ax_assert_eq!(ids, [11]);
        // `Entry.source` is the folder the file is in, not the folder the scan
        // started from, so a nested config is traceable to where it lives.
        ax_assert_eq!(pool.entries()[0].source(), nested.as_str());
        let reported: alloc::vec::Vec<_> = pool
            .issues()
            .iter()
            .map(|issue| format!("{} {}", issue.kind().as_str(), issue.path()))
            .collect();
        ax_assert_eq!(reported, [format!("invalid-toml {nested}/damaged.toml")]);
        ax_assert!(
            pool.entries()
                .iter()
                .all(|entry| !entry.path().contains("/.files/"))
        );
        let hidden = browse(&staging);
        ax_assert!(hidden.files().is_empty());
        ax_assert_eq!(hidden.entries().len(), 0);
        ax_assert_eq!(hidden.issues().len(), 1);

        // The guest tree is a source as well, read last so the narrower ones
        // keep precedence; a file reached through two sources is one candidate
        // rather than a duplicate of itself.
        ax_assert_eq!(sources().last(), Some(&"/guest".to_string()));
        let overlap = scan_dirs(&[nested.clone(), root.to_string()]);
        let ids: alloc::vec::Vec<usize> =
            overlap.entries().iter().map(|entry| entry.id()).collect();
        ax_assert_eq!(ids, [11]);
        ax_assert!(
            overlap
                .issues()
                .iter()
                .all(|issue| issue.kind().as_str() != "duplicate-id")
        );

        // The walk stops at the depth cap instead of following a pathological
        // tree for as long as it takes to read it.
        let mut too_deep = root.to_string();
        for level in 0..=MAX_SCAN_DEPTH {
            too_deep = format!("{too_deep}/level{level}");
            fs::create_dir(&too_deep).expect("create deep fixture directory");
        }
        fs::write(&format!("{too_deep}/buried.toml"), entry_toml(13, "buried"))
            .expect("write buried entry");
        let capped = scan_dir(root);
        let ids: alloc::vec::Vec<usize> = capped.entries().iter().map(|entry| entry.id()).collect();
        ax_assert_eq!(ids, [11]);

        remove_path(
            root,
            RemoveOptions {
                recursive: true,
                ..RemoveOptions::default()
            },
        )
        .expect("remove walk fixture");
    }

    #[test]
    fn vm_pool_reads_several_directories_in_precedence_order() {
        use crate::pool::{browse, scan_dirs, sources};

        // The directory a new config is written to comes first, so a config the
        // operator saves there shadows a same-id config found elsewhere.
        ax_assert_eq!(sources().first(), Some(&"/guest".to_string()));

        let root = "/tmp/axvisor-vm-pool-multi";
        reset_test_dir(root);
        let kernel = format!("{root}/kernel.bin");
        fs::write(&kernel, b"guest kernel").expect("write kernel image fixture");
        let entry_toml = |id: usize, name: &str| {
            format!(
                "[base]\nid = {id}\nname = \"{name}\"\n\n[kernel]\nkernel_path = \"{kernel}\"\n"
            )
        };

        let first = format!("{root}/first");
        let second = format!("{root}/second");
        fs::create_dir(&first).expect("create first fixture directory");
        fs::create_dir(&second).expect("create second fixture directory");
        fs::write(&format!("{first}/only.toml"), entry_toml(1, "only-first"))
            .expect("write first-only entry");
        fs::write(
            &format!("{second}/other.toml"),
            entry_toml(2, "only-second"),
        )
        .expect("write second-only entry");
        fs::write(&format!("{first}/shadow.toml"), entry_toml(3, "from-first"))
            .expect("write shadowing entry");
        fs::write(
            &format!("{second}/shadow.toml"),
            entry_toml(3, "from-second"),
        )
        .expect("write shadowed entry");

        let pool = scan_dirs(&[first.clone(), second.clone()]);

        ax_assert_eq!(pool.directory(), first.as_str());
        ax_assert_eq!(pool.sources(), [first.clone(), second.clone()].as_slice());
        let mut names: alloc::vec::Vec<&str> =
            pool.entries().iter().map(|entry| entry.name()).collect();
        names.sort();
        ax_assert_eq!(names, ["from-first", "only-first", "only-second"]);
        // Every entry says which folder it came from, which is what makes a
        // duplicate id traceable to two files.
        let from_second = pool
            .entries()
            .iter()
            .find(|entry| entry.name() == "only-second")
            .expect("the second directory must contribute entries");
        ax_assert_eq!(from_second.source(), second.as_str());
        let shadow = pool
            .issues()
            .iter()
            .find(|issue| issue.path().ends_with("second/shadow.toml"))
            .expect("the shadowed file must be reported");
        ax_assert_eq!(shadow.kind().as_str(), "duplicate-id");

        // Browsing walks one directory: folders are listed as folders to enter
        // rather than as unreadable files, and only `.toml` files are entries.
        fs::write(&format!("{first}/notes.txt"), b"not a config").expect("write note");
        let folder = browse(&first);
        ax_assert_eq!(folder.path(), first.as_str());
        ax_assert_eq!(folder.parent(), Some(root));
        ax_assert!(folder.directories().is_empty());
        ax_assert_eq!(folder.entries().len(), 2);
        ax_assert!(folder.issues().is_empty());

        let nested = browse(root);
        let mut subdirectories: alloc::vec::Vec<&str> = nested
            .directories()
            .iter()
            .map(|directory| directory.name())
            .collect();
        subdirectories.sort();
        ax_assert_eq!(subdirectories, ["first", "second"]);
        // `kernel.bin` is neither a directory nor a `.toml`, so it is not
        // reported as a problem: browsing must not turn a normal file into noise.
        ax_assert!(nested.issues().is_empty());
        // It is still a file, though: a folder view lists what is there, and the
        // length is what the filesystem knows about it.
        let listed: alloc::vec::Vec<(&str, usize)> = nested
            .files()
            .iter()
            .map(|file| (file.name(), file.size()))
            .collect();
        ax_assert_eq!(listed, [("kernel.bin", 12)]);

        remove_path(
            root,
            RemoveOptions {
                recursive: true,
                ..RemoveOptions::default()
            },
        )
        .expect("remove multi-directory fixture");
    }

    #[test]
    fn vm_pool_save_only_writes_validated_configs_inside_the_directory() {
        use crate::pool::{SaveError, save_in, scan_dir};

        let root = "/tmp/axvisor-vm-pool-save";
        reset_test_dir(root);
        let kernel = format!("{root}/kernel.bin");
        fs::write(&kernel, b"guest kernel").expect("write kernel image fixture");
        let valid =
            format!("[base]\nid = 4\nname = \"saved\"\n\n[kernel]\nkernel_path = \"{kernel}\"\n");

        // A name that could escape the directory is refused before any write,
        // so a request cannot place a file anywhere it likes.
        for name in [
            "",
            "  ",
            "guest",
            "guest.toml.bak",
            "../escape.toml",
            ".hidden.toml",
        ] {
            ax_assert!(matches!(
                save_in(root, name, &valid),
                Err(SaveError::InvalidName(_))
            ));
        }
        ax_assert!(fs::metadata(&format!("{root}/../escape.toml")).is_err());

        // Text that is not a guest config is refused too: the pool only ever
        // holds files that can become a VM.
        ax_assert!(matches!(
            save_in(root, "broken.toml", "base = { id = 1,"),
            Err(SaveError::InvalidToml(_))
        ));
        ax_assert!(fs::metadata(&format!("{root}/broken.toml")).is_err());

        // The happy path is observable: the file lands where it was asked to
        // and the next scan lists it as a candidate.
        let path = save_in(root, "saved.toml", &valid).expect("save a valid config");
        ax_assert_eq!(path, format!("{root}/saved.toml"));
        ax_assert_eq!(
            fs::read_to_string(&path).expect("read the saved config"),
            valid
        );
        let pool = scan_dir(root);
        ax_assert_eq!(pool.entries().len(), 1);
        ax_assert_eq!(pool.entries()[0].id(), 4);
        ax_assert_eq!(pool.entries()[0].name(), "saved");

        // A save never replaces an existing candidate behind an operator's
        // back; choose a new name or remove the old file explicitly.
        ax_assert!(matches!(
            save_in(root, "saved.toml", &valid),
            Err(SaveError::Exists(_))
        ));

        remove_path(
            root,
            RemoveOptions {
                recursive: true,
                ..RemoveOptions::default()
            },
        )
        .expect("remove save fixture");
    }
}
