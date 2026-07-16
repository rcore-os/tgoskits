//! ACPI host inventory normalization shared by architecture adapters.

use alloc::{format, string::String, vec::Vec};

use rdrive::probe::acpi::{
    AcpiGsiRoute, AcpiIoApic, AcpiPchPic, AcpiPciEcam, AcpiResourceDevice, AcpiResourceRange,
};

use crate::machine::{
    AddressRange, HostDeviceDescriptor, HostDeviceId, HostDeviceOwnership, HostInterruptResource,
    HostPlatformSnapshot, IoPortRange, MachinePlanError, MachinePlanResult,
};

/// Captures the live ACPI namespace and fixed controller tables as one snapshot.
pub(crate) fn current_host_platform_snapshot() -> MachinePlanResult<HostPlatformSnapshot> {
    let inventory = rdrive::probe::acpi::with_acpi(capture_inventory).ok_or_else(|| {
        MachinePlanError::InvalidFirmware {
            detail: "host ACPI inventory is unavailable".into(),
        }
    })??;
    build_snapshot(&inventory)
}

fn capture_inventory(system: &rdrive::probe::acpi::System) -> MachinePlanResult<AcpiInventory> {
    let devices = system
        .resource_devices()
        .map_err(|error| MachinePlanError::InvalidFirmware {
            detail: format!("failed to evaluate host ACPI device resources: {error}"),
        })?;
    let console_id = system.spcr_console_device_id();
    let console_path = console_id.and_then(|console_id| {
        devices
            .iter()
            .find(|device| system.path_to_device_id(&device.path) == Some(console_id))
            .map(|device| device.path.clone())
    });
    Ok(AcpiInventory {
        devices,
        pci_ecam: system.pci_ecam_regions().to_vec(),
        io_apics: system.routing().io_apics().to_vec(),
        pch_pics: system.routing().pch_pics().to_vec(),
        console_path,
        console_memory: system.serial_console_memory_range(),
    })
}

fn build_snapshot(inventory: &AcpiInventory) -> MachinePlanResult<HostPlatformSnapshot> {
    let mut snapshot = HostPlatformSnapshot::new(inventory_generation(inventory));
    for device in &inventory.devices {
        snapshot = add_namespace_device(snapshot, inventory, device)?;
    }
    for controller in &inventory.io_apics {
        snapshot = add_io_apic(snapshot, *controller)?;
    }
    for controller in &inventory.pch_pics {
        snapshot = add_pch_pic(snapshot, *controller)?;
    }
    for ecam in &inventory.pci_ecam {
        snapshot = add_pci_ecam(snapshot, *ecam)?;
    }
    Ok(snapshot)
}

fn add_namespace_device(
    mut snapshot: HostPlatformSnapshot,
    inventory: &AcpiInventory,
    device: &AcpiResourceDevice,
) -> MachinePlanResult<HostPlatformSnapshot> {
    let ownership = classify_namespace_device(inventory, device);
    let mut descriptor =
        HostDeviceDescriptor::new(HostDeviceId::new(device.path.clone())?, ownership);
    if let Some(hid) = &device.hid {
        descriptor = descriptor.with_compatible(hid.clone());
    }
    for cid in &device.cids {
        descriptor = descriptor.with_compatible(cid.clone());
    }
    for range in &device.memory_ranges {
        let range = checked_mmio_range(range, &device.path)?;
        snapshot = snapshot.with_io_aperture(range);
        descriptor = descriptor.with_mmio(range);
    }
    for range in &device.io_ranges {
        descriptor = descriptor.with_pio(checked_pio_range(range, &device.path)?);
    }
    for route in &device.irq_routes {
        descriptor =
            descriptor.with_interrupt(HostInterruptResource::acpi(normalize_irq_route(*route)));
    }
    Ok(snapshot.with_device(descriptor))
}

fn normalize_irq_route(route: AcpiGsiRoute) -> irq_framework::AcpiGsiRoute {
    irq_framework::AcpiGsiRoute {
        gsi: route.gsi,
        vector: route.vector,
        controller: match route.controller {
            rdrive::probe::acpi::AcpiGsiController::IoApic => {
                irq_framework::AcpiGsiController::IoApic
            }
            rdrive::probe::acpi::AcpiGsiController::PchPic => {
                irq_framework::AcpiGsiController::PchPic
            }
        },
        controller_id: route.controller_id,
        controller_address: route.controller_address,
        controller_input: route.controller_input,
        trigger: match route.trigger {
            rdrive::probe::acpi::AcpiIrqTrigger::Edge => irq_framework::AcpiIrqTrigger::Edge,
            rdrive::probe::acpi::AcpiIrqTrigger::Level => irq_framework::AcpiIrqTrigger::Level,
        },
        polarity: match route.polarity {
            rdrive::probe::acpi::AcpiIrqPolarity::ActiveHigh => {
                irq_framework::AcpiIrqPolarity::ActiveHigh
            }
            rdrive::probe::acpi::AcpiIrqPolarity::ActiveLow => {
                irq_framework::AcpiIrqPolarity::ActiveLow
            }
        },
    }
}

fn add_io_apic(
    snapshot: HostPlatformSnapshot,
    controller: AcpiIoApic,
) -> MachinePlanResult<HostPlatformSnapshot> {
    let range = AddressRange::new(u64::from(controller.address), 0x1000)?;
    let descriptor = HostDeviceDescriptor::new(
        HostDeviceId::new(format!("acpi:ioapic:{}", controller.id))?,
        HostDeviceOwnership::HostExclusive,
    )
    .with_compatible("ACPIIOAP")
    .with_mmio(range);
    Ok(snapshot.with_io_aperture(range).with_device(descriptor))
}

fn add_pch_pic(
    snapshot: HostPlatformSnapshot,
    controller: AcpiPchPic,
) -> MachinePlanResult<HostPlatformSnapshot> {
    let range = AddressRange::new(controller.address, u64::from(controller.mmio_size))?;
    let descriptor = HostDeviceDescriptor::new(
        HostDeviceId::new(format!("acpi:pch-pic:{}", controller.id))?,
        HostDeviceOwnership::HostExclusive,
    )
    .with_compatible("LOONGSON-PCH-PIC")
    .with_mmio(range);
    Ok(snapshot.with_io_aperture(range).with_device(descriptor))
}

fn add_pci_ecam(
    snapshot: HostPlatformSnapshot,
    ecam: AcpiPciEcam,
) -> MachinePlanResult<HostPlatformSnapshot> {
    let size = u64::try_from(ecam.size()).map_err(|_| MachinePlanError::InvalidFirmware {
        detail: "host PCI ECAM size exceeds u64".into(),
    })?;
    let range = AddressRange::new(ecam.base_address, size)?;
    let descriptor = HostDeviceDescriptor::new(
        HostDeviceId::new(format!(
            "acpi:mcfg:{}:{}-{}",
            ecam.segment_group, ecam.bus_start, ecam.bus_end
        ))?,
        HostDeviceOwnership::HostExclusive,
    )
    .with_compatible("PNP0A08-ECAM")
    .with_mmio(range);
    Ok(snapshot.with_io_aperture(range).with_device(descriptor))
}

fn classify_namespace_device(
    inventory: &AcpiInventory,
    device: &AcpiResourceDevice,
) -> HostDeviceOwnership {
    if inventory.console_path.as_deref() == Some(device.path.as_str())
        || inventory.console_memory.is_some_and(|console| {
            device
                .memory_ranges
                .iter()
                .any(|range| ranges_overlap(console, *range))
        })
    {
        return HostDeviceOwnership::HostExclusive;
    }
    let ids = device.hid.iter().chain(&device.cids);
    if ids
        .clone()
        .any(|id| matches!(id.as_str(), "ACPI0007" | "PNP0A03" | "PNP0A08" | "PNP0C0F"))
    {
        return HostDeviceOwnership::Structural;
    }
    if ids.clone().any(|id| {
        matches!(
            id.as_str(),
            "PNP0103" | "PNP0C02" | "PNP0C09" | "PNP0C0C" | "PNP0C0E"
        )
    }) {
        return HostDeviceOwnership::HostExclusive;
    }
    if device.memory_ranges.is_empty()
        && device.io_ranges.is_empty()
        && device.irq_routes.is_empty()
    {
        HostDeviceOwnership::Structural
    } else if device.hid.is_none() && device.cids.is_empty() {
        HostDeviceOwnership::Unrepresentable
    } else {
        HostDeviceOwnership::Assignable
    }
}

fn checked_mmio_range(range: &AcpiResourceRange, path: &str) -> MachinePlanResult<AddressRange> {
    AddressRange::new(range.base, range.size).map_err(|_| MachinePlanError::InvalidFirmware {
        detail: format!(
            "host ACPI device {path} has invalid MMIO range {:#x}+{:#x}",
            range.base, range.size
        ),
    })
}

fn checked_pio_range(range: &AcpiResourceRange, path: &str) -> MachinePlanResult<IoPortRange> {
    let base = u16::try_from(range.base).map_err(|_| MachinePlanError::InvalidFirmware {
        detail: format!(
            "host ACPI device {path} has PIO base {:#x} above 0xffff",
            range.base
        ),
    })?;
    let size = u16::try_from(range.size).map_err(|_| MachinePlanError::InvalidFirmware {
        detail: format!(
            "host ACPI device {path} has PIO size {:#x} above 0xffff",
            range.size
        ),
    })?;
    IoPortRange::new(base, size).map_err(|_| MachinePlanError::InvalidFirmware {
        detail: format!(
            "host ACPI device {path} has invalid PIO range {:#x}+{:#x}",
            range.base, range.size
        ),
    })
}

fn ranges_overlap(left: AcpiResourceRange, right: AcpiResourceRange) -> bool {
    left.base < right.base.saturating_add(right.size)
        && right.base < left.base.saturating_add(left.size)
}

fn inventory_generation(inventory: &AcpiInventory) -> u64 {
    let mut hash = SnapshotHash::new();
    for device in &inventory.devices {
        hash.text(&device.path);
        if let Some(hid) = &device.hid {
            hash.text(hid);
        }
        for cid in &device.cids {
            hash.text(cid);
        }
        for range in device.memory_ranges.iter().chain(&device.io_ranges) {
            hash.number(range.base);
            hash.number(range.size);
        }
        for route in &device.irq_routes {
            hash.route(*route);
        }
    }
    for ecam in &inventory.pci_ecam {
        hash.number(ecam.base_address);
        hash.number(ecam.size() as u64);
    }
    for controller in &inventory.io_apics {
        hash.number(u64::from(controller.address));
        hash.number(u64::from(controller.gsi_base));
    }
    for controller in &inventory.pch_pics {
        hash.number(controller.address);
        hash.number(u64::from(controller.gsi_base));
    }
    hash.finish()
}

struct AcpiInventory {
    devices: Vec<AcpiResourceDevice>,
    pci_ecam: Vec<AcpiPciEcam>,
    io_apics: Vec<AcpiIoApic>,
    pch_pics: Vec<AcpiPchPic>,
    console_path: Option<String>,
    console_memory: Option<AcpiResourceRange>,
}

struct SnapshotHash(u64);

impl SnapshotHash {
    const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn text(&mut self, value: &str) {
        for byte in value.bytes() {
            self.byte(byte);
        }
        self.byte(0xff);
    }

    fn number(&mut self, value: u64) {
        for byte in value.to_le_bytes() {
            self.byte(byte);
        }
    }

    fn route(&mut self, route: AcpiGsiRoute) {
        self.number(u64::from(route.gsi));
        self.number(route.vector as u64);
        self.number(route.controller_address);
        self.number(u64::from(route.controller_input));
    }

    fn byte(&mut self, byte: u8) {
        self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
    }

    const fn finish(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use rdrive::probe::acpi::{AcpiGsiController, AcpiIrqPolarity, AcpiIrqTrigger};

    use super::*;

    #[test]
    fn snapshot_protects_spcr_console_and_interrupt_controller() {
        let inventory = AcpiInventory {
            devices: vec![AcpiResourceDevice {
                path: "\\_SB.UAR0".into(),
                hid: Some("PNP0501".into()),
                cids: Vec::new(),
                memory_ranges: vec![AcpiResourceRange {
                    base: 0x1000,
                    size: 0x100,
                }],
                io_ranges: Vec::new(),
                irq_routes: vec![AcpiGsiRoute {
                    gsi: 4,
                    vector: 0x44,
                    controller: AcpiGsiController::IoApic,
                    controller_id: 0,
                    controller_address: 0xfec0_0000,
                    controller_input: 4,
                    trigger: AcpiIrqTrigger::Edge,
                    polarity: AcpiIrqPolarity::ActiveHigh,
                }],
            }],
            pci_ecam: Vec::new(),
            io_apics: vec![AcpiIoApic {
                id: 0,
                address: 0xfec0_0000,
                gsi_base: 0,
                redirection_entries: 24,
            }],
            pch_pics: Vec::new(),
            console_path: Some("\\_SB.UAR0".into()),
            console_memory: None,
        };

        let snapshot = build_snapshot(&inventory).unwrap();
        assert_eq!(
            snapshot.devices()[0].ownership(),
            HostDeviceOwnership::HostExclusive
        );
        assert_eq!(snapshot.devices()[0].interrupts()[0].input_u32(), 4);
        assert_eq!(
            snapshot.devices()[0].interrupts()[0].trigger(),
            axvm_types::InterruptTriggerMode::EdgeTriggered
        );
        assert!(snapshot.devices().iter().any(|device| {
            device.id().as_str() == "acpi:ioapic:0"
                && device.ownership() == HostDeviceOwnership::HostExclusive
        }));
        assert!(
            snapshot
                .io_apertures()
                .iter()
                .any(|range| range.contains(0x1000))
        );
    }

    #[test]
    fn resource_device_without_hardware_identity_is_unrepresentable() {
        let inventory = AcpiInventory {
            devices: vec![AcpiResourceDevice {
                path: "\\_SB.ANON".into(),
                hid: None,
                cids: Vec::new(),
                memory_ranges: vec![AcpiResourceRange {
                    base: 0x2000,
                    size: 0x100,
                }],
                io_ranges: Vec::new(),
                irq_routes: Vec::new(),
            }],
            pci_ecam: Vec::new(),
            io_apics: Vec::new(),
            pch_pics: Vec::new(),
            console_path: None,
            console_memory: None,
        };

        let snapshot = build_snapshot(&inventory).unwrap();
        assert_eq!(
            snapshot.devices()[0].ownership(),
            HostDeviceOwnership::Unrepresentable
        );
    }
}
