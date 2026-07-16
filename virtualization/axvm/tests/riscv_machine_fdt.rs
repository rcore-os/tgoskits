use axvm::machine::{
    AddressRange, DeviceInstanceId, DeviceModelId, DeviceRequirements, GuestMemoryRegion,
    HostPlatformSnapshot, InterruptControllerProfile, InterruptSourceKind, MachineProfile,
    ResourceSlot, RiscvFdtConfig, RiscvPlicProfile, VirtualDeviceDescriptor, VmMachinePlanner,
    VmMachineRequest, generate_riscv_fdt,
};
use axvm_types::{GuestFirmwareKind, InterruptTriggerMode, VmMachineMode};
use fdt_edit::Fdt;

#[test]
fn generated_fdt_uses_planned_plic_and_uart_resources() {
    let profile = MachineProfile::new(
        AddressRange::new(0x1000_0000, 0x1000_0000).unwrap(),
        1..=1023,
    )
    .unwrap()
    .with_interrupt_controller(InterruptControllerProfile::RiscvPlic(
        RiscvPlicProfile::new(AddressRange::new(0x0c00_0000, 0x0060_0000).unwrap(), 1023).unwrap(),
    ));
    let request = VmMachineRequest::new(VmMachineMode::Virtual, GuestFirmwareKind::Fdt)
        .with_vcpu_count(2)
        .with_memory(GuestMemoryRegion::new(
            AddressRange::new(0x8000_0000, 0x2000_0000).unwrap(),
        ))
        .with_virtual_device(ns16550());
    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &HostPlatformSnapshot::new(0))
        .unwrap();
    let config = RiscvFdtConfig::new(2)
        .unwrap()
        .with_bootargs("console=ttyS0");

    let bytes = generate_riscv_fdt(&plan, &config).unwrap();

    let fdt = Fdt::from_bytes(&bytes).unwrap();
    let uart = fdt.get_by_path("/soc/serial@10000000").unwrap();
    assert_eq!(uart.regs()[0].address, 0x1000_0000);
    assert_eq!(uart.regs()[0].size, Some(0x1000));
    assert_eq!(
        uart.as_node().get_property("interrupts").unwrap().get_u32(),
        Some(1)
    );
    let plic = fdt
        .get_by_path("/soc/interrupt-controller@c000000")
        .unwrap();
    assert_eq!(
        plic.as_node().get_property("riscv,ndev").unwrap().get_u32(),
        Some(1023)
    );
    assert_eq!(
        plic.as_node()
            .get_property("interrupts-extended")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>(),
        [0x100, 11, 0x100, 9, 0x101, 11, 0x101, 9]
    );
    assert_eq!(
        fdt.get_by_path("/aliases")
            .unwrap()
            .as_node()
            .get_property("serial0")
            .unwrap()
            .as_str(),
        Some("/soc/serial@10000000")
    );
}

fn ns16550() -> VirtualDeviceDescriptor {
    VirtualDeviceDescriptor::new(
        DeviceInstanceId::new("console0").unwrap(),
        DeviceModelId::new("ns16550a").unwrap(),
        DeviceRequirements::new()
            .with_mmio(ResourceSlot::new("registers").unwrap(), 0x1000, 0x1000)
            .unwrap()
            .with_wired_irq(
                ResourceSlot::new("irq").unwrap(),
                InterruptTriggerMode::LevelTriggered,
                InterruptSourceKind::Software,
            )
            .unwrap(),
    )
    .with_compatible("ns16550a")
}
