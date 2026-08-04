//! Host-console ownership and mandatory guest virtual serial multiplexing.

mod host;
mod mux;

pub(crate) use host::{configure_host_console_reader, read_host_byte, wait_for_host_input};
pub(crate) use mux::{
    ConsoleInputEvent, attach, attach_default, attached_vm, mark_running, mark_stopped,
    reconcile_vm_states, remove, route_host_byte, serial_backend_factory,
};
