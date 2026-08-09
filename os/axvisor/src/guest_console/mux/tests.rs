use super::*;

fn route_shortcut(mux: &GuestConsoleMux, suffix: u8) -> ConsoleInputEvent {
    assert_eq!(mux.route_host_byte(ESC).event, ConsoleInputEvent::Consumed);
    mux.route_host_byte(suffix).event
}

#[test]
fn lowest_running_vm_is_default_and_input_only_reaches_foreground() {
    let mux = GuestConsoleMux::new();
    let backend_1 = mux.core.create_serial_backend(1);
    let backend_2 = mux.core.create_serial_backend(2);

    assert_eq!(mux.attach_default([2, 1]), Some(1));
    assert_eq!(mux.route_host_byte(b'x').event, ConsoleInputEvent::Consumed);

    let mut input = [0u8; 2];
    assert_eq!(backend_1.read(&mut input), 1);
    assert_eq!(input[0], b'x');
    assert_eq!(backend_2.read(&mut input), 0);
}

#[test]
fn console_shortcuts_detach_and_cycle_running_guests() {
    let mux = GuestConsoleMux::new();
    for vm_id in [2, 7, 10] {
        mux.core.create_serial_backend(vm_id);
    }
    assert_eq!(mux.attach_default([10, 2, 7]), Some(2));

    assert_eq!(
        route_shortcut(&mux, CTRL_RIGHT_BRACKET),
        ConsoleInputEvent::Attached(7)
    );
    assert_eq!(
        route_shortcut(&mux, CTRL_RIGHT_BRACKET),
        ConsoleInputEvent::Attached(10)
    );
    assert_eq!(route_shortcut(&mux, ESC), ConsoleInputEvent::Attached(7));
    assert_eq!(route_shortcut(&mux, CTRL_H), ConsoleInputEvent::Detached(7));
    assert_eq!(mux.attached_vm(), None);
    assert_eq!(
        route_shortcut(&mux, CTRL_RIGHT_BRACKET),
        ConsoleInputEvent::Attached(10)
    );
}

#[test]
fn non_shortcut_escape_sequences_reach_the_current_console() {
    let mux = GuestConsoleMux::new();
    let backend = mux.core.create_serial_backend(7);
    assert_eq!(mux.attach_default([7]), Some(7));

    assert_eq!(route_shortcut(&mux, b'['), ConsoleInputEvent::Consumed);
    let mut input = [0u8; 2];
    assert_eq!(backend.read(&mut input), 2);
    assert_eq!(input, [ESC, b'[']);

    assert_eq!(route_shortcut(&mux, CTRL_H), ConsoleInputEvent::Detached(7));
    assert_eq!(
        route_shortcut(&mux, b'['),
        ConsoleInputEvent::ShellSequence(ESC, b'[')
    );
}

#[test]
fn stopping_foreground_guest_returns_to_shell() {
    let mux = GuestConsoleMux::new();
    mux.core.create_serial_backend(3);
    mux.attach_default([3]);

    assert_eq!(mux.set_running([]), Some(3));
    assert_eq!(mux.attached_vm(), None);
}

#[test]
fn stopped_or_removed_guest_invalidates_its_serial_backend_generation() {
    let mux = GuestConsoleMux::new();
    let backend = mux.core.create_serial_backend(4);
    assert_eq!(mux.attach_default([4]), Some(4));
    assert_eq!(mux.route_host_byte(b'x').event, ConsoleInputEvent::Consumed);

    assert!(mux.mark_stopped(4));

    let mut input = [0u8; 1];
    assert_eq!(backend.read(&mut input), 0);
    assert_eq!(
        mux.core
            .format_guest_output(4, backend.generation, b"late output"),
        None
    );

    let removed_backend = mux.core.create_serial_backend(3);
    mux.remove(3);
    removed_backend.write(b"late output");
    assert!(!mux.core.lock_state().guests.contains_key(&3));
}

#[test]
fn multiple_running_guests_receive_line_prefixes() {
    let mux = GuestConsoleMux::new();
    let backend_1 = mux.core.create_serial_backend(1);
    mux.set_running([1]);
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"boot\n"),
        Some(b"boot\n".to_vec())
    );

    let backend_2 = mux.core.create_serial_backend(2);
    mux.set_running([1, 2]);
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"ready\nprompt"),
        Some(b"[VM 1] ready\n[VM 1] prompt".to_vec())
    );
    assert_eq!(
        mux.core
            .format_guest_output(2, backend_2.generation, b"other\n"),
        Some(b"\n[VM 2] other\n".to_vec())
    );
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"> \n"),
        Some(b"[VM 1] > \n".to_vec())
    );
}

#[test]
fn replacement_backend_invalidates_the_previous_vm_generation() {
    let mux = GuestConsoleMux::new();
    let stale_backend = mux.core.create_serial_backend(8);
    let current_backend = mux.core.create_serial_backend(8);
    mux.attach_default([8]);
    mux.route_host_byte(b'x');

    let mut input = [0u8; 1];
    assert_eq!(stale_backend.read(&mut input), 0);
    assert_eq!(current_backend.read(&mut input), 1);
    assert_eq!(input, [b'x']);
    assert_eq!(
        mux.core
            .format_guest_output(8, stale_backend.generation, b"stale"),
        None
    );
}
