//! Host-console ownership and mandatory guest virtual serial multiplexing.

mod host;
mod mux;
mod terminal;

pub(crate) use host::{
    configure_host_console, read_host_byte, read_host_log, submit_host_bytes, subscribe_lifecycle,
    take_host_log_drops, wait_for_host_event,
};
#[cfg(feature = "web")]
pub(crate) use mux::route_network_input;
pub(crate) use mux::{
    ConsoleAttachment, ConsoleInputEvent, activate, attach, attached_vm, backend_identity,
    mark_running, mark_stopped, notify_retained_tx_retries, reconcile_vm_states, remove_if_backend,
    replay_guest_output, route_host_byte, route_host_log, serial_backend_factory,
};
