use axvm::machine::{
    Aarch64FdtConfig, Aarch64GicV3Profile, AddressRange, DeviceInstanceId, DeviceModelId,
    DeviceRequirements, GuestMemoryRegion, HostPlatformSnapshot, InterruptControllerProfile,
    MachineProfile, ResourceSlot, VirtualDeviceDescriptor, VmMachinePlanner, VmMachineRequest,
    generate_aarch64_fdt,
};
use axvm_types::{GuestFirmwareKind, InterruptTriggerMode, VmMachineMode};
use fdt_edit::Fdt;

#[test]
fn generated_fdt_uses_the_planned_pl011_resources() {
    let controller = Aarch64GicV3Profile::new(
        AddressRange::new(0x0800_0000, 0x1_0000).unwrap(),
        0x080a_0000,
        0x2_0000,
        Some(AddressRange::new(0x0808_0000, 0x2_0000).unwrap()),
        480,
    )
    .unwrap();
    let profile = MachineProfile::new(
        AddressRange::new(0x0900_0000, 0x0100_0000).unwrap(),
        32..=511,
    )
    .unwrap()
    .with_interrupt_controller(InterruptControllerProfile::Aarch64GicV3(controller));
    let request = VmMachineRequest::new(VmMachineMode::Virtual, GuestFirmwareKind::Fdt)
        .with_vcpu_count(2)
        .with_memory(GuestMemoryRegion::new(
            AddressRange::new(0x4000_0000, 0x1000_0000).unwrap(),
        ))
        .with_virtual_device(pl011());
    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &HostPlatformSnapshot::new(0))
        .unwrap();
    let config = Aarch64FdtConfig::new(2)
        .unwrap()
        .with_bootargs("console=ttyAMA0");

    let bytes = generate_aarch64_fdt(&plan, &config).unwrap();

    let fdt = Fdt::from_bytes(&bytes).unwrap();
    let serial = fdt.get_by_path("/pl011@9000000").unwrap();
    let registers = serial.regs();
    assert_eq!(registers.len(), 1);
    assert_eq!(registers[0].address, 0x0900_0000);
    assert_eq!(registers[0].size, Some(0x1000));
    assert_eq!(
        serial
            .as_node()
            .get_property("interrupts")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>(),
        [0, 0, 4]
    );
    assert_eq!(
        fdt.get_by_path("/aliases")
            .unwrap()
            .as_node()
            .get_property("serial0")
            .unwrap()
            .as_str(),
        Some("/pl011@9000000")
    );
    assert_eq!(
        fdt.get_by_path("/memory@40000000").unwrap().regs()[0].size,
        Some(0x1000_0000)
    );
    let timer = fdt.get_by_path("/timer").unwrap();
    assert_eq!(
        timer
            .as_node()
            .get_property("interrupts")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>(),
        [1, 13, 4, 1, 14, 4]
    );
    assert!(timer.as_node().get_property("interrupt-names").is_none());
}

fn pl011() -> VirtualDeviceDescriptor {
    VirtualDeviceDescriptor::new(
        DeviceInstanceId::new("console0").unwrap(),
        DeviceModelId::new("arm-pl011").unwrap(),
        DeviceRequirements::new()
            .with_mmio(ResourceSlot::new("registers").unwrap(), 0x1000, 0x1000)
            .unwrap()
            .with_wired_irq(
                ResourceSlot::new("irq").unwrap(),
                InterruptTriggerMode::LevelTriggered,
                axdevice::InterruptSharing::Exclusive,
            )
            .unwrap(),
    )
    .with_compatible("arm,pl011")
}
