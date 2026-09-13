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
#[cfg(feature = "browser-console")]
pub(crate) use mux::route_network_input;
pub(crate) use mux::{
    ConsoleAttachment, ConsoleInputEvent, activate, attach, attached_vm, backend_identity,
    mark_running, mark_stopped, reconcile_vm_states, remove_if_backend, replay_guest_output,
    route_host_byte, route_host_log, serial_backend_factory,
};
