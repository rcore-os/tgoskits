#[path = "../src/shell/connection.rs"]
mod connection;

use connection::{
    CONSOLE_INPUT_READ_SIZE, ConnectError, ConnectionToken, ConsoleConnectionState, DetachEvent,
    split_console_input,
};

#[test]
fn stale_token_cannot_detach_reconnected_vm() {
    let state = ConsoleConnectionState::new();
    let old = state.connect(1).unwrap();
    assert!(state.detach(old).is_some());
    let new = state.connect(1).unwrap();

    assert_ne!(old, new);
    assert!(state.detach(old).is_none());
    assert_eq!(state.current(), Some(new));
}

#[test]
fn second_connect_is_rejected_until_exact_token_detaches() {
    let state = ConsoleConnectionState::new();
    let token = state.connect(2).unwrap();

    assert_eq!(state.connect(3), Err(ConnectError::AlreadyConnected));
    assert_eq!(
        state.detach(token),
        Some(DetachEvent::Detached { vm_id: 2 })
    );
    assert_eq!(state.detach(token), None);
    assert_eq!(state.current(), None);
}

#[test]
fn stale_output_pump_snapshot_preserves_new_connection() {
    let state = ConsoleConnectionState::new();
    let stale_snapshot = state.connect(4).unwrap();
    assert_eq!(
        state.detach(stale_snapshot),
        Some(DetachEvent::Detached { vm_id: 4 })
    );
    let current = state.connect(5).unwrap();

    assert_eq!(state.detach(stale_snapshot), None);
    assert_eq!(state.current(), Some(current));
}

#[test]
fn detach_event_belongs_only_to_compare_exchange_winner() {
    let state = ConsoleConnectionState::new();
    let token = state.connect(6).unwrap();
    let competing_snapshot = ConnectionToken {
        vm_id: token.vm_id,
        generation: token.generation.wrapping_add(1),
    };

    assert_eq!(state.detach(competing_snapshot), None);
    assert_eq!(
        state.detach(token),
        Some(DetachEvent::Detached { vm_id: 6 })
    );
}

#[test]
fn oversized_vm_id_is_rejected() {
    if usize::BITS > u32::BITS {
        let state = ConsoleConnectionState::new();
        assert_eq!(
            state.connect(u32::MAX as usize),
            Err(ConnectError::VmIdOutOfRange)
        );
    }
}

#[test]
fn console_input_splits_at_first_ctrl_right_bracket() {
    assert_eq!(
        split_console_input(b"abc\x1dignored"),
        (b"abc".as_slice(), true)
    );
    assert_eq!(split_console_input(b"abc"), (b"abc".as_slice(), false));
    assert_eq!(split_console_input(b"\x1d"), (b"".as_slice(), true));
}

#[test]
fn console_input_is_read_one_byte_per_uart_interrupt() {
    assert_eq!(CONSOLE_INPUT_READ_SIZE, 1);
}

#[test]
fn management_shell_starts_without_waiting_for_default_guests() {
    let main_source = include_str!("../src/main.rs");

    assert!(main_source.contains("manager.init_default_vms();"));
    assert!(!main_source.contains("manager.start_default_vms();"));
    assert!(main_source.contains("shell::console_init();"));
}
