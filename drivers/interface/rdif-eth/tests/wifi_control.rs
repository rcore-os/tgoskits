use rdif_eth::{InitIrqSources, WifiCommand, WifiCommandResult, WifiCommandSchedule};

#[test]
fn wifi_commands_own_all_configuration_bytes() {
    let command = WifiCommand::JoinStation {
        ssid: b"owner-net".to_vec(),
        passphrase: b"owner-secret".to_vec(),
    };

    assert_eq!(
        command,
        WifiCommand::JoinStation {
            ssid: b"owner-net".to_vec(),
            passphrase: b"owner-secret".to_vec(),
        }
    );
    assert_eq!(
        WifiCommandResult::StationConnected,
        WifiCommandResult::StationConnected
    );
}

#[test]
fn wifi_command_schedule_requires_an_explicit_activation_source() {
    assert!(WifiCommandSchedule::default().validate().is_err());
    assert!(WifiCommandSchedule::run_again().validate().is_ok());
    assert!(
        WifiCommandSchedule::wait_for_irq(InitIrqSources::from_bits(0x40))
            .validate()
            .is_ok()
    );
    assert!(WifiCommandSchedule::wait_until(123_000).validate().is_ok());
}
