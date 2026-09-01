//! Host-console ownership and mandatory guest virtual serial multiplexing.

mod host;
mod mux;
mod terminal;

#[cfg(feature = "test-console-atomic-output")]
pub(crate) use host::fill_runtime_output_queue;
pub(crate) use host::{
    configure_host_console, read_host_byte, read_host_log, submit_host_bytes, take_host_log_drops,
    wait_for_host_event,
};
#[cfg_attr(
    feature = "no-auto-start",
    expect(
        unused_imports,
        reason = "only the auto-start boot path attaches the console to a default running VM"
    )
)]
pub(crate) use mux::attach_default;
#[cfg(feature = "browser-console")]
pub(crate) use mux::route_network_input;
pub(crate) use mux::{
    ConsoleInputEvent, activate, attach, attached_vm, mark_running, mark_stopped,
    reconcile_vm_states, remove, route_host_byte, route_host_log, serial_backend_factory,
};
