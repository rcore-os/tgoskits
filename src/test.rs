use crate::AxVMCrateConfig;
use crate::EmulatedDeviceType;
use crate::VMDevicesConfig;
use crate::VMInterruptMode;
use crate::VmMemMappingType;
use enumerable::Enumerable;

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
fn test_emu_dev_type_from_usize() {
    for emu_dev_type in EmulatedDeviceType::enumerator() {
        let converted = EmulatedDeviceType::from_usize(emu_dev_type as usize);
        assert_eq!(
            converted, emu_dev_type,
            "Value mismatch after bidirectional conversion: {:?} -> {:?}",
            emu_dev_type, converted
        );
    }
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
    assert!(!EmulatedDeviceType::IVCChannel.removable());
    assert!(!EmulatedDeviceType::GPPTDistributor.removable());
    assert!(!EmulatedDeviceType::GPPTITS.removable());
}

#[test]
fn test_emulated_device_type_display() {
    use crate::EmulatedDeviceType;
    use alloc::format;

    assert_eq!(format!("{}", EmulatedDeviceType::Dummy), "meta device");
    assert_eq!(
        format!("{}", EmulatedDeviceType::InterruptController),
        "interrupt controller"
    );
    assert_eq!(format!("{}", EmulatedDeviceType::Console), "console");
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
    assert!(vm_kernel_config.bios_path.is_none());
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
