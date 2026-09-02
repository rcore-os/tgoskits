use super::*;

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn stop_request_preserves_buffered_and_trailing_output_until_vm_exit() {
    let mux = GuestConsoleMux::new();
    let backend = mux.core.create_serial_backend(4);
    mux.set_running([4]);
    assert_eq!(
        mux.core
            .format_guest_output(4, backend.generation, b"before stop\n"),
        Some(Vec::new())
    );

    assert_eq!(mux.set_vm_states([], [4]), None);
    assert_eq!(mux.core.lock_state().output.pending_len(4), 12);
    assert_eq!(
        mux.core
            .format_guest_output(4, backend.generation, b"after request\n"),
        Some(Vec::new())
    );

    assert_eq!(mux.set_running([]), None);
    assert_eq!(mux.core.lock_state().output.pending_len(4), 0);
    assert_eq!(
        mux.core
            .format_guest_output(4, backend.generation, b"after exit\n"),
        None
    );
}
