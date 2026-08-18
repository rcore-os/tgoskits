//! Host-console ownership and mandatory guest virtual serial multiplexing.

mod host;
mod mux;

pub(crate) use host::{configure_host_console_reader, read_host_byte, wait_for_host_input};
#[cfg_attr(
    feature = "no-auto-start",
    expect(
        unused_imports,
        reason = "only the auto-start boot path attaches the console to a default running VM"
    )
)]
pub(crate) use mux::attach_default;
pub(crate) use mux::{
    ConsoleInputEvent, activate, attach, attached_vm, mark_running, mark_stopped,
    reconcile_vm_states, remove, route_host_byte, serial_backend_factory,
};
