use axvm::machine::{
    AddressRange, DeviceInstanceId, DeviceModelId, DeviceRequirements, GuestMemoryRegion,
    HostDeviceDescriptor, HostDeviceId, HostDeviceOwnership, HostInterruptResource,
    HostPlatformSnapshot, InterruptControllerProfile, InterruptSourceKind, IoPortRange,
    MachineProfile, ResourceSlot, VirtualDeviceDescriptor, VmMachinePlanner, VmMachineRequest,
    X86AcpiConfig, X86ApicProfile, generate_x86_acpi,
};
use axvm_types::{GuestFirmwareKind, InterruptTriggerMode, VmMachineMode};

#[test]
fn generated_acpi_uses_planned_ioapic_and_com1_resources() {
    let profile = MachineProfile::new(AddressRange::new(0x1000_0000, 0x1000_0000).unwrap(), 4..=23)
        .unwrap()
        .with_pio_pool(IoPortRange::new(0x3f8, 8).unwrap())
        .with_interrupt_controller(InterruptControllerProfile::X86Apic(X86ApicProfile::new(
            AddressRange::new(0xfee0_0000, 0x1000).unwrap(),
            AddressRange::new(0xfec0_0000, 0x1000).unwrap(),
        )));
    let request = VmMachineRequest::new(VmMachineMode::Virtual, GuestFirmwareKind::Acpi)
        .with_vcpu_count(2)
        .with_memory(GuestMemoryRegion::new(
            AddressRange::new(0, 0x4000_0000).unwrap(),
        ))
        .with_virtual_device(com1());
    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &HostPlatformSnapshot::new(0))
        .unwrap();

    let image = generate_x86_acpi(&plan, &X86AcpiConfig::new(2, 0x000e_0000).unwrap()).unwrap();

    assert_eq!(image.rsdp_address(), 0x000e_0000);
    assert_eq!(&image.bytes()[..8], b"RSD PTR ");
    assert_eq!(checksum(image.bytes().get(..36).unwrap()), 0);
    assert_eq!(
        read_u64(image.bytes(), 24),
        image.table_address(*b"XSDT").unwrap()
    );

    let madt = image.table(*b"APIC").unwrap();
    assert_eq!(checksum(madt), 0);
    assert!(
        madt.windows(4)
            .any(|bytes| bytes == 0xfec0_0000u32.to_le_bytes())
    );
    let dsdt = image.table(*b"DSDT").unwrap();
    assert_eq!(checksum(dsdt), 0);
    assert!(dsdt.windows(4).any(|bytes| bytes == b"COM1"));
    let spcr = image.table(*b"SPCR").unwrap();
    assert_eq!(checksum(spcr), 0);
    assert!(spcr.windows(2).any(|bytes| bytes == 0x3f8u16.to_le_bytes()));
}

#[test]
fn passthrough_device_aml_uses_guest_gsi_and_host_route_polarity() {
    let profile = x86_profile();
    let device_range = AddressRange::new(0xfedc_0000, 0x1000).unwrap();
    let route = irq_framework::AcpiGsiRoute {
        gsi: 17,
        vector: 49,
        controller: irq_framework::AcpiGsiController::IoApic,
        controller_id: 0,
        controller_address: 0xfec0_0000,
        controller_input: 17,
        trigger: irq_framework::AcpiIrqTrigger::Level,
        polarity: irq_framework::AcpiIrqPolarity::ActiveLow,
    };
    let snapshot = HostPlatformSnapshot::new(1)
        .with_io_aperture(device_range)
        .with_device(
            HostDeviceDescriptor::new(
                HostDeviceId::new("\\_SB.PTST").unwrap(),
                HostDeviceOwnership::Assignable,
            )
            .with_compatible("ACME0001")
            .with_mmio(device_range)
            .with_interrupt(HostInterruptResource::routed_acpi(18, route)),
        );
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Acpi)
        .with_vcpu_count(1)
        .with_memory(GuestMemoryRegion::new(
            AddressRange::new(0, 0x4000_0000).unwrap(),
        ));
    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();

    let image = generate_x86_acpi(&plan, &X86AcpiConfig::new(1, 0x000e_0000).unwrap()).unwrap();
    let dsdt = image.table(*b"DSDT").unwrap();

    assert!(dsdt.windows(4).any(|bytes| bytes == b"PT00"));
    assert!(
        dsdt.windows(8)
            .any(|bytes| bytes == device_range.base().to_le_bytes())
    );
    assert!(dsdt.windows(4).any(|bytes| bytes == 18u32.to_le_bytes()));
}

fn x86_profile() -> MachineProfile {
    MachineProfile::new(AddressRange::new(0x1000_0000, 0x1000_0000).unwrap(), 4..=23)
        .unwrap()
        .with_pio_pool(IoPortRange::new(0x3f8, 8).unwrap())
        .with_interrupt_controller(InterruptControllerProfile::X86Apic(X86ApicProfile::new(
            AddressRange::new(0xfee0_0000, 0x1000).unwrap(),
            AddressRange::new(0xfec0_0000, 0x1000).unwrap(),
        )))
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |sum, byte| sum.wrapping_add(*byte))
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn com1() -> VirtualDeviceDescriptor {
    VirtualDeviceDescriptor::new(
        DeviceInstanceId::new("console0").unwrap(),
        DeviceModelId::new("x86-com1").unwrap(),
        DeviceRequirements::new()
            .with_pio(ResourceSlot::new("registers").unwrap(), 8, 8)
            .unwrap()
            .with_wired_irq(
                ResourceSlot::new("irq").unwrap(),
                InterruptTriggerMode::LevelTriggered,
                InterruptSourceKind::Software,
            )
            .unwrap(),
    )
}
