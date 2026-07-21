use std::{
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axdevice::ControllerInputId;
use axvm::machine::{
    Aarch64GicV3Profile, AddressRange, ArmScmiMediationProfile, ConsoleRxPolicy, ConsoleTxPolicy,
    DeviceBackend, DeviceInstanceId, DeviceModelId, DeviceRequirements, GuestMemoryPlacement,
    GuestMemoryRegion, HostConsoleBackend, HostDeviceClaimProvider, HostDeviceDependency,
    HostDeviceDependencyKind, HostDeviceDescriptor, HostDeviceId, HostDeviceLease,
    HostDeviceOwnership, HostDeviceSelector, HostInterruptResource, HostPlatformSnapshot,
    HostProviderReference, HostProviderResourceClaim, HostProviderResourceGrant,
    HostProviderResourceLease, InterruptControllerPlan, InterruptControllerProfile, IoPortRange,
    MachinePlanError, MachineProfile, RegisteredHostDeviceClaimProvider, ResourceSlot,
    VirtualDeviceDescriptor, VirtualDeviceSource, VmMachinePlanner, VmMachineRequest,
    VmMachineTransaction,
};
use axvm_types::{GuestFirmwareKind, InterruptTriggerMode, PhysicalInterruptPolicy, VmMachineMode};

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
fn scmi_profile_rejects_smc_ids_owned_by_the_vcpu_core() {
    for function in [0x0200_0010, 0x8000_0010, 0x8400_0010] {
        assert!(
            ArmScmiMediationProfile::new(function).is_err(),
            "SMC function {function:#x} must not collide with local SMCCC/PSCI handling"
        );
    }
    assert!(ArmScmiMediationProfile::new(0x8200_0010).is_ok());
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
fn identity_allocated_ram_does_not_hide_low_passthrough_io() {
    let profile =
        MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 64..=95).unwrap();
    let low_io = AddressRange::new(0x10_0000, 0x1000).unwrap();
    let snapshot = HostPlatformSnapshot::new(1).with_io_aperture(low_io);
    let dynamic_ram = GuestMemoryRegion::identity_allocated(0x6000_0000).unwrap();
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_memory(dynamic_ram);

    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();

    assert_eq!(
        dynamic_ram.placement(),
        GuestMemoryPlacement::IdentityAllocated
    );
    assert_eq!(dynamic_ram.size(), 0x6000_0000);
    assert_eq!(plan.guest_memory(), &[dynamic_ram]);
    assert_eq!(plan.fixed_guest_memory().count(), 0);
    assert_eq!(plan.identity_mappings(), &[low_io]);
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
fn shared_passthrough_interrupt_is_planned_as_one_physical_route() {
    let profile =
        MachineProfile::new(AddressRange::new(0x8000, 0x1000).unwrap(), 32..=479).unwrap();
    let shared_interrupt =
        HostInterruptResource::controller_input(376, InterruptTriggerMode::LevelTriggered);
    let snapshot = HostPlatformSnapshot::new(1)
        .with_device(
            HostDeviceDescriptor::new(
                HostDeviceId::new("/soc/device-a").unwrap(),
                HostDeviceOwnership::Assignable,
            )
            .with_interrupt(shared_interrupt.clone()),
        )
        .with_device(
            HostDeviceDescriptor::new(
                HostDeviceId::new("/soc/device-b").unwrap(),
                HostDeviceOwnership::Assignable,
            )
            .with_interrupt(shared_interrupt.clone()),
        );
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);

    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();

    assert_eq!(plan.assigned_host_interrupts(), [shared_interrupt]);
}

#[test]
fn shared_passthrough_interrupt_rejects_conflicting_trigger_modes() {
    let profile =
        MachineProfile::new(AddressRange::new(0x8000, 0x1000).unwrap(), 32..=479).unwrap();
    let snapshot = HostPlatformSnapshot::new(1)
        .with_device(
            HostDeviceDescriptor::new(
                HostDeviceId::new("/soc/device-a").unwrap(),
                HostDeviceOwnership::Assignable,
            )
            .with_interrupt(HostInterruptResource::controller_input(
                376,
                InterruptTriggerMode::LevelTriggered,
            )),
        )
        .with_device(
            HostDeviceDescriptor::new(
                HostDeviceId::new("/soc/device-b").unwrap(),
                HostDeviceOwnership::Assignable,
            )
            .with_interrupt(HostInterruptResource::controller_input(
                376,
                InterruptTriggerMode::EdgeTriggered,
            )),
        );
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);

    let error = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap_err();

    assert!(matches!(
        error,
        MachinePlanError::ConflictingHostInterrupt {
            input: 376,
            first_device,
            second_device,
        } if first_device == "/soc/device-a" && second_device == "/soc/device-b"
    ));
}

#[test]
fn aarch64_planner_rejects_private_interrupts_as_device_inputs() {
    let controller = Aarch64GicV3Profile::new(
        AddressRange::new(0x0800_0000, 0x1_0000).unwrap(),
        0x080a_0000,
        0x2_0000,
        None,
        480,
    )
    .unwrap();
    let profile = MachineProfile::new(AddressRange::new(0x0900_0000, 0x10_0000).unwrap(), 32..=511)
        .unwrap()
        .with_interrupt_controller(InterruptControllerProfile::Aarch64GicV3(controller));
    let snapshot = HostPlatformSnapshot::new(1)
        .with_device(
            HostDeviceDescriptor::new(
                HostDeviceId::new("/interrupt-controller@8000000").unwrap(),
                HostDeviceOwnership::HostExclusive,
            )
            .with_compatible("arm,gic-v3")
            .with_mmio(AddressRange::new(0x0800_0000, 0x1_0000).unwrap())
            .with_mmio(AddressRange::new(0x080a_0000, 0x20_0000).unwrap()),
        )
        .with_device(
            HostDeviceDescriptor::new(
                HostDeviceId::new("/cpu-private-device").unwrap(),
                HostDeviceOwnership::Assignable,
            )
            .with_interrupt(HostInterruptResource::controller_input(
                23,
                InterruptTriggerMode::LevelTriggered,
            )),
        );
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);

    let error = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap_err();

    assert!(matches!(
        error,
        MachinePlanError::UnroutableDeviceInterrupt {
            device,
            input: 23,
            controller: "GICv3 SPI",
        } if device == "/cpu-private-device"
    ));
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
fn physical_interrupt_forwarding_allows_software_interrupt_devices() {
    let profile =
        MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 32..=127).unwrap();
    let snapshot = HostPlatformSnapshot::new(1);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_physical_interrupt_policy(PhysicalInterruptPolicy::HardwareForwarded)
        .with_virtual_device(pl011("console0"));

    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();

    assert_eq!(plan.virtual_devices().len(), 1);
    assert_eq!(plan.virtual_devices()[0].interrupts().len(), 1);
}

#[test]
fn passthrough_guest_virtual_mmio_pool_is_independent_from_unmapped_host_ram() {
    let virtual_mmio = AddressRange::new(0x0900_0000, 0x0100_0000).unwrap();
    let profile = MachineProfile::new(virtual_mmio, 32..=127).unwrap();
    let snapshot = HostPlatformSnapshot::new(1).with_device(
        HostDeviceDescriptor::new(
            HostDeviceId::new("/memory").unwrap(),
            HostDeviceOwnership::HostExclusive,
        )
        .with_mmio(AddressRange::new(0x0020_0000, 0x0efe_0000).unwrap()),
    );
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_virtual_device(pl011("console0"));

    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();

    assert_eq!(
        plan.virtual_devices()[0].mmio()[0].range(),
        AddressRange::new(0x0900_0000, 0x1000).unwrap()
    );
    assert!(plan.identity_mappings().is_empty());
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
fn failed_device_claim_releases_acquired_provider_resource() {
    let profile =
        MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 32..=127).unwrap();
    let provider_id = HostDeviceId::new("/clock-controller").unwrap();
    let consumer_id = HostDeviceId::new("/device-b").unwrap();
    let dependency = HostDeviceDependency::new(
        provider_id.clone(),
        "clocks",
        HostDeviceDependencyKind::Required,
        HostProviderReference::clock(vec![3]),
    )
    .unwrap();
    let mut snapshot = HostPlatformSnapshot::new(19)
        .with_device(
            HostDeviceDescriptor::new(provider_id.clone(), HostDeviceOwnership::HostExclusive)
                .with_mmio(AddressRange::new(0x2000_0000, 0x1000).unwrap()),
        )
        .with_device(
            HostDeviceDescriptor::new(consumer_id, HostDeviceOwnership::Assignable)
                .with_mmio(AddressRange::new(0x2000_1000, 0x1000).unwrap())
                .with_dependency(dependency),
        );
    snapshot
        .grant_provider_resource(
            &provider_id,
            HostProviderResourceGrant::fixed_clock(vec![3], NonZeroU32::new(24_000_000).unwrap()),
        )
        .unwrap();
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);
    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();
    assert_eq!(plan.provider_resource_claims().len(), 1);
    let releases = Arc::new(AtomicUsize::new(0));
    let claim_provider = FailingClaimProvider {
        generation: snapshot.generation(),
        releases: releases.clone(),
    };

    let error = VmMachineTransaction::claim(&plan, &claim_provider).unwrap_err();

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

#[test]
fn registered_provider_resource_claims_reject_competing_vm() {
    let profile =
        MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 32..=127).unwrap();
    let provider_id = HostDeviceId::new("/transaction-test-clock-controller").unwrap();
    let dependency = HostDeviceDependency::new(
        provider_id.clone(),
        "clocks",
        HostDeviceDependencyKind::Required,
        HostProviderReference::clock(vec![5]),
    )
    .unwrap();
    let mut snapshot = HostPlatformSnapshot::new(81)
        .with_device(
            HostDeviceDescriptor::new(provider_id.clone(), HostDeviceOwnership::HostExclusive)
                .with_mmio(AddressRange::new(0x2100_0000, 0x1000).unwrap()),
        )
        .with_device(
            HostDeviceDescriptor::new(
                HostDeviceId::new("/transaction-test-clock-consumer").unwrap(),
                HostDeviceOwnership::Assignable,
            )
            .with_mmio(AddressRange::new(0x2100_1000, 0x1000).unwrap())
            .with_dependency(dependency),
        );
    snapshot
        .grant_provider_resource(
            &provider_id,
            HostProviderResourceGrant::fixed_clock(vec![5], NonZeroU32::new(100_000_000).unwrap()),
        )
        .unwrap();
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);
    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();
    let first_provider = RegisteredHostDeviceClaimProvider::new(snapshot.generation(), 1101);
    let second_provider = RegisteredHostDeviceClaimProvider::new(snapshot.generation(), 1102);

    let first = VmMachineTransaction::claim(&plan, &first_provider).unwrap();
    let error = VmMachineTransaction::claim(&plan, &second_provider).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("provider resource Clock selector [5]")
    );

    drop(first);
    let second = VmMachineTransaction::claim(&plan, &second_provider).unwrap();
    assert_eq!(second.len(), 2);
}

#[test]
fn exclusion_registry_cannot_impersonate_a_mediated_provider_capability() {
    let profile = MachineProfile::new(AddressRange::new(0x1000_0000, 0x10_0000).unwrap(), 32..=127)
        .unwrap()
        .with_arm_scmi_mediation(ArmScmiMediationProfile::new(0x8200_0010).unwrap());
    let provider_id = HostDeviceId::new("/mediated-clock-controller").unwrap();
    let dependency = HostDeviceDependency::new(
        provider_id.clone(),
        "clocks",
        HostDeviceDependencyKind::Required,
        HostProviderReference::clock(vec![7]),
    )
    .unwrap();
    let mut snapshot = HostPlatformSnapshot::new(89)
        .with_device(
            HostDeviceDescriptor::new(provider_id.clone(), HostDeviceOwnership::HostExclusive)
                .with_mmio(AddressRange::new(0x2200_0000, 0x1000).unwrap()),
        )
        .with_device(
            HostDeviceDescriptor::new(
                HostDeviceId::new("/mediated-clock-consumer").unwrap(),
                HostDeviceOwnership::Assignable,
            )
            .with_mmio(AddressRange::new(0x2200_1000, 0x1000).unwrap())
            .with_dependency(dependency),
        );
    snapshot
        .grant_provider_resource(
            &provider_id,
            HostProviderResourceGrant::mediated_clock(vec![7]),
        )
        .unwrap();
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);
    let plan = VmMachinePlanner::new(profile)
        .plan(&request, &snapshot)
        .unwrap();
    let provider = RegisteredHostDeviceClaimProvider::new(snapshot.generation(), 1201);

    let error = VmMachineTransaction::claim(&plan, &provider).unwrap_err();

    assert!(error.to_string().contains("requires Clock control"));
    assert!(error.to_string().contains("lease exposes Pinned"));
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

    fn claim_provider_resource(
        &self,
        _resource: &HostProviderResourceClaim,
    ) -> Result<Arc<dyn HostProviderResourceLease>, MachinePlanError> {
        Ok(Arc::new(CountingLease {
            releases: self.releases.clone(),
        }))
    }
}

struct CountingLease {
    releases: Arc<AtomicUsize>,
}

impl HostDeviceLease for CountingLease {}
impl HostProviderResourceLease for CountingLease {}

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
                axdevice::InterruptSharing::Exclusive,
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
