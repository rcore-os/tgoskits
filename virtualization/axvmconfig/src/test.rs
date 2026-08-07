// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::*;

const MINIMAL_CONFIG: &str = r#"
[base]
id = 12
name = "test-guest"
guest_type = "passthrough"
cpu_num = 2
phys_cpu_sets = [3, 4]
phys_cpu_ids = [0x500, 0x501]

[kernel]
entry_point = 0xdeadbeef
image_location = "memory"
kernel_path = "guest.bin"
kernel_load_addr = 0xdeadbeef
memory_regions = [
    [0x8000_0000, 0x8000_0000, 0x7, 1],
]

[devices]
passthrough = [
    { path = "/soc/ethernet@1000" },
]
disabled = [
    { path = "/soc/gpio@2000" },
]
"#;

#[test]
fn parses_structured_guest_config() {
    let config = GuestConfig::from_toml(MINIMAL_CONFIG).unwrap();

    assert_eq!(config.base.id, 12);
    assert_eq!(config.base.name, "test-guest");
    assert_eq!(config.base.guest_type, GuestType::Passthrough);
    assert_eq!(config.base.cpu_num, 2);
    assert_eq!(config.base.phys_cpu_ids, Some(vec![0x500, 0x501]));
    assert_eq!(config.base.phys_cpu_sets, Some(vec![3, 4]));

    assert_eq!(config.kernel.entry_point, 0xdeadbeef);
    assert_eq!(config.kernel.configured_memory_region_count, 1);
    assert_eq!(
        config.kernel.memory_regions[0].map_type,
        VmMemMappingType::MapIdentical
    );

    assert_eq!(
        config.devices.passthrough,
        vec![PhysicalDeviceRef {
            path: "/soc/ethernet@1000".into(),
        }]
    );
    assert_eq!(
        config.devices.disabled,
        vec![PhysicalDeviceRef {
            path: "/soc/gpio@2000".into(),
        }]
    );
}

#[test]
fn parses_open_virtual_device_options() {
    let config = GuestConfig::from_toml(
        r#"
[devices]
[[devices.virtual]]
id = "data0"
model = "virtio-blk-like"
capacity = "20GiB"
backend = { type = "file", path = "/images/data.raw" }
"#,
    )
    .unwrap();
    let [request] = config.devices.virtual_devices.as_slice() else {
        panic!("expected one virtual device request");
    };
    assert_eq!(request.id, "data0");
    assert_eq!(request.model, "virtio-blk-like");
    assert_eq!(request.options["capacity"].as_str(), Some("20GiB"));
}

#[test]
fn rejects_duplicate_ids_and_numeric_resource_options() {
    let duplicate = GuestConfig::from_toml(
        r#"
[devices]
[[devices.virtual]]
id = "data0"
model = "demo"
[[devices.virtual]]
id = "data0"
model = "demo"
"#,
    )
    .unwrap_err();
    assert_eq!(
        duplicate,
        AxVmConfigError::DuplicateVirtualDeviceId { id: "data0".into() }
    );

    let raw_irq = GuestConfig::from_toml(
        r#"
[devices]
[[devices.virtual]]
id = "data0"
model = "demo"
irq_id = 32
"#,
    )
    .unwrap_err();
    assert_eq!(
        raw_irq,
        AxVmConfigError::ForbiddenVirtualDeviceResourceOption {
            id: "data0".into(),
            option: "irq_id".into(),
        }
    );
}

#[test]
fn guest_type_owns_address_space_policy() {
    assert_eq!(
        GuestType::Virtualized.address_space_policy(),
        AddressSpacePolicy::Virtualized
    );
    assert_eq!(
        GuestType::Passthrough.address_space_policy(),
        AddressSpacePolicy::Passthrough
    );

    let devices = GuestDevices {
        passthrough: vec![PhysicalDeviceRef {
            path: "/soc/net@1000".into(),
        }],
        disabled: Vec::new(),
        virtual_devices: Vec::new(),
        legacy_virtual_devices: Vec::new(),
    };
    let unresolved = devices.unresolved_host_devices();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].name, "/soc/net@1000");
    assert!(
        unresolved.iter().all(|device| device.name != "/"),
        "ordinary device passthrough must not invent a root selector"
    );
}

#[test]
fn rejects_removed_configuration_fields() {
    let removed_fields = [
        ("", "version = 1\n"),
        ("[devices]\n", "serial = {}\n"),
        ("[devices]\n", "interrupt_mode = \"passthrough\"\n"),
        ("[devices]\n", "passthrough_devices = []\n"),
        ("[devices]\n", "passthrough_addresses = []\n"),
        ("[devices]\n", "passthrough_ports = []\n"),
        ("[kernel]\n", "disk_path = \"disk.img\"\n"),
    ];

    for (table, field) in removed_fields {
        let raw = format!("{table}{field}");
        let error = GuestConfig::from_toml(&raw).unwrap_err();
        assert!(
            matches!(error, AxVmConfigError::TomlParse { .. }),
            "removed field unexpectedly parsed: {field}"
        );
    }
}

#[test]
fn parses_legacy_vm_type_and_emulated_devices() {
    let config = GuestConfig::from_toml(
        r#"
[base]
vm_type = 1

[devices]
emu_devices = [
  ["ivc-channel", 0xbff0_0000, 0x1_0000, 0, 0xA, [60]],
]
"#,
    )
    .unwrap();

    assert_eq!(config.base.guest_type, GuestType::Passthrough);
    assert!(config.devices.virtual_devices.is_empty());

    let [request] = config.devices.legacy_virtual_devices.as_slice() else {
        panic!("expected one legacy virtual device request");
    };
    assert_eq!(request.id, "ivc-channel@bff00000");
    assert_eq!(request.model, "ivc-channel");
    assert_eq!(
        request.options["legacy_base_gpa"].as_integer(),
        Some(0xbff0_0000)
    );
    assert_eq!(
        request.options["legacy_length"].as_integer(),
        Some(0x1_0000)
    );
    assert_eq!(request.options["notify_irq"].as_integer(), Some(60));
}

#[test]
fn rejects_unknown_nested_fields() {
    let error = GuestConfig::from_toml(
        r#"
[devices]
passthrough = [{ path = "/soc/net@1000", irq = 4 }]
"#,
    )
    .unwrap_err();
    assert!(matches!(error, AxVmConfigError::TomlParse { .. }));
}

#[test]
fn validates_physical_device_selectors() {
    let relative = GuestConfig::from_toml(
        r#"
[devices]
passthrough = [{ path = "soc/net@1000" }]
"#,
    )
    .unwrap_err();
    assert_eq!(
        relative,
        AxVmConfigError::InvalidPhysicalDevicePath {
            path: "soc/net@1000".into(),
        }
    );

    let root = GuestConfig::from_toml(
        r#"
[devices]
passthrough = [{ path = "/" }]
"#,
    )
    .unwrap();
    assert_eq!(root.devices.passthrough[0].path, "/");

    let conflict = GuestConfig::from_toml(
        r#"
[devices]
passthrough = [{ path = "/soc/net@1000" }]
disabled = [{ path = "/soc/net@1000" }]
"#,
    )
    .unwrap_err();
    assert_eq!(
        conflict,
        AxVmConfigError::ConflictingPhysicalDeviceSelection {
            path: "/soc/net@1000".into(),
        }
    );
}

#[test]
fn serialization_has_no_serial_or_raw_device_fields() {
    let encoded = toml::to_string(&GuestConfig::default()).unwrap();
    for removed in [
        "serial",
        "emu_devices",
        "cfg_list",
        "interrupt_mode",
        "passthrough_addresses",
        "passthrough_ports",
        "vm_type",
        "version",
    ] {
        assert!(!encoded.contains(removed), "{removed} leaked into schema");
    }
    assert!(encoded.contains("guest_type = \"virtualized\""));
    assert!(encoded.contains("passthrough = []"));
    assert!(encoded.contains("disabled = []"));
}

#[test]
fn menuconfig_schema_exposes_only_structured_device_selectors() {
    let schema = schemars::schema_for!(GuestConfig);
    let definitions = schema
        .as_value()
        .get("$defs")
        .and_then(|value| value.as_object())
        .unwrap();
    let device_properties = definitions["GuestDevices"]
        .get("properties")
        .and_then(|value| value.as_object())
        .unwrap();
    assert_eq!(device_properties.len(), 3);
    assert!(device_properties.contains_key("disabled"));
    assert!(device_properties.contains_key("passthrough"));
    assert!(device_properties.contains_key("virtual"));

    let base_properties = definitions["VMBaseConfig"]
        .get("properties")
        .and_then(|value| value.as_object())
        .unwrap();
    assert!(base_properties.contains_key("guest_type"));
    assert!(!base_properties.contains_key("vm_type"));

    let root_properties = schema
        .as_value()
        .get("properties")
        .and_then(|value| value.as_object())
        .unwrap();
    assert_eq!(root_properties.len(), 3);
    assert!(root_properties.contains_key("base"));
    assert!(root_properties.contains_key("devices"));
    assert!(root_properties.contains_key("kernel"));
}

#[test]
fn boot_config_validation_preserves_typed_errors() {
    let direct_with_bios = VMKernelConfig {
        enable_bios: true,
        boot_protocol: Some(VMBootProtocol::Direct),
        ..Default::default()
    };
    assert_eq!(
        direct_with_bios.validate_boot_config(),
        Err(AxVmConfigError::BootProtocolConflict {
            protocol: VMBootProtocol::Direct,
            enable_bios: true,
        })
    );

    let uefi_without_firmware = VMKernelConfig {
        enable_bios: true,
        boot_protocol: Some(VMBootProtocol::Uefi),
        bios_load_addr: Some(0xffc0_0000),
        ..Default::default()
    };
    assert_eq!(
        uefi_without_firmware.validate_boot_config_for_arch("x86_64"),
        Err(AxVmConfigError::MissingFirmwarePath {
            protocol: VMBootProtocol::Uefi,
        })
    );
}

#[test]
fn rejects_invalid_toml_with_public_error() {
    let result = GuestConfig::from_toml("[base");
    assert!(matches!(result, Err(AxVmConfigError::TomlParse { .. })));
}
