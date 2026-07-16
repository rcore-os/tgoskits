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
fn host_tooling_validates_toml_for_the_requested_target_architecture() {
    let config = MINIMAL_CONFIG
        .replace("mode = \"virtual\"", "mode = \"passthrough\"")
        .replace("guest_base = 0x8000_0000", "guest_base = 0")
        .replace(
            "backing = { kind = \"allocate\" }",
            "backing = { kind = \"identity-allocate\" }",
        );

    assert!(matches!(
        AxVMCrateConfig::from_toml_for_target_arch(&config, "aarch64"),
        Err(AxVmConfigError::UnsupportedIdentityAllocatedMemory { .. })
    ));
    AxVMCrateConfig::from_toml_for_target_arch(&config, "x86_64").unwrap();
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
fn rejects_overlapping_guest_memory_regions() {
    let invalid = MINIMAL_CONFIG.replace(
        "[devices]\n",
        r#"[[memory.regions]]
guest_base = 0x8fff_f000
size = 0x2000
permissions = "rw"
backing = { kind = "allocate" }

[devices]
"#,
    );

    assert!(matches!(
        AxVMCrateConfig::from_toml(&invalid),
        Err(AxVmConfigError::OverlappingMemoryRegions { .. })
    ));
}

#[test]
fn identity_allocated_passthrough_memory_may_overlap_a_fixed_low_scratch_range() {
    let regions = [
        MemoryRegionConfig {
            guest_base: 0,
            size: 0x800_0000,
            permissions: MemoryPermissions::default(),
            backing: MemoryBackingConfig::IdentityAllocate,
        },
        MemoryRegionConfig {
            guest_base: 0,
            size: 0x10_0000,
            permissions: MemoryPermissions::default(),
            backing: MemoryBackingConfig::Allocate,
        },
    ];

    validate_memory_regions(&regions, VmMachineMode::Passthrough, "x86_64").unwrap();
}

#[test]
fn identity_allocated_memory_rejects_a_fixed_guest_base() {
    let region = MemoryRegionConfig {
        guest_base: 0x10_0000,
        size: 0x800_0000,
        permissions: MemoryPermissions::default(),
        backing: MemoryBackingConfig::IdentityAllocate,
    };

    assert!(matches!(
        validate_memory_regions(&[region], VmMachineMode::Passthrough, "x86_64"),
        Err(AxVmConfigError::InvalidIdentityAllocatedMemoryBase {
            guest_base: 0x10_0000
        })
    ));
}

#[test]
fn identity_allocated_memory_is_x86_passthrough_only() {
    let region = MemoryRegionConfig {
        guest_base: 0,
        size: 0x800_0000,
        permissions: MemoryPermissions::default(),
        backing: MemoryBackingConfig::IdentityAllocate,
    };

    assert!(matches!(
        validate_memory_regions(&[region.clone()], VmMachineMode::Virtual, "x86_64"),
        Err(AxVmConfigError::UnsupportedIdentityAllocatedMemory { .. })
    ));
    assert!(matches!(
        validate_memory_regions(&[region], VmMachineMode::Passthrough, "aarch64"),
        Err(AxVmConfigError::UnsupportedIdentityAllocatedMemory { .. })
    ));
}

#[test]
fn fixed_host_backings_publish_early_physical_reservations() {
    let regions = [
        MemoryRegionConfig {
            guest_base: 0x8000_0000,
            size: 0x2000_0000,
            permissions: MemoryPermissions::default(),
            backing: MemoryBackingConfig::Host {
                host_base: 0x1_8000_0000,
            },
        },
        MemoryRegionConfig {
            guest_base: 0xa000_0000,
            size: 0x1000,
            permissions: MemoryPermissions::default(),
            backing: MemoryBackingConfig::Shared {
                host_base: 0x2_0000_0000,
            },
        },
        MemoryRegionConfig {
            guest_base: 0xb000_0000,
            size: 0x20_0000,
            permissions: MemoryPermissions::default(),
            backing: MemoryBackingConfig::Reserved,
        },
        MemoryRegionConfig {
            guest_base: 0xc000_0000,
            size: 0x20_0000,
            permissions: MemoryPermissions::default(),
            backing: MemoryBackingConfig::Allocate,
        },
    ];

    let reservations = regions
        .iter()
        .filter_map(MemoryRegionConfig::host_physical_reservation)
        .collect::<Vec<_>>();

    assert_eq!(
        reservations,
        vec![
            HostPhysicalMemoryReservation::new(0x1_8000_0000, 0x2000_0000),
            HostPhysicalMemoryReservation::new(0x2_0000_0000, 0x1000),
            HostPhysicalMemoryReservation::new(0xb000_0000, 0x20_0000),
        ]
    );
}

#[test]
fn fixed_host_reservation_requires_complete_reserved_range_coverage() {
    let reservation = HostPhysicalMemoryReservation::new(0x1_0000, 0x3000);

    assert!(reservation.is_covered_by([
        HostPhysicalMemoryReservation::new(0x1_2000, 0x1000),
        HostPhysicalMemoryReservation::new(0x1_0000, 0x2000),
    ]));
    assert!(!reservation.is_covered_by([
        HostPhysicalMemoryReservation::new(0x1_0000, 0x1000),
        HostPhysicalMemoryReservation::new(0x1_2000, 0x1000),
    ]));
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
