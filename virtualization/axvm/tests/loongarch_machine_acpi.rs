use axvm::machine::{
    AddressRange, DeviceInstanceId, DeviceModelId, DeviceRequirements, GuestMemoryRegion,
    HostDeviceDescriptor, HostDeviceId, HostDeviceOwnership, HostInterruptResource,
    HostPlatformSnapshot, InterruptControllerProfile, InterruptSourceKind,
    LoongArchAcpiInterruptRouting, LoongArchFirmwareDevicesProfile, LoongArchInterruptProfile,
    LoongArchInterruptRouting, LoongArchPciProfile, LoongArchPlatformProfile,
    LoongArchPowerProfile, MachineProfile, ResourceSlot, VirtualDeviceDescriptor, VmMachinePlanner,
    VmMachineRequest, generate_loongarch_fw_cfg_acpi,
};
use axvm_types::{GuestFirmwareKind, InterruptTriggerMode, VmMachineMode};

#[test]
fn generated_fw_cfg_acpi_uses_planned_serial_and_pch_pic() {
    let profile = loongarch_profile();
    let request = VmMachineRequest::new(VmMachineMode::Virtual, GuestFirmwareKind::Acpi)
        .with_vcpu_count(2)
        .with_memory(GuestMemoryRegion::new(
            AddressRange::new(0x8000_0000, 0x2000_0000).unwrap(),
        ))
        .with_virtual_device(ns16550());
    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &HostPlatformSnapshot::new(0))
        .unwrap();

    let files = generate_loongarch_fw_cfg_acpi(&plan, 2).unwrap();

    assert_eq!(&files.rsdp()[..8], b"RSD PTR ");
    assert_eq!(checksum(&files.rsdp()[..20]), 0);
    assert_eq!(checksum(files.rsdp()), 0);
    assert_eq!(files.loader().len() % 128, 0);

    let tables = split_tables(files.tables());
    for signature in [
        *b"DSDT", *b"FACP", *b"APIC", *b"SRAT", *b"SPCR", *b"MCFG", *b"XSDT",
    ] {
        let table = tables
            .iter()
            .find(|table| table[..4] == signature)
            .unwrap_or_else(|| panic!("missing {}", core::str::from_utf8(&signature).unwrap()));
        assert_eq!(checksum(table), 0);
    }
    let dsdt = tables.iter().find(|table| table[..4] == *b"DSDT").unwrap();
    assert!(dsdt.windows(4).any(|bytes| bytes == b"COMA"));
    assert!(
        dsdt.windows(4)
            .any(|bytes| bytes == 0x1fe0_0000u32.to_le_bytes())
    );
    let madt = tables.iter().find(|table| table[..4] == *b"APIC").unwrap();
    assert!(
        madt.windows(8)
            .any(|bytes| bytes == 0x1000_0000u64.to_le_bytes())
    );
}

#[test]
fn passthrough_device_aml_uses_complete_acpi_route() {
    let device_range = AddressRange::new(0x1f00_0000, 0x1000).unwrap();
    let route = irq_framework::AcpiGsiRoute {
        gsi: 0x51,
        vector: 0x51,
        controller: irq_framework::AcpiGsiController::PchPic,
        controller_id: 1,
        controller_address: 0x1000_0000,
        controller_input: 0x11,
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
            .with_compatible("ACME0002")
            .with_mmio(device_range)
            .with_interrupt(HostInterruptResource::acpi(route)),
        );
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Acpi)
        .with_vcpu_count(1)
        .with_memory(GuestMemoryRegion::new(
            AddressRange::new(0x8000_0000, 0x2000_0000).unwrap(),
        ));
    let plan = VmMachinePlanner::new(loongarch_profile())
        .plan(&request, &snapshot)
        .unwrap();

    let files = generate_loongarch_fw_cfg_acpi(&plan, 1).unwrap();
    let tables = split_tables(files.tables());
    let dsdt = tables.iter().find(|table| table[..4] == *b"DSDT").unwrap();

    assert!(dsdt.windows(4).any(|bytes| bytes == b"PT00"));
    assert!(
        dsdt.windows(8)
            .any(|bytes| bytes == device_range.base().to_le_bytes())
    );
    assert!(dsdt.windows(4).any(|bytes| bytes == 0x51u32.to_le_bytes()));
}

fn loongarch_profile() -> MachineProfile {
    MachineProfile::new(
        AddressRange::new(0x1fe0_0000, 0x0010_0000).unwrap(),
        1..=255,
    )
    .unwrap()
    .with_interrupt_controller(InterruptControllerProfile::LoongArch(
        LoongArchInterruptProfile::new(
            AddressRange::new(0x1000_0000, 0x1000).unwrap(),
            AddressRange::new(0x2ff0_0000, 0x1_0000).unwrap(),
            LoongArchInterruptRouting::new(
                3,
                0,
                0x20,
                0xe0,
                LoongArchAcpiInterruptRouting::new(0x40, 0x40, 0xc0),
            ),
        ),
    ))
    .with_loongarch_platform(LoongArchPlatformProfile::new(
        AddressRange::new(0x1e02_0000, 0x18).unwrap(),
        LoongArchPciProfile::new(
            AddressRange::new(0x2000_0000, 0x0800_0000).unwrap(),
            AddressRange::new(0x4000_0000, 0x4000_0000).unwrap(),
            AddressRange::new(0x1800_0000, 0x1_0000).unwrap(),
            16,
        ),
        LoongArchPowerProfile::new(
            0x100e_001e,
            0x42,
            0x100e_001c,
            0x34,
            0x100e_001c,
            0x100e_001d,
        ),
        LoongArchFirmwareDevicesProfile::new(
            AddressRange::new(0x100d_0100, 0x100).unwrap(),
            6,
            [
                AddressRange::new(0x1c00_0000, 0x0100_0000).unwrap(),
                AddressRange::new(0x1d00_0000, 0x0100_0000).unwrap(),
            ],
            4,
        ),
    ))
}

fn split_tables(mut bytes: &[u8]) -> Vec<&[u8]> {
    let facs_length = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    bytes = &bytes[facs_length..];
    let mut tables = Vec::new();
    while !bytes.is_empty() {
        let length = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        tables.push(&bytes[..length]);
        bytes = &bytes[length..];
    }
    tables
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |sum, byte| sum.wrapping_add(*byte))
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
                axdevice::InterruptSharing::Exclusive,
            )
            .unwrap(),
    )
}
