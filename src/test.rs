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
