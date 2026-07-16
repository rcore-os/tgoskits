use axvm_types::{GuestFirmwareKind, InterruptDelivery, VmMachineMode};
use axvmconfig::MachineConfig;

#[test]
fn passthrough_interrupts_default_to_mediated() {
    let machine: MachineConfig = toml::from_str(
        r#"
mode = "passthrough"
firmware = "auto"
"#,
    )
    .unwrap();

    assert_eq!(machine.mode(), VmMachineMode::Passthrough);
    assert_eq!(machine.firmware(), GuestFirmwareKind::Auto);
    assert_eq!(machine.interrupt_delivery(), InterruptDelivery::Mediated);
}

#[test]
fn passthrough_interrupts_can_be_enabled_explicitly() {
    let machine: MachineConfig = toml::from_str(
        r#"
mode = "passthrough"
firmware = "fdt"
interrupts_passthrough = true
"#,
    )
    .unwrap();

    assert_eq!(machine.mode(), VmMachineMode::Passthrough);
    assert_eq!(machine.firmware(), GuestFirmwareKind::Fdt);
    assert_eq!(machine.interrupt_delivery(), InterruptDelivery::Direct);
}

#[test]
fn virtual_machine_rejects_passthrough_interrupt_field_even_when_false() {
    let result = toml::from_str::<MachineConfig>(
        r#"
mode = "virtual"
firmware = "auto"
interrupts_passthrough = false
"#,
    );

    assert!(result.is_err());
}
