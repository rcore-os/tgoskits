use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axdevice::ControllerInputId;
use axvm::machine::{
    Aarch64GicV3Profile, AddressRange, ConsoleRxPolicy, ConsoleTxPolicy, DeviceBackend,
    DeviceInstanceId, DeviceModelId, DeviceRequirements, GuestMemoryRegion, HostConsoleBackend,
    HostDeviceClaimProvider, HostDeviceDescriptor, HostDeviceId, HostDeviceLease,
    HostDeviceOwnership, HostDeviceSelector, HostInterruptResource, HostPlatformSnapshot,
    InterruptControllerPlan, InterruptControllerProfile, InterruptSourceKind, IoPortRange,
    MachinePlanError, MachineProfile, RegisteredHostDeviceClaimProvider, ResourceSlot,
    VirtualDeviceDescriptor, VirtualDeviceSource, VmMachinePlanner, VmMachineRequest,
    VmMachineTransaction,
};
use axvm_types::{GuestFirmwareKind, InterruptDelivery, InterruptTriggerMode, VmMachineMode};

#[test]
fn virtual_machine_allocates_resources_deterministically_without_host_io_mapping() {
    let profile =
        MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 32..=127).unwrap();
    let snapshot = HostPlatformSnapshot::new(7);
    let request = VmMachineRequest::new(VmMachineMode::Virtual, GuestFirmwareKind::Fdt)
        .with_virtual_device(pl011("serial-b"))
        .with_virtual_device(pl011("serial-a"));

    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();

    assert!(plan.identity_mappings().is_empty());
    assert_eq!(plan.virtual_devices()[0].instance_id().as_str(), "serial-a");
    assert_eq!(
        plan.virtual_devices()[0].mmio()[0].range().base(),
        0x1000_0000
    );
    assert_eq!(plan.virtual_devices()[0].interrupts()[0].id(), 32);
    assert_eq!(plan.virtual_devices()[1].instance_id().as_str(), "serial-b");
    assert_eq!(
        plan.virtual_devices()[1].mmio()[0].range().base(),
        0x1000_1000
    );
    assert_eq!(plan.virtual_devices()[1].interrupts()[0].id(), 33);
}

#[test]
fn virtual_machine_never_claims_devices_from_a_populated_host_snapshot() {
    let profile =
        MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 32..=127).unwrap();
    let snapshot = HostPlatformSnapshot::new(7).with_device(HostDeviceDescriptor::new(
        HostDeviceId::new("/host-device").unwrap(),
        HostDeviceOwnership::Assignable,
    ));
    let request = VmMachineRequest::new(VmMachineMode::Virtual, GuestFirmwareKind::Fdt);

    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();

    assert!(plan.claims().is_empty());
    assert!(plan.host_devices().is_empty());
}

#[test]
fn virtual_device_backend_policy_survives_machine_planning() {
    let profile =
        MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 32..=127).unwrap();
    let backend = DeviceBackend::HostConsole(HostConsoleBackend::new(
        ConsoleRxPolicy::Disabled,
        ConsoleTxPolicy::Exclusive,
    ));
    let request = VmMachineRequest::new(VmMachineMode::Virtual, GuestFirmwareKind::Fdt)
        .with_virtual_device(pl011("console0").with_backend(backend));

    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &HostPlatformSnapshot::new(0))
        .unwrap();

    assert_eq!(plan.virtual_devices()[0].backend(), backend);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn x86_standard_profile_allocates_com1_ports_and_irq() {
    let request = VmMachineRequest::new(VmMachineMode::Virtual, GuestFirmwareKind::Auto)
        .with_virtual_device(VirtualDeviceDescriptor::new(
            DeviceInstanceId::new("console0").unwrap(),
            DeviceModelId::new("x86-com1").unwrap(),
            axvm::x86_com1_device_requirements().unwrap(),
        ));

    let plan = VmMachinePlanner::new(axvm::standard_machine_profile().unwrap())
        .plan(&request, &HostPlatformSnapshot::new(0))
        .unwrap();
    let console = &plan.virtual_devices()[0];

    assert_eq!(console.pio()[0].range().base(), 0x3f8);
    assert_eq!(console.pio()[0].range().size(), 8);
    assert_eq!(console.interrupts()[0].id(), 4);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn passthrough_auto_com1_replaces_host_serial_resources() {
    let host_com1 = HostDeviceId::new("\\_SB.COM1").unwrap();
    let snapshot = HostPlatformSnapshot::new(1).with_device(
        HostDeviceDescriptor::new(host_com1.clone(), HostDeviceOwnership::Assignable)
            .with_compatible("PNP0501")
            .with_pio(IoPortRange::new(0x3f8, 8).unwrap())
            .with_interrupt(HostInterruptResource::controller_input(
                4,
                InterruptTriggerMode::EdgeTriggered,
            )),
    );
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Acpi)
        .with_virtual_device(
            VirtualDeviceDescriptor::new(
                DeviceInstanceId::new("console0").unwrap(),
                DeviceModelId::new("x86-com1").unwrap(),
                axvm::x86_com1_device_requirements().unwrap(),
            )
            .with_compatible("PNP0501"),
        );

    let plan = VmMachinePlanner::new(axvm::standard_machine_profile().unwrap())
        .plan(&request, &snapshot)
        .unwrap();

    assert_eq!(plan.virtual_devices()[0].host_template(), Some(&host_com1));
    assert_eq!(
        plan.host_devices()[0].disposition(),
        axvm::machine::DeviceDisposition::VirtualReplacement
    );
    assert!(plan.assigned_host_pio().next().is_none());
}

#[test]
fn passthrough_dynamic_pio_allocation_avoids_host_ports() {
    let profile = MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 4..=23)
        .unwrap()
        .with_pio_pool(IoPortRange::new(0x3f8, 8).unwrap());
    let snapshot = HostPlatformSnapshot::new(1).with_device(
        HostDeviceDescriptor::new(
            HostDeviceId::new("acpi:host-com1").unwrap(),
            HostDeviceOwnership::Assignable,
        )
        .with_pio(IoPortRange::new(0x3f8, 8).unwrap()),
    );
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Acpi)
        .with_virtual_device(
            VirtualDeviceDescriptor::new(
                DeviceInstanceId::new("allocated-console").unwrap(),
                DeviceModelId::new("test-pio-device").unwrap(),
                DeviceRequirements::new()
                    .with_pio(ResourceSlot::new("registers").unwrap(), 8, 8)
                    .unwrap(),
            )
            .with_source(VirtualDeviceSource::Allocate),
        );

    let error = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap_err();

    assert!(matches!(
        error,
        MachinePlanError::ResourceAllocation {
            resource: "PIO",
            ..
        }
    ));
}

#[test]
fn passthrough_reserves_host_pio_at_the_inclusive_pool_end() {
    let profile = MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 4..=23)
        .unwrap()
        .with_pio_pool(IoPortRange::new(0x3f8, 8).unwrap());
    let snapshot = HostPlatformSnapshot::new(1).with_device(
        HostDeviceDescriptor::new(
            HostDeviceId::new("acpi:last-pio").unwrap(),
            HostDeviceOwnership::HostExclusive,
        )
        .with_pio(IoPortRange::new(0x3ff, 1).unwrap()),
    );
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Acpi);

    VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();
}

#[test]
fn passthrough_reserves_interrupt_at_inclusive_pool_end() {
    let profile =
        MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 4..=23).unwrap();
    let snapshot = HostPlatformSnapshot::new(1).with_device(
        HostDeviceDescriptor::new(
            HostDeviceId::new("acpi:last-gsi").unwrap(),
            HostDeviceOwnership::HostExclusive,
        )
        .with_interrupt(HostInterruptResource::controller_input(
            23,
            InterruptTriggerMode::LevelTriggered,
        )),
    );
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Acpi);

    VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();
}

#[test]
fn aarch64_controller_plan_sizes_redistributors_from_vcpu_count() {
    let controller = Aarch64GicV3Profile::new(
        AddressRange::new(0x0800_0000, 0x1_0000).unwrap(),
        0x080a_0000,
        0x2_0000,
        Some(AddressRange::new(0x0808_0000, 0x2_0000).unwrap()),
        480,
    )
    .unwrap();
    let profile = MachineProfile::new(AddressRange::new(0x0900_0000, 0x10_0000).unwrap(), 32..=511)
        .unwrap()
        .with_interrupt_controller(InterruptControllerProfile::Aarch64GicV3(controller));
    let request =
        VmMachineRequest::new(VmMachineMode::Virtual, GuestFirmwareKind::Fdt).with_vcpu_count(4);

    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &HostPlatformSnapshot::new(0))
        .unwrap();

    let InterruptControllerPlan::Aarch64GicV3(gic) = plan.interrupt_controller().unwrap() else {
        panic!("expected an AArch64 GICv3 plan");
    };
    assert_eq!(gic.redistributors().base(), 0x080a_0000);
    assert_eq!(gic.redistributors().size(), 4 * 0x2_0000);
    assert_eq!(gic.spi_count(), 480);
}

#[test]
fn passthrough_machine_punches_mandatory_and_configured_holes() {
    let profile = MachineProfile::new(AddressRange::new(0x3000, 0x1000).unwrap(), 64..=95).unwrap();
    let snapshot = HostPlatformSnapshot::new(1)
        .with_io_aperture(AddressRange::new(0x1000, 0x5000).unwrap())
        .with_device(
            HostDeviceDescriptor::new(
                HostDeviceId::new("/host-only").unwrap(),
                HostDeviceOwnership::HostExclusive,
            )
            .with_mmio(AddressRange::new(0x1800, 0x800).unwrap()),
        )
        .with_device(
            HostDeviceDescriptor::new(
                HostDeviceId::new("/denied").unwrap(),
                HostDeviceOwnership::Assignable,
            )
            .with_mmio(AddressRange::new(0x4000, 0x1000).unwrap()),
        );
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_memory(GuestMemoryRegion::new(
            AddressRange::new(0x2800, 0x800).unwrap(),
        ))
        .deny(HostDeviceSelector::Id(
            HostDeviceId::new("/denied").unwrap(),
        ));

    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();

    let mappings = plan.identity_mappings();
    assert!(mappings.iter().any(|range| range.contains(0x1000)));
    assert!(!mappings.iter().any(|range| range.contains(0x1800)));
    assert!(!mappings.iter().any(|range| range.contains(0x2800)));
    assert!(!mappings.iter().any(|range| range.contains(0x4000)));
    assert!(mappings.iter().any(|range| range.contains(0x5800)));
}

#[test]
fn passthrough_pci_io_bar_is_claimed_and_exposed_as_host_pio() {
    let profile =
        MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 4..=23).unwrap();
    let endpoint = HostDeviceId::new("pci:0000:00:03.0").unwrap();
    let snapshot = HostPlatformSnapshot::new(11).with_device(
        HostDeviceDescriptor::new(endpoint.clone(), HostDeviceOwnership::Transferable)
            .with_compatible("pci1af4,1001")
            .with_pio(IoPortRange::new(0x6000, 0x80).unwrap()),
    );
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Acpi);

    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();

    assert_eq!(
        plan.assigned_host_pio().collect::<Vec<_>>(),
        [IoPortRange::new(0x6000, 0x80).unwrap()]
    );
    assert_eq!(plan.claims(), [endpoint]);
}

#[test]
fn interrupt_deny_excludes_the_owning_passthrough_device() {
    let profile = MachineProfile::new(AddressRange::new(0x8000, 0x1000).unwrap(), 64..=95).unwrap();
    let device_id = HostDeviceId::new("/device@2000").unwrap();
    let snapshot = HostPlatformSnapshot::new(1)
        .with_io_aperture(AddressRange::new(0x1000, 0x4000).unwrap())
        .with_device(
            HostDeviceDescriptor::new(device_id.clone(), HostDeviceOwnership::Assignable)
                .with_mmio(AddressRange::new(0x2000, 0x1000).unwrap())
                .with_interrupt(HostInterruptResource::controller_input(
                    70,
                    InterruptTriggerMode::LevelTriggered,
                )),
        );
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .deny(HostDeviceSelector::Interrupt(ControllerInputId::new(70)));

    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();
    let device = plan
        .host_devices()
        .iter()
        .find(|device| device.id() == &device_id)
        .unwrap();

    assert_eq!(
        device.disposition(),
        axvm::machine::DeviceDisposition::Denied
    );
    assert!(plan.claims().is_empty());
    assert!(
        !plan
            .identity_mappings()
            .iter()
            .any(|range| range.contains(0x2000))
    );
}

#[test]
fn passthrough_punches_profile_owned_mmio_windows() {
    let profile = MachineProfile::new(AddressRange::new(0x8000, 0x1000).unwrap(), 64..=95)
        .unwrap()
        .with_reserved_mmio(AddressRange::new(0x3000, 0x1000).unwrap());
    let snapshot =
        HostPlatformSnapshot::new(1).with_io_aperture(AddressRange::new(0x1000, 0x4000).unwrap());
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);

    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();

    assert!(
        plan.identity_mappings()
            .iter()
            .any(|range| range.contains(0x2000))
    );
    assert!(
        !plan
            .identity_mappings()
            .iter()
            .any(|range| range.contains(0x3000))
    );
}

#[test]
fn passthrough_auto_matching_uses_each_host_template_once() {
    let profile =
        MachineProfile::new(AddressRange::new(0x3000_0000, 0x10_0000).unwrap(), 64..=95).unwrap();
    let snapshot = HostPlatformSnapshot::new(1)
        .with_device(host_pl011("/soc/serial@1000", 0x1000, 40))
        .with_device(host_pl011("/soc/serial@2000", 0x2000, 41));
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_virtual_device(pl011("serial-b"))
        .with_virtual_device(pl011("serial-a"));

    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();

    assert_eq!(plan.virtual_devices()[0].mmio()[0].range().base(), 0x1000);
    assert_eq!(plan.virtual_devices()[0].interrupts()[0].id(), 40);
    assert_eq!(plan.virtual_devices()[1].mmio()[0].range().base(), 0x2000);
    assert_eq!(plan.virtual_devices()[1].interrupts()[0].id(), 41);
}

#[test]
fn host_console_backend_prefers_the_firmware_selected_template() {
    let profile =
        MachineProfile::new(AddressRange::new(0x3000_0000, 0x10_0000).unwrap(), 64..=127).unwrap();
    let first = HostDeviceId::new("/soc/serial@2800c000").unwrap();
    let console = HostDeviceId::new("/soc/serial@2800d000").unwrap();
    let snapshot = HostPlatformSnapshot::new(1)
        .with_device(host_pl011(first.as_str(), 0x2800_c000, 115))
        .with_device(host_pl011(console.as_str(), 0x2800_d000, 116))
        .with_console_device(console.clone())
        .unwrap();
    let backend = DeviceBackend::HostConsole(HostConsoleBackend::new(
        ConsoleRxPolicy::Exclusive,
        ConsoleTxPolicy::Shared,
    ));
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_virtual_device(pl011("console0").with_backend(backend));

    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();

    assert_eq!(plan.virtual_devices()[0].host_template(), Some(&console));
    assert_eq!(
        plan.virtual_devices()[0].mmio()[0].range().base(),
        0x2800_d000
    );
    assert_eq!(plan.virtual_devices()[0].interrupts()[0].id(), 116);
    assert_eq!(plan.host_console(), Some(&console));
}

#[test]
fn direct_interrupt_delivery_rejects_software_interrupt_devices() {
    let profile =
        MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 32..=127).unwrap();
    let snapshot = HostPlatformSnapshot::new(1);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_interrupt_delivery(InterruptDelivery::Direct)
        .with_virtual_device(pl011("console0"));

    let error = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap_err();

    assert!(error.to_string().contains("software interrupt"));
}

#[test]
fn failed_claim_transaction_releases_every_acquired_device() {
    let profile =
        MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 32..=127).unwrap();
    let snapshot = HostPlatformSnapshot::new(9)
        .with_device(HostDeviceDescriptor::new(
            HostDeviceId::new("/device-a").unwrap(),
            HostDeviceOwnership::Assignable,
        ))
        .with_device(HostDeviceDescriptor::new(
            HostDeviceId::new("/device-b").unwrap(),
            HostDeviceOwnership::Transferable,
        ));
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);
    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();
    let releases = Arc::new(AtomicUsize::new(0));
    let provider = FailingClaimProvider {
        generation: 9,
        releases: releases.clone(),
    };

    let error = VmMachineTransaction::claim(&plan, &provider).unwrap_err();

    assert!(error.to_string().contains("/device-b"));
    assert_eq!(releases.load(Ordering::Acquire), 1);
}

#[test]
fn registered_claims_reject_competing_vm_and_release_on_drop() {
    let profile =
        MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 32..=127).unwrap();
    let snapshot = HostPlatformSnapshot::new(77).with_device(HostDeviceDescriptor::new(
        HostDeviceId::new("/transaction-test-exclusive-device").unwrap(),
        HostDeviceOwnership::Assignable,
    ));
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);
    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();
    let first_provider = RegisteredHostDeviceClaimProvider::new(77, 1001);
    let second_provider = RegisteredHostDeviceClaimProvider::new(77, 1002);

    let first = VmMachineTransaction::claim(&plan, &first_provider).unwrap();
    let error = VmMachineTransaction::claim(&plan, &second_provider).unwrap_err();
    assert!(error.to_string().contains("already owned by VM 1001"));

    drop(first);
    let second = VmMachineTransaction::claim(&plan, &second_provider).unwrap();
    assert_eq!(second.len(), 1);
}

struct FailingClaimProvider {
    generation: u64,
    releases: Arc<AtomicUsize>,
}

impl HostDeviceClaimProvider for FailingClaimProvider {
    fn snapshot_generation(&self) -> u64 {
        self.generation
    }

    fn claim(&self, device: &HostDeviceId) -> Result<Box<dyn HostDeviceLease>, MachinePlanError> {
        if device.as_str() == "/device-b" {
            return Err(MachinePlanError::ClaimRejected {
                device: device.to_string(),
                detail: "already owned".into(),
            });
        }
        Ok(Box::new(CountingLease {
            releases: self.releases.clone(),
        }))
    }
}

struct CountingLease {
    releases: Arc<AtomicUsize>,
}

impl HostDeviceLease for CountingLease {}

impl Drop for CountingLease {
    fn drop(&mut self) {
        self.releases.fetch_add(1, Ordering::AcqRel);
    }
}

fn pl011(instance_id: &str) -> VirtualDeviceDescriptor {
    VirtualDeviceDescriptor::new(
        DeviceInstanceId::new(instance_id).unwrap(),
        DeviceModelId::new("arm-pl011").unwrap(),
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
    .with_compatible("arm,pl011")
    .with_source(VirtualDeviceSource::Auto)
}

fn host_pl011(path: &str, base: u64, interrupt: u32) -> HostDeviceDescriptor {
    HostDeviceDescriptor::new(
        HostDeviceId::new(path).unwrap(),
        HostDeviceOwnership::Assignable,
    )
    .with_compatible("arm,pl011")
    .with_mmio(AddressRange::new(base, 0x1000).unwrap())
    .with_interrupt(HostInterruptResource::controller_input(
        interrupt,
        InterruptTriggerMode::LevelTriggered,
    ))
}
