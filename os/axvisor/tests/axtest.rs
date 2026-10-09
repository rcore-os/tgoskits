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

use ax_hal as _;
use ax_std as _;
use axvisor as _;
use axvm as _;

// Compile the production guest-console mux with narrow host/manager adapters
// so its application-layer state machine is exercised by the kernel harness.
// These modules stay reachable from the production binary build; the harness
// compiles them only for their in-file axtest suites.
#[allow(dead_code)]
#[path = "../src/network_console/delivery.rs"]
mod browser_console_delivery;
#[allow(dead_code)]
#[path = "../src/network_console/layout.rs"]
mod browser_console_layout;
mod guest_console_harness;
#[allow(dead_code)]
#[path = "../src/guest_console/terminal.rs"]
mod host_terminal;
mod manager;
mod network_console;

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
    fn diskless_boot_keeps_memory_root_with_inherited_root_parameter() {
        ax_assert!(
            ax_fs_ng::block::runtime::BlockRuntime::installed_devices()
                .is_none_or(|devices| devices.is_empty())
        );
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

    fn remove_guest_console(vm_id: usize) {
        use crate::guest_console_harness::mux;

        let identity = mux::backend_identity(vm_id).expect("guest backend must be registered");
        assert!(mux::remove_if_backend(identity));
    }

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
        remove_guest_console(1);
        remove_guest_console(2);
    }

    #[test]
    fn guest_output_skips_network_path_without_a_browser_session() {
        use crate::{guest_console_harness::mux, network_console};

        network_console::reset();
        let backend = mux::serial_backend_factory(1).create();
        mux::mark_running(1);

        backend.write(b"physical console only\n");

        ax_assert!(network_console::take_guest_output(1).is_empty());
        remove_guest_console(1);
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
        remove_guest_console(1);
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
        remove_guest_console(2);
    }

    #[test]
    fn blocked_physical_output_keeps_guest_bytes_out_of_network_delivery() {
        use crate::{guest_console_harness, network_console};

        network_console::reset();
        network_console::set_guest_connected(1);
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

        network_console::set_guest_connected(1);
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
}
