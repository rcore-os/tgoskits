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
#[axtest::tests]
mod tests {
    use axtest::prelude::*;

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
}
