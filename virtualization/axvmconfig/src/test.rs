use super::*;

const MINIMAL_CONFIG: &str = r#"
[machine]
mode = "virtual"
firmware = "fdt"

[base]
id = 1
name = "typed-machine"
cpu_num = 1

[kernel]
entry_point = 0x8020_0000
kernel_path = "/guest/kernel"
kernel_load_addr = 0x8020_0000
image_location = "fs"

[[memory.regions]]
guest_base = 0x8000_0000
size = 0x1000_0000
permissions = "rwx"
backing = { kind = "allocate" }

[devices]
disable_defaults = []

[[devices.virtual]]
id = "console0"
model = "arm-pl011"
source = { kind = "auto" }
backend = { kind = "host-console", rx = "exclusive", tx = "shared" }
"#;

#[test]
fn parses_typed_machine_memory_and_virtual_device() {
    let config = AxVMCrateConfig::from_toml(MINIMAL_CONFIG).unwrap();

    assert_eq!(config.machine.mode(), VmMachineMode::Virtual);
    assert_eq!(config.memory.regions.len(), 1);
    assert_eq!(config.memory.regions[0].permissions.as_str(), "rwx");
    assert_eq!(config.devices.virtual_devices.len(), 1);
    assert_eq!(config.devices.virtual_devices[0].model, "arm-pl011");
}

#[test]
fn rejects_removed_legacy_machine_and_device_fields() {
    let legacy = r#"
[machine]
mode = "passthrough"

[base]
id = 1
name = "legacy"
vm_type = 1
cpu_num = 1

[kernel]
entry_point = 0
kernel_path = "kernel"
kernel_load_addr = 0
memory_regions = []

[memory]
regions = []

[devices]
emu_devices = []
passthrough_devices = []
"#;

    let error = AxVMCrateConfig::from_toml(legacy).unwrap_err();
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn rejects_overflowing_explicit_host_backing() {
    let invalid = MINIMAL_CONFIG.replace(
        "backing = { kind = \"allocate\" }",
        "backing = { kind = \"host\", host_base = 0xfffffffffffffff0 }",
    );

    assert!(matches!(
        AxVMCrateConfig::from_toml(&invalid),
        Err(AxVmConfigError::InvalidMemoryBacking { .. })
    ));
}

#[test]
fn only_the_optional_console_default_can_be_disabled() {
    let invalid = MINIMAL_CONFIG.replace(
        "disable_defaults = []",
        "disable_defaults = [\"architected-timer\"]",
    );

    let error = AxVMCrateConfig::from_toml(&invalid).unwrap_err();
    assert!(error.to_string().contains("architected-timer"));
}
