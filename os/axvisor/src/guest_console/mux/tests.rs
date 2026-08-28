use super::*;

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn host_log_record_terminates_an_open_guest_line() {
    let mux = GuestConsoleMux::new();
    {
        let mut state = mux.core.lock_state();
        assert_eq!(state.output.format(1, false, b"guest> "), b"guest> ");
    }

    assert_eq!(
        mux.route_host_log(b"host record\n", 0, 0),
        Some(b"\nhost record\n".to_vec())
    );
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn foreground_guest_buffers_whole_host_records_and_replays_on_detach() {
    let mux = GuestConsoleMux::new();
    {
        let mut state = mux.core.lock_state();
        state.output.enter_interactive(1);
    }
    assert_eq!(mux.route_host_log(b"first\n", 0, 0), None);
    assert_eq!(mux.route_host_log(b"second\n", 0, 0), None);

    let mut state = mux.core.lock_state();
    state.output.buffer_all();
    let mut replay = Vec::new();
    append_host_log_replay(&mut state, &mut replay);
    assert_eq!(replay, b"first\nsecond\n");
}

fn route_shortcut(mux: &GuestConsoleMux, suffix: u8) -> ConsoleInputEvent {
    assert_eq!(
        mux.route_host_byte(CTRL_X).event,
        ConsoleInputEvent::Consumed
    );
    mux.route_host_byte(suffix).event
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn ctrl_x_h_detaches_the_foreground_guest() {
    let mux = GuestConsoleMux::new();
    mux.core.create_serial_backend(7);
    assert_eq!(mux.attach_default([7]), Some(7));

    assert_eq!(mux.route_host_byte(0x18).event, ConsoleInputEvent::Consumed);
    assert_eq!(
        mux.route_host_byte(b'h').event,
        ConsoleInputEvent::Detached(7)
    );
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
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

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn network_input_reaches_its_guest_without_changing_physical_foreground() {
    let mux = GuestConsoleMux::new();
    let backend_1 = mux.core.create_serial_backend(1);
    let backend_2 = mux.core.create_serial_backend(2);
    assert_eq!(mux.attach_default([1, 2]), Some(1));

    assert_eq!(mux.route_network_input(2, b"uptime\r"), Some(false));
    assert_eq!(mux.attached_vm(), Some(1));

    let mut input = [0u8; 16];
    assert_eq!(backend_1.read(&mut input), 0);
    let read = backend_2.read(&mut input);
    assert_eq!(&input[..read], b"uptime\r");
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn network_input_rejects_stopped_and_stale_guest_backends() {
    let mux = GuestConsoleMux::new();
    mux.core.create_serial_backend(1);
    mux.set_running([1]);
    mux.mark_stopped(1);

    assert_eq!(mux.route_network_input(1, b"ignored"), None);
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn guest_input_overflow_is_reported_once_until_the_guest_drains_input() {
    let mux = GuestConsoleMux::new();
    let backend = mux.core.create_serial_backend(1);
    let mut state = mux.core.lock_state();
    state.attached = Some(1);

    assert!(!enqueue_guest_input(
        &mut state,
        1,
        &[b'x'; INPUT_QUEUE_CAPACITY]
    ));
    assert!(
        route_literal_input(&mut state, b"y", ConsoleInputEvent::ShellByte(b'y'))
            .input_overflow
            .is_some()
    );
    assert!(
        route_literal_input(&mut state, b"z", ConsoleInputEvent::ShellByte(b'z'))
            .input_overflow
            .is_none()
    );
    drop(state);

    assert_eq!(backend.read(&mut [0]), 1);
    let mut state = mux.core.lock_state();
    assert!(!enqueue_guest_input(&mut state, 1, b"w"));
    assert!(enqueue_guest_input(&mut state, 1, b"q"));
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn console_shortcuts_detach_and_cycle_running_guests() {
    let mux = GuestConsoleMux::new();
    for vm_id in [2, 7, 10] {
        mux.core.create_serial_backend(vm_id);
    }
    assert_eq!(mux.attach_default([10, 2, 7]), Some(2));

    assert_eq!(route_shortcut(&mux, b']'), ConsoleInputEvent::Attached(7));
    assert_eq!(route_shortcut(&mux, b']'), ConsoleInputEvent::Attached(10));
    assert_eq!(route_shortcut(&mux, b'['), ConsoleInputEvent::Attached(7));
    assert_eq!(route_shortcut(&mux, b'h'), ConsoleInputEvent::Detached(7));
    assert_eq!(mux.attached_vm(), None);
    assert_eq!(route_shortcut(&mux, b']'), ConsoleInputEvent::Attached(10));
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn non_shortcut_ctrl_x_sequences_reach_the_current_console() {
    let mux = GuestConsoleMux::new();
    let backend = mux.core.create_serial_backend(7);
    assert_eq!(mux.attach_default([7]), Some(7));

    assert_eq!(route_shortcut(&mux, b'z'), ConsoleInputEvent::Consumed);
    let mut input = [0u8; 2];
    assert_eq!(backend.read(&mut input), 2);
    assert_eq!(input, [CTRL_X, b'z']);

    assert_eq!(route_shortcut(&mux, b'h'), ConsoleInputEvent::Detached(7));
    assert_eq!(
        route_shortcut(&mux, b'z'),
        ConsoleInputEvent::ShellSequence(CTRL_X, b'z')
    );
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn doubled_ctrl_x_reaches_the_current_console_as_one_byte() {
    let mux = GuestConsoleMux::new();
    let backend = mux.core.create_serial_backend(7);
    assert_eq!(mux.attach_default([7]), Some(7));

    assert_eq!(route_shortcut(&mux, CTRL_X), ConsoleInputEvent::Consumed);
    let mut input = [0u8; 2];
    assert_eq!(backend.read(&mut input), 1);
    assert_eq!(input[0], CTRL_X);
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn stopping_foreground_guest_returns_to_shell() {
    let mux = GuestConsoleMux::new();
    mux.core.create_serial_backend(3);
    mux.attach_default([3]);

    assert_eq!(mux.set_running([]), Some(3));
    assert_eq!(mux.attached_vm(), None);
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
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

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
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
        Some(b"[VM 1] ready\n".to_vec())
    );
    assert_eq!(
        mux.core
            .format_guest_output(2, backend_2.generation, b"other\n"),
        Some(b"[VM 2] other\n".to_vec())
    );
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"> \n"),
        Some(b"[VM 1] prompt> \n".to_vec())
    );
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn default_attachment_preempts_an_unterminated_background_fragment() {
    let mux = GuestConsoleMux::new();
    let backend_1 = mux.core.create_serial_backend(1);
    let backend_2 = mux.core.create_serial_backend(2);
    mux.set_running([1, 2]);
    assert_eq!(
        mux.core
            .format_guest_output(2, backend_2.generation, b"background"),
        Some(Vec::new())
    );

    assert_eq!(mux.attach_default([1, 2]), Some(1));
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"ready\n"),
        Some(b"[VM 1] ready\n".to_vec())
    );
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn foreground_input_preempts_an_unterminated_background_fragment() {
    let mux = GuestConsoleMux::new();
    let backend_1 = mux.core.create_serial_backend(1);
    let backend_2 = mux.core.create_serial_backend(2);
    assert_eq!(mux.attach_default([1, 2]), Some(1));
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"ready\n"),
        Some(b"[VM 1] ready\n".to_vec())
    );
    assert_eq!(
        mux.core
            .format_guest_output(2, backend_2.generation, b"~ # "),
        Some(Vec::new())
    );

    assert_eq!(mux.route_host_byte(b'x').event, ConsoleInputEvent::Consumed);
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"echo\n"),
        Some(b"echo\n".to_vec())
    );
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn foreground_command_result_preempts_after_its_echo_completed() {
    let mux = GuestConsoleMux::new();
    let backend_1 = mux.core.create_serial_backend(1);
    let backend_2 = mux.core.create_serial_backend(2);
    assert_eq!(mux.attach_default([1, 2]), Some(1));
    assert_eq!(
        mux.core
            .format_guest_output(2, backend_2.generation, b"~ # "),
        Some(Vec::new())
    );

    assert_eq!(mux.route_host_byte(b'x').event, ConsoleInputEvent::Consumed);
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"x\n"),
        Some(b"x\n".to_vec())
    );
    assert_eq!(
        mux.core
            .format_guest_output(2, backend_2.generation, b"waiting"),
        Some(Vec::new())
    );

    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"RESULT\n"),
        Some(b"RESULT\n".to_vec())
    );
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn first_foreground_input_enters_interactive_exclusive_mode() {
    let mux = GuestConsoleMux::new();
    let backend_1 = mux.core.create_serial_backend(1);
    let backend_2 = mux.core.create_serial_backend(2);
    assert_eq!(mux.attach_default([1, 2]), Some(1));
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"vm1 booted\n"),
        Some(b"[VM 1] vm1 booted\n".to_vec())
    );
    assert_eq!(
        mux.core
            .format_guest_output(2, backend_2.generation, b"vm2 booted\n"),
        Some(b"[VM 2] vm2 booted\n".to_vec())
    );
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"~ # "),
        Some(Vec::new())
    );

    let routed = mux.route_host_byte(b'x');
    assert_eq!(routed.event, ConsoleInputEvent::Consumed);
    assert_eq!(routed.host_output, b"~ # ");
    assert_eq!(
        mux.core
            .format_guest_output(2, backend_2.generation, b"background\n"),
        Some(Vec::new())
    );
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"result\n"),
        Some(b"result\n".to_vec())
    );
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn switching_guests_replays_background_log_before_direct_output() {
    let mux = GuestConsoleMux::new();
    let backend_1 = mux.core.create_serial_backend(1);
    let backend_2 = mux.core.create_serial_backend(2);
    assert_eq!(mux.attach_default([1, 2]), Some(1));

    assert_eq!(mux.route_host_byte(b'x').event, ConsoleInputEvent::Consumed);
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"foreground\n"),
        Some(b"foreground\n".to_vec())
    );
    assert_eq!(
        mux.core
            .format_guest_output(2, backend_2.generation, b"background\n"),
        Some(Vec::new())
    );

    assert_eq!(
        mux.route_host_byte(CTRL_X).event,
        ConsoleInputEvent::Consumed
    );
    let switched = mux.route_host_byte(b']');
    assert_eq!(switched.event, ConsoleInputEvent::Attached(2));
    assert_eq!(
        mux.core
            .format_guest_output(2, backend_2.generation, b"before activation\n"),
        Some(Vec::new())
    );
    assert_eq!(
        mux.activate(2),
        Some(b"background\nbefore activation\n".to_vec())
    );
    assert_eq!(
        mux.core
            .format_guest_output(2, backend_2.generation, b"direct\n"),
        Some(b"direct\n".to_vec())
    );
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn detaching_buffers_all_guest_output() {
    let mux = GuestConsoleMux::new();
    let backend = mux.core.create_serial_backend(1);
    assert_eq!(mux.attach_default([1]), Some(1));
    assert_eq!(mux.route_host_byte(b'x').event, ConsoleInputEvent::Consumed);
    assert_eq!(
        mux.core.format_guest_output(1, backend.generation, b"~ # "),
        Some(b"~ # ".to_vec())
    );

    assert_eq!(
        mux.route_host_byte(CTRL_X).event,
        ConsoleInputEvent::Consumed
    );
    let detached = mux.route_host_byte(b'h');
    assert_eq!(detached.event, ConsoleInputEvent::Detached(1));
    assert_eq!(detached.host_output, b"\n");
    assert_eq!(
        mux.core
            .format_guest_output(1, backend.generation, b"while detached\n"),
        Some(Vec::new())
    );
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn switching_guests_terminates_unfinished_foreground_output() {
    let mux = GuestConsoleMux::new();
    let backend_1 = mux.core.create_serial_backend(1);
    mux.core.create_serial_backend(2);
    assert_eq!(mux.attach_default([1, 2]), Some(1));
    assert_eq!(mux.route_host_byte(b'x').event, ConsoleInputEvent::Consumed);
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"~ # "),
        Some(b"~ # ".to_vec())
    );

    assert_eq!(
        mux.route_host_byte(CTRL_X).event,
        ConsoleInputEvent::Consumed
    );
    let switched = mux.route_host_byte(b']');

    assert_eq!(switched.event, ConsoleInputEvent::Attached(2));
    assert_eq!(switched.host_output, b"\n");
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn guest_switch_preempts_an_unterminated_background_fragment() {
    let mux = GuestConsoleMux::new();
    let backend_1 = mux.core.create_serial_backend(1);
    let backend_2 = mux.core.create_serial_backend(2);
    assert_eq!(mux.attach_default([1, 2]), Some(1));
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"ready\n"),
        Some(b"[VM 1] ready\n".to_vec())
    );
    assert_eq!(
        mux.core
            .format_guest_output(1, backend_1.generation, b"~ # "),
        Some(Vec::new())
    );

    assert_eq!(
        mux.route_host_byte(CTRL_X).event,
        ConsoleInputEvent::Consumed
    );
    let switched = mux.route_host_byte(b']');
    assert_eq!(switched.event, ConsoleInputEvent::Attached(2));
    assert_eq!(mux.activate(2), Some(Vec::new()));
    assert_eq!(
        mux.core
            .format_guest_output(2, backend_2.generation, b"ready\n"),
        Some(b"ready\n".to_vec())
    );
}

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
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

#[cfg_attr(axtest, axtest::axtest)]
#[cfg_attr(not(axtest), test)]
fn stale_backends_leave_output_arbitration_unchanged() {
    let mux = GuestConsoleMux::new();

    let stopped = mux.core.create_serial_backend(1);
    mux.set_running([1]);
    mux.mark_stopped(1);
    let stopped_snapshot = mux.core.lock_state().output.snapshot();
    stopped.write(b"late stopped output");
    assert_eq!(mux.core.lock_state().output.snapshot(), stopped_snapshot);

    let removed = mux.core.create_serial_backend(2);
    mux.set_running([2]);
    mux.remove(2);
    let removed_snapshot = mux.core.lock_state().output.snapshot();
    removed.write(b"late removed output");
    assert_eq!(mux.core.lock_state().output.snapshot(), removed_snapshot);

    let replaced = mux.core.create_serial_backend(3);
    mux.set_running([3]);
    let _current = mux.core.create_serial_backend(3);
    let replaced_snapshot = mux.core.lock_state().output.snapshot();
    replaced.write(b"late replaced output");
    assert_eq!(mux.core.lock_state().output.snapshot(), replaced_snapshot);
}
