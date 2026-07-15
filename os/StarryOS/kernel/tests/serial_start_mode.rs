#[path = "../src/pseudofs/dev/tty/serial_start.rs"]
mod serial_start;

use core::sync::atomic::{AtomicUsize, Ordering};

use serial_start::{
    FailedStartRecovery, SerialStartMode, SerialStartPolicy, SerialStartPolicyError,
};

#[test]
fn boot_console_adoption_never_recomputes_the_line_baudrate() {
    static READS: AtomicUsize = AtomicUsize::new(0);
    READS.store(0, Ordering::Release);

    let baudrate = SerialStartMode::AdoptBootConfiguration.startup_baudrate(|| {
        READS.fetch_add(1, Ordering::AcqRel);
        1_500_000
    });

    assert_eq!(baudrate, None);
    assert_eq!(READS.load(Ordering::Acquire), 0);
}

#[test]
fn an_uninitialized_ordinary_port_uses_the_runtime_default() {
    assert_eq!(
        SerialStartMode::ConfigurePort.startup_baudrate(|| 0),
        Some(115_200)
    );
    assert_eq!(
        SerialStartMode::ConfigurePort.startup_baudrate(|| 1_500_000),
        Some(1_500_000)
    );
}

#[test]
fn startup_role_is_assigned_once_and_cannot_change_before_handover() {
    let policy = SerialStartPolicy::new();
    assert_eq!(policy.mode(), Err(SerialStartPolicyError::Unassigned));
    assert_eq!(policy.assign(SerialStartMode::ConfigurePort), Ok(()));
    assert_eq!(policy.mode(), Ok(SerialStartMode::ConfigurePort));
    assert_eq!(
        policy.assign(SerialStartMode::AdoptBootConfiguration),
        Err(SerialStartPolicyError::AlreadyAssigned),
    );
    assert_eq!(policy.mode(), Ok(SerialStartMode::ConfigurePort));
}

#[test]
fn failed_adoption_restores_polling_while_ordinary_ports_shut_down() {
    assert_eq!(
        SerialStartMode::AdoptBootConfiguration.failed_start_recovery(),
        FailedStartRecovery::RestoreBootPolling
    );
    assert_eq!(
        SerialStartMode::ConfigurePort.failed_start_recovery(),
        FailedStartRecovery::ShutdownPort
    );
}
