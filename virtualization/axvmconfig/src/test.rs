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

use crate::{
    AxVMCrateConfig, EmulatedDeviceType, VMBootProtocol, VMDevicesConfig, VMInterruptMode,
    VmMemMappingType,
};

#[test]
fn test_config_deser() {
    const EXAMPLE_CONFIG: &str = r#"
[base]
id = 12
name = "test_vm"
vm_type = 1
cpu_num = 2
phys_cpu_sets = [3, 4]
phys_cpu_ids = [0x500, 0x501]

[kernel]
entry_point = 0xdeadbeef
image_location = "memory"
kernel_path = "amazing-os.bin"
kernel_load_addr = 0xdeadbeef
enable_bios = true
bios_path = "test-bios.bin"
bios_load_addr = 0x8000
dtb_path = "impressive-board.dtb"
dtb_load_addr = 0xa0000000

memory_regions = [
    [0x8000_0000, 0x8000_0000, 0x7, 1],
]

[devices]
passthrough_devices = [
    ["dev0", 0x0, 0x0, 0x0800_0000, 0x1],
    ["dev1", 0x0900_0000, 0x0900_0000, 0x0a00_0000, 0x2],
]

passthrough_ports = [
    [0x6000, 0x80],
]

emu_devices = [
    ["dev2", 0x0800_0000, 0x1_0000, 0, 0x21, []],
    ["dev3", 0x0808_0000, 0x1_0000, 0, 0x22, []],
]

interrupt_mode = "passthrough"
    "#;

    let config: AxVMCrateConfig = toml::from_str(EXAMPLE_CONFIG).unwrap();

    assert_eq!(config.base.id, 12);
    assert_eq!(config.base.name, "test_vm");
    assert_eq!(config.base.vm_type, 1);
    assert_eq!(config.base.cpu_num, 2);
    assert_eq!(config.base.phys_cpu_ids, Some(vec![0x500, 0x501]));
    assert_eq!(config.base.phys_cpu_sets, Some(vec![3, 4]));

    assert_eq!(config.kernel.entry_point, 0xdeadbeef);
    assert_eq!(config.kernel.image_location, Some("memory".to_string()));
    assert_eq!(config.kernel.kernel_path, "amazing-os.bin");
    assert_eq!(config.kernel.kernel_load_addr, 0xdeadbeef);
    assert!(config.kernel.enable_bios);
    assert_eq!(
        config.kernel.effective_boot_protocol(),
        VMBootProtocol::Multiboot
    );
    assert_eq!(config.kernel.bios_path, Some("test-bios.bin".to_string()));
    assert_eq!(config.kernel.bios_load_addr, Some(0x8000));
    assert_eq!(
        config.kernel.dtb_path,
        Some("impressive-board.dtb".to_string())
    );
    assert_eq!(config.kernel.dtb_load_addr, Some(0xa000_0000));
    assert_eq!(config.kernel.memory_regions.len(), 1);
    assert_eq!(config.kernel.memory_regions[0].gpa, 0x8000_0000);
    assert_eq!(config.kernel.memory_regions[0].size, 0x8000_0000);
    assert_eq!(config.kernel.memory_regions[0].flags, 0x7);
    assert_eq!(
        config.kernel.memory_regions[0].map_type,
        VmMemMappingType::MapIdentical
    );

    assert_eq!(config.devices.passthrough_devices.len(), 2);
    assert_eq!(config.devices.passthrough_devices[0].name, "dev0");
    assert_eq!(config.devices.passthrough_devices[0].base_gpa, 0x0);
    assert_eq!(config.devices.passthrough_devices[0].base_hpa, 0x0);
    assert_eq!(config.devices.passthrough_devices[0].length, 0x0800_0000);
    assert_eq!(config.devices.passthrough_devices[0].irq_id, 1);
    assert_eq!(config.devices.passthrough_devices[1].name, "dev1");
    assert_eq!(config.devices.passthrough_devices[1].base_gpa, 0x0900_0000);
    assert_eq!(config.devices.passthrough_devices[1].base_hpa, 0x0900_0000);
    assert_eq!(config.devices.passthrough_devices[1].length, 0x0a00_0000);
    assert_eq!(config.devices.passthrough_devices[1].irq_id, 2);
    assert_eq!(config.devices.passthrough_ports.len(), 1);
    assert_eq!(config.devices.passthrough_ports[0].base, 0x6000);
    assert_eq!(config.devices.passthrough_ports[0].length, 0x80);
    assert_eq!(config.devices.emu_devices.len(), 2);
    assert_eq!(config.devices.emu_devices[0].name, "dev2");
    assert_eq!(config.devices.emu_devices[0].base_gpa, 0x0800_0000);
    assert_eq!(config.devices.emu_devices[0].length, 0x1_0000);
    assert_eq!(config.devices.emu_devices[0].irq_id, 0);
    assert_eq!(
        config.devices.emu_devices[0].emu_type,
        EmulatedDeviceType::GPPTDistributor
    );
    assert_eq!(config.devices.emu_devices[1].name, "dev3");
    assert_eq!(config.devices.emu_devices[1].base_gpa, 0x0808_0000);
    assert_eq!(config.devices.emu_devices[1].length, 0x1_0000);
    assert_eq!(config.devices.emu_devices[1].irq_id, 0);
    assert_eq!(
        config.devices.emu_devices[1].emu_type,
        EmulatedDeviceType::GPPTITS
    );
    assert_eq!(config.devices.interrupt_mode, VMInterruptMode::Passthrough);
}

#[test]
fn test_boot_protocol_deser_and_legacy_defaults() {
    const UEFI_KERNEL_CONFIG: &str = r#"
entry_point = 0xffff_fff0
image_location = "fs"
kernel_path = "guest-loader.efi"
kernel_load_addr = 0x20_0000
enable_bios = true
boot_protocol = "uefi"
uefi_firmware_path = "OVMF_CODE.fd"
bios_path = "legacy-OVMF_CODE.fd"
bios_load_addr = 0xffc0_0000
memory_regions = []
    "#;

    let uefi_config: crate::VMKernelConfig = toml::from_str(UEFI_KERNEL_CONFIG).unwrap();
    assert_eq!(uefi_config.boot_protocol, Some(VMBootProtocol::Uefi));
    assert_eq!(uefi_config.effective_boot_protocol(), VMBootProtocol::Uefi);
    assert_eq!(uefi_config.boot_firmware_path(), Some("OVMF_CODE.fd"));
    assert!(uefi_config.validate_boot_config().is_ok());

    let legacy_uefi_config = crate::VMKernelConfig {
        enable_bios: true,
        boot_protocol: Some(VMBootProtocol::Uefi),
        bios_path: Some("OVMF_CODE.fd".to_string()),
        bios_load_addr: Some(0xffc0_0000),
        ..Default::default()
    };
    assert_eq!(
        legacy_uefi_config.boot_firmware_path(),
        Some("OVMF_CODE.fd")
    );
    assert!(legacy_uefi_config.validate_boot_config().is_ok());

    let direct_config = crate::VMKernelConfig {
        enable_bios: false,
        ..Default::default()
    };
    assert_eq!(
        direct_config.effective_boot_protocol(),
        VMBootProtocol::Direct
    );

    let legacy_bios_config = crate::VMKernelConfig {
        enable_bios: true,
        ..Default::default()
    };
    assert_eq!(
        legacy_bios_config.effective_boot_protocol(),
        VMBootProtocol::Multiboot
    );
}

#[test]
fn test_boot_config_validation_requires_uefi_inputs() {
    let mut missing_firmware = crate::VMKernelConfig {
        enable_bios: true,
        boot_protocol: Some(VMBootProtocol::Uefi),
        bios_load_addr: Some(0xffc0_0000),
        ..Default::default()
    };

    assert!(missing_firmware.validate_boot_config().is_err());

    missing_firmware.uefi_firmware_path = Some("OVMF_CODE.fd".to_string());
    assert!(missing_firmware.validate_boot_config().is_ok());
}

#[test]
fn test_boot_config_validation_rejects_unsupported_firmware_protocols_on_other_arches() {
    let uefi_config = crate::VMKernelConfig {
        enable_bios: true,
        boot_protocol: Some(VMBootProtocol::Uefi),
        uefi_firmware_path: Some("OVMF_CODE.fd".to_string()),
        bios_load_addr: Some(0xffc0_0000),
        ..Default::default()
    };

    assert!(
        uefi_config
            .validate_boot_config_for_arch("aarch64")
            .is_err()
    );
    assert!(
        uefi_config
            .validate_boot_config_for_arch("riscv64")
            .is_err()
    );
    assert!(
        uefi_config
            .validate_boot_config_for_arch("loongarch64")
            .is_ok()
    );
    assert!(uefi_config.validate_boot_config_for_arch("x86_64").is_ok());

    let multiboot_config = crate::VMKernelConfig {
        enable_bios: true,
        boot_protocol: Some(VMBootProtocol::Multiboot),
        ..Default::default()
    };

    assert!(
        multiboot_config
            .validate_boot_config_for_arch("aarch64")
            .is_err()
    );
    assert!(
        multiboot_config
            .validate_boot_config_for_arch("x86_64")
            .is_ok()
    );
}

#[test]
fn test_boot_config_validation_rejects_direct_bios_mix() {
    let direct_with_bios = crate::VMKernelConfig {
        enable_bios: true,
        boot_protocol: Some(VMBootProtocol::Direct),
        bios_load_addr: Some(0x8000),
        ..Default::default()
    };

    assert!(direct_with_bios.validate_boot_config().is_err());
}

#[test]
fn test_emu_dev_type_from_usize() {
    for emu_dev_type in EmulatedDeviceType::all() {
        let converted = EmulatedDeviceType::from_usize(*emu_dev_type as usize).unwrap();
        assert_eq!(
            converted, *emu_dev_type,
            "Value mismatch after bidirectional conversion: {:?} -> {:?}",
            emu_dev_type, converted
        );
    }

    assert_eq!(
        EmulatedDeviceType::from_usize(0x3),
        Some(EmulatedDeviceType::FwCfg)
    );
}

#[test]
fn test_rejects_unknown_emulated_device_type() {
    const EXAMPLE_DEVICE_CONFIG: &str = r#"
passthrough_devices = []
emu_devices = [
    ["bad-device", 0x1000, 0x1000, 0, 0x4, []],
]
    "#;

    assert!(toml::from_str::<VMDevicesConfig>(EXAMPLE_DEVICE_CONFIG).is_err());
}

#[test]
fn test_rejects_incomplete_explicit_passthrough_device() {
    const EXAMPLE_DEVICE_CONFIG: &str = r#"
passthrough_devices = [
    ["bad-device", 0x1000, 0x1000],
]
emu_devices = []
    "#;

    assert!(toml::from_str::<VMDevicesConfig>(EXAMPLE_DEVICE_CONFIG).is_err());

    const FDT_PATH_DEVICE_CONFIG: &str = r#"
passthrough_devices = [
    ["/"],
]
emu_devices = []
    "#;

    let config: VMDevicesConfig = toml::from_str(FDT_PATH_DEVICE_CONFIG).unwrap();
    assert_eq!(config.passthrough_devices.len(), 1);
    assert_eq!(config.passthrough_devices[0].name, "/");
    assert_eq!(config.passthrough_devices[0].length, 0);
}

#[test]
fn test_interrupt_mode_deser() {
    const EXAMPLE_DEVICE_CONFIG: &str = r#"
passthrough_devices = []
emu_devices = []
    "#;

    let device_config: VMDevicesConfig = toml::from_str(EXAMPLE_DEVICE_CONFIG).unwrap();
    assert_eq!(device_config.interrupt_mode, VMInterruptMode::default());

    fn test_deser(s: &str, expected: VMInterruptMode) {
        let config_str = format!(
            "{}{}",
            EXAMPLE_DEVICE_CONFIG,
            format!("interrupt_mode = \"{}\"", s)
        );
        let device_config: VMDevicesConfig = toml::from_str(&config_str).unwrap();
        assert_eq!(device_config.interrupt_mode, expected);
    }

    test_deser("emulated", VMInterruptMode::Emulated);
    test_deser("emu", VMInterruptMode::Emulated);
    test_deser("passthrough", VMInterruptMode::Passthrough);
    test_deser("pt", VMInterruptMode::Passthrough);
    test_deser("no_irq", VMInterruptMode::NoIrq);
    test_deser("no", VMInterruptMode::NoIrq);
    test_deser("none", VMInterruptMode::NoIrq);
}

#[test]
fn test_vmtype_enum() {
    use crate::VMType;

    assert_eq!(VMType::default(), VMType::VMTRTOS);

    assert_eq!(VMType::from(0), VMType::VMTHostVM);
    assert_eq!(VMType::from(1), VMType::VMTRTOS);
    assert_eq!(VMType::from(2), VMType::VMTLinux);
    assert_eq!(VMType::from(999), VMType::VMTRTOS);

    assert_eq!(usize::from(VMType::VMTHostVM), 0);
    assert_eq!(usize::from(VMType::VMTRTOS), 1);
    assert_eq!(usize::from(VMType::VMTLinux), 2);
}

#[test]
fn test_vm_mem_mapping_type() {
    use crate::VmMemMappingType;

    assert_eq!(VmMemMappingType::default(), VmMemMappingType::MapAlloc);

    let alloc_type = VmMemMappingType::MapAlloc;
    let identical_type = VmMemMappingType::MapIdentical;

    assert_eq!(alloc_type as u8, 0);
    assert_eq!(identical_type as u8, 1);
}

#[test]
fn test_emulated_device_type_removable() {
    use crate::EmulatedDeviceType;

    assert!(EmulatedDeviceType::InterruptController.removable());
    assert!(EmulatedDeviceType::GPPTRedistributor.removable());
    assert!(EmulatedDeviceType::VirtioBlk.removable());
    assert!(EmulatedDeviceType::VirtioNet.removable());
    assert!(EmulatedDeviceType::VirtioConsole.removable());

    assert!(!EmulatedDeviceType::Dummy.removable());
    assert!(!EmulatedDeviceType::Console.removable());
    assert!(!EmulatedDeviceType::FwCfg.removable());
    assert!(!EmulatedDeviceType::IVCChannel.removable());
    assert!(!EmulatedDeviceType::GPPTDistributor.removable());
    assert!(!EmulatedDeviceType::GPPTITS.removable());
}

#[test]
fn test_emulated_device_type_display() {
    use alloc::format;

    use crate::EmulatedDeviceType;

    assert_eq!(format!("{}", EmulatedDeviceType::Dummy), "meta device");
    assert_eq!(
        format!("{}", EmulatedDeviceType::InterruptController),
        "interrupt controller"
    );
    assert_eq!(format!("{}", EmulatedDeviceType::Console), "console");
    assert_eq!(format!("{}", EmulatedDeviceType::FwCfg), "fw_cfg");
    assert_eq!(format!("{}", EmulatedDeviceType::IVCChannel), "ivc channel");
    assert_eq!(
        format!("{}", EmulatedDeviceType::GPPTRedistributor),
        "gic partial passthrough redistributor"
    );
    assert_eq!(
        format!("{}", EmulatedDeviceType::GPPTDistributor),
        "gic partial passthrough distributor"
    );
    assert_eq!(
        format!("{}", EmulatedDeviceType::GPPTITS),
        "gic partial passthrough its"
    );
    assert_eq!(format!("{}", EmulatedDeviceType::VirtioBlk), "virtio block");
    assert_eq!(format!("{}", EmulatedDeviceType::VirtioNet), "virtio net");
    assert_eq!(
        format!("{}", EmulatedDeviceType::VirtioConsole),
        "virtio console"
    );
}

#[test]
fn test_config_from_toml_error_handling() {
    use crate::AxVMCrateConfig;

    let invalid_toml = r#"
[base
id = "invalid"
    "#;

    let result = AxVMCrateConfig::from_toml(invalid_toml);
    assert!(result.is_err());

    let invalid_data_type = r#"
[base]
id = "not_a_number"
name = "test"
vm_type = 1
cpu_num = 1
    "#;

    let result = AxVMCrateConfig::from_toml(invalid_data_type);
    assert!(result.is_err());
}

#[test]
fn test_default_implementations() {
    use crate::*;

    assert_eq!(VMType::default(), VMType::VMTRTOS);
    assert_eq!(VmMemMappingType::default(), VmMemMappingType::MapAlloc);
    assert_eq!(EmulatedDeviceType::default(), EmulatedDeviceType::Dummy);
    assert_eq!(VMInterruptMode::default(), VMInterruptMode::NoIrq);

    let vm_mem_config = VmMemConfig::default();
    assert_eq!(vm_mem_config.gpa, 0);
    assert_eq!(vm_mem_config.size, 0);
    assert_eq!(vm_mem_config.flags, 0);
    assert_eq!(vm_mem_config.map_type, VmMemMappingType::MapAlloc);

    let emu_device_config = EmulatedDeviceConfig::default();
    assert_eq!(emu_device_config.name, "");
    assert_eq!(emu_device_config.base_gpa, 0);
    assert_eq!(emu_device_config.length, 0);
    assert_eq!(emu_device_config.irq_id, 0);
    assert_eq!(emu_device_config.emu_type, EmulatedDeviceType::Dummy);
    assert!(emu_device_config.cfg_list.is_empty());

    let passthrough_device_config = PassThroughDeviceConfig::default();
    assert_eq!(passthrough_device_config.name, "");
    assert_eq!(passthrough_device_config.base_gpa, 0);
    assert_eq!(passthrough_device_config.base_hpa, 0);
    assert_eq!(passthrough_device_config.length, 0);
    assert_eq!(passthrough_device_config.irq_id, 0);

    let vm_base_config = VMBaseConfig::default();
    assert_eq!(vm_base_config.id, 0);
    assert_eq!(vm_base_config.name, "");
    assert_eq!(vm_base_config.vm_type, 0);
    assert_eq!(vm_base_config.cpu_num, 0);
    assert!(vm_base_config.phys_cpu_ids.is_none());
    assert!(vm_base_config.phys_cpu_sets.is_none());

    let vm_kernel_config = VMKernelConfig::default();
    assert_eq!(vm_kernel_config.entry_point, 0);
    assert_eq!(vm_kernel_config.kernel_path, "");
    assert_eq!(vm_kernel_config.kernel_load_addr, 0);
    assert!(!vm_kernel_config.enable_bios);
    assert!(vm_kernel_config.boot_protocol.is_none());
    assert_eq!(
        vm_kernel_config.effective_boot_protocol(),
        VMBootProtocol::Direct
    );
    assert!(vm_kernel_config.bios_path.is_none());
    assert!(vm_kernel_config.uefi_firmware_path.is_none());
    assert!(vm_kernel_config.bios_load_addr.is_none());
    assert!(vm_kernel_config.dtb_path.is_none());
    assert!(vm_kernel_config.dtb_load_addr.is_none());
    assert!(vm_kernel_config.ramdisk_path.is_none());
    assert!(vm_kernel_config.ramdisk_load_addr.is_none());
    assert!(vm_kernel_config.image_location.is_none());
    assert!(vm_kernel_config.cmdline.is_none());
    assert!(vm_kernel_config.disk_path.is_none());
    assert!(vm_kernel_config.memory_regions.is_empty());

    let vm_devices_config = VMDevicesConfig::default();
    assert!(vm_devices_config.emu_devices.is_empty());
    assert!(vm_devices_config.passthrough_devices.is_empty());
    assert_eq!(vm_devices_config.interrupt_mode, VMInterruptMode::NoIrq);

    let axvm_crate_config = AxVMCrateConfig::default();
    assert_eq!(axvm_crate_config.base.id, 0);
    assert_eq!(axvm_crate_config.kernel.entry_point, 0);
    assert!(axvm_crate_config.devices.emu_devices.is_empty());
}
