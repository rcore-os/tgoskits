use axdevice::{DeviceModelId, DeviceRequirements, InterruptSourceKind, ResourceSlot};
use axvm::machine::{
    Aarch64GicV3Profile, AddressRange, DeviceDisposition, DeviceInstanceId, FdtInterruptEncoding,
    GuestMemoryRegion, HostConsoleEvidence, HostConsoleLocation, HostDeviceId, HostDeviceSelector,
    HostFdtConfig, HostPlatformSnapshot, InterruptControllerProfile, MachineProfile,
    VirtualDeviceDescriptor, VirtualDeviceSource, VmMachinePlanner, VmMachineRequest,
    generate_host_fdt,
};
use axvm_types::{GuestFirmwareKind, InterruptDelivery, InterruptTriggerMode, VmMachineMode};
use fdt_edit::{Fdt, Node, Property};
use fdt_raw::RegInfo;

#[test]
fn host_fdt_is_filtered_and_rebuilt_from_the_machine_plan() {
    let host = host_fdt();
    let snapshot = whole_machine_snapshot(&host);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_memory(GuestMemoryRegion::new(
            AddressRange::new(0x8000_0000, 0x2000_0000).unwrap(),
        ))
        .deny(HostDeviceSelector::PathSubtree(
            HostDeviceId::new("/soc/denied@a000000").unwrap(),
        ))
        .with_virtual_device(pl011());
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();
    let guest = generate_host_fdt(
        &plan,
        &snapshot,
        &HostFdtConfig::new([0]).with_bootargs("console=ttyAMA0"),
    )
    .unwrap();
    let guest = Fdt::from_bytes(&guest).unwrap();

    assert!(guest.get_by_path_id("/soc/denied@a000000").is_none());
    assert!(guest.get_by_path_id("/soc/serial@9000000").is_some());
    assert!(guest.get_by_path_id("/memory@40000000").is_none());
    let memory = guest.get_by_path_id("/memory@80000000").unwrap();
    let reg = guest.view_typed(memory).unwrap().regs();
    assert_eq!(reg[0].address, 0x8000_0000);
    assert_eq!(reg[0].size, Some(0x2000_0000));
    assert!(guest.get_by_path_id("/cpus/cpu@0").is_some());
    assert!(guest.get_by_path_id("/cpus/cpu@1").is_none());
    assert_eq!(
        guest
            .get_by_path("/chosen")
            .unwrap()
            .as_node()
            .get_property("bootargs")
            .unwrap()
            .as_str(),
        Some("console=ttyAMA0")
    );
}

#[test]
fn live_authorized_passthrough_console_preserves_source_fdt_activation() {
    let host = host_fdt();
    let mut host = Fdt::from_bytes(&host).unwrap();
    let chosen = host.get_by_path_id("/chosen").unwrap();
    host.node_mut(chosen).unwrap().set_property(string_property(
        "stdout-path",
        "/soc/serial@9000000:115200n8",
    ));
    let uart = host.get_by_path_id("/soc/serial@9000000").unwrap();
    host.node_mut(uart)
        .unwrap()
        .set_property(string_property("status", "disabled"));
    let host = host.encode().as_ref().to_vec();

    let mut snapshot = whole_machine_snapshot(&host);
    snapshot
        .grant_console_transfer(
            HostConsoleLocation::MmioBase(0x0900_0000),
            HostConsoleEvidence::LivePlatform,
        )
        .unwrap();
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();
    assert_eq!(
        plan.host_devices()
            .iter()
            .find(|device| device.id().as_str() == "/soc/serial@9000000")
            .unwrap()
            .disposition(),
        DeviceDisposition::Passthrough
    );

    let guest = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0])).unwrap();
    let guest = Fdt::from_bytes(&guest).unwrap();
    let uart = guest.get_by_path("/soc/serial@9000000").unwrap().as_node();

    assert_eq!(
        uart.get_property("status").and_then(Property::as_str),
        Some("disabled")
    );
}

#[test]
fn rockchip_fiq_console_is_normalized_to_an_owned_uart() {
    let host = host_fdt_with_rockchip_fiq_debugger_console();
    let mut snapshot = whole_machine_snapshot(&host);
    let uart = HostDeviceId::new("/soc/serial@9000000").unwrap();
    snapshot
        .grant_console_transfer(
            HostConsoleLocation::Device(uart.clone()),
            HostConsoleEvidence::LivePlatform,
        )
        .unwrap();

    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_interrupt_delivery(InterruptDelivery::Direct);
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();

    assert_eq!(
        plan.host_devices()
            .iter()
            .find(|device| device.id().as_str() == "/fiq-debugger")
            .unwrap()
            .disposition(),
        DeviceDisposition::HostExclusive
    );
    assert_eq!(
        plan.assigned_host_interrupts()
            .iter()
            .map(|interrupt| interrupt.input_u32())
            .collect::<Vec<_>>(),
        vec![33]
    );

    let guest = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0])).unwrap();
    let guest = Fdt::from_bytes(&guest).unwrap();
    assert!(guest.get_by_path_id("/fiq-debugger").is_none());
    let uart = guest.get_by_path("/soc/serial@9000000").unwrap().as_node();
    assert_eq!(
        uart.get_property("status").and_then(Property::as_str),
        Some("okay")
    );
    let chosen = guest.get_by_path("/chosen").unwrap().as_node();
    let bootargs = chosen
        .get_property("bootargs")
        .and_then(Property::as_str)
        .unwrap();
    assert!(bootargs.contains("console=ttyS2,1500000"));
    assert!(!bootargs.contains("ttyFIQ"));
    assert_eq!(
        chosen
            .get_property("stdout-path")
            .and_then(Property::as_str),
        Some("serial2:1500000n8")
    );
}

#[test]
fn protected_rockchip_console_is_replaced_by_the_virtual_uart() {
    let host = host_fdt_with_rockchip_fiq_debugger_console();
    let snapshot = whole_machine_snapshot(&host);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_virtual_device(pl011().with_source(VirtualDeviceSource::Allocate));
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();
    let console_base = plan.virtual_devices()[0].mmio()[0].range().base();

    let guest = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0])).unwrap();
    let guest = Fdt::from_bytes(&guest).unwrap();
    assert!(guest.get_by_path_id("/fiq-debugger").is_none());
    assert!(guest.get_by_path_id("/soc/serial@9000000").is_none());
    assert!(
        guest
            .get_by_path_id(&format!("/serial@{console_base:x}"))
            .is_some()
    );

    let chosen = guest.get_by_path("/chosen").unwrap().as_node();
    let bootargs = chosen
        .get_property("bootargs")
        .and_then(Property::as_str)
        .unwrap();
    assert_eq!(bootargs, "earlycon rootwait");
    assert_eq!(
        chosen
            .get_property("stdout-path")
            .and_then(Property::as_str),
        Some("serial0:115200n8")
    );
}

#[test]
fn passthrough_pcie_retains_its_embedded_legacy_interrupt_controller() {
    let host = host_fdt_with_pcie_legacy_interrupt_controller();
    let snapshot = whole_machine_snapshot(&host);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();

    let guest = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0])).unwrap();
    let guest = Fdt::from_bytes(&guest).unwrap();

    assert!(guest.get_by_path_id("/soc/pcie@fe180000").is_some());
    assert!(
        guest
            .get_by_path_id("/soc/pcie@fe180000/legacy-interrupt-controller")
            .is_some()
    );
}

#[test]
fn passthrough_retains_a_memory_mapped_interrupt_controller_cascade() {
    let host = host_fdt_with_gpio_interrupt_controller();
    let snapshot = whole_machine_snapshot(&host);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();

    assert_eq!(
        plan.host_devices()
            .iter()
            .find(|device| device.id().as_str() == "/soc/gpio@b000000")
            .unwrap()
            .disposition(),
        DeviceDisposition::Passthrough
    );
    assert_eq!(
        plan.host_devices()
            .iter()
            .find(|device| device.id().as_str() == "/soc/interrupt-controller@8000000")
            .unwrap()
            .disposition(),
        DeviceDisposition::HostExclusive
    );

    let guest = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0])).unwrap();
    let guest = Fdt::from_bytes(&guest).unwrap();

    assert!(guest.get_by_path_id("/soc/gpio@b000000").is_some());
}

#[test]
fn direct_interrupt_passthrough_hides_the_unisolated_physical_its() {
    let host = host_fdt_with_physical_its();
    let snapshot = whole_machine_snapshot(&host);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_interrupt_delivery(InterruptDelivery::Direct);
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();

    let its = plan
        .host_devices()
        .iter()
        .find(|device| device.id().as_str() == "/soc/gic-its@8080000")
        .unwrap();
    assert_eq!(its.disposition(), DeviceDisposition::HostExclusive);
    assert!(
        !plan
            .identity_mappings()
            .iter()
            .any(|mapping| mapping.contains(0x0808_0000))
    );

    let guest = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0])).unwrap();
    let guest = Fdt::from_bytes(&guest).unwrap();

    assert!(guest.get_by_path_id("/soc/gic-its@8080000").is_none());
}

#[test]
fn direct_interrupt_passthrough_filters_devices_requiring_the_physical_its() {
    let host = host_fdt_with_pcie_using_physical_its();
    let snapshot = whole_machine_snapshot(&host);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_interrupt_delivery(InterruptDelivery::Direct);
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();

    let pcie = plan
        .host_devices()
        .iter()
        .find(|device| device.id().as_str() == "/soc/pcie@40000000")
        .unwrap();
    assert_eq!(pcie.disposition(), DeviceDisposition::Unrepresentable);

    let guest = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0])).unwrap();
    let guest = Fdt::from_bytes(&guest).unwrap();
    assert!(guest.get_by_path_id("/soc/pcie@40000000").is_none());
    assert!(guest.get_by_path_id("/soc/gic-its@8080000").is_none());
}

#[test]
fn host_cpu_selection_uses_the_hardware_affinity_from_reg() {
    let host = host_fdt_with_non_identity_cpu_unit_addresses();
    let snapshot = whole_machine_snapshot(&host);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();

    let guest = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0x100])).unwrap();
    let guest = Fdt::from_bytes(&guest).unwrap();

    assert!(guest.get_by_path_id("/cpus/cpu@0").is_some());
    assert!(guest.get_by_path_id("/cpus/cpu@1").is_none());
}

#[test]
fn host_psci_conduit_is_preserved_for_platform_compatibility() {
    for method in ["smc", "hvc"] {
        let host = host_fdt_with_psci(method);
        let snapshot = whole_machine_snapshot(&host);
        let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);
        let plan = VmMachinePlanner::new(aarch64_profile())
            .plan(&request, &snapshot)
            .unwrap();

        let guest = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0])).unwrap();
        let guest = Fdt::from_bytes(&guest).unwrap();
        let psci = guest.get_by_path("/psci").unwrap().as_node();

        assert_eq!(psci.get_property("method").unwrap().as_str(), Some(method));
    }
}

#[test]
fn mixed_interrupt_contexts_keep_only_assigned_cpu_providers() {
    let host = host_fdt_with_mixed_cpu_interrupt_contexts();
    let snapshot = whole_machine_snapshot(&host);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();

    let guest = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0])).unwrap();
    let guest = Fdt::from_bytes(&guest).unwrap();
    let plic = guest.get_by_path("/soc/plic@c000000").unwrap().as_node();

    assert_eq!(
        plic.get_property("interrupts-extended")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>(),
        vec![10, 3, 10, 7]
    );
    assert!(
        guest
            .get_by_path_id("/cpus/cpu@1/interrupt-controller")
            .is_none()
    );
}

#[test]
fn dynamic_pl011_uses_gic_cells_fixed_clock_and_console_alias() {
    let host = host_fdt();
    let snapshot = whole_machine_snapshot(&host);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_memory(GuestMemoryRegion::new(
            AddressRange::new(0x8000_0000, 0x2000_0000).unwrap(),
        ))
        .with_virtual_device(pl011().with_source(VirtualDeviceSource::Allocate));
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();

    let guest = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0])).unwrap();
    let guest = Fdt::from_bytes(&guest).unwrap();
    let uart = guest.get_by_path_id("/serial@9001000").unwrap();
    let uart = guest.node(uart).unwrap();

    assert_eq!(
        uart.get_property("interrupts")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>(),
        vec![0, 0, 4]
    );
    assert_eq!(
        uart.get_property("interrupt-parent")
            .and_then(Property::get_u32),
        Some(1)
    );
    assert_eq!(
        uart.get_property("clocks").unwrap().get_u32_iter().count(),
        2
    );
    assert!(guest.get_by_path_id("/clock-2").is_some());
    assert_eq!(
        guest
            .get_by_path("/aliases")
            .unwrap()
            .as_node()
            .get_property("serial0")
            .unwrap()
            .as_str(),
        Some("/serial@9001000")
    );
    assert_eq!(
        guest
            .get_by_path("/chosen")
            .unwrap()
            .as_node()
            .get_property("stdout-path")
            .unwrap()
            .as_str(),
        Some("serial0:115200n8")
    );
}

#[test]
fn host_template_rejects_an_irq_trigger_incompatible_with_the_model() {
    let host = host_fdt_with_uart_interrupt_flags(1);
    let snapshot = whole_machine_snapshot(&host);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_virtual_device(pl011());

    let error = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap_err();

    assert!(error.to_string().contains("trigger"));
}

#[test]
fn virtual_uart_template_does_not_expose_host_dma_iommu_or_msi_capabilities() {
    let host = host_fdt_with_virtual_uart_host_capabilities();
    let snapshot = whole_machine_snapshot(&host);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_virtual_device(pl011());
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();

    let guest = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0])).unwrap();
    let guest = Fdt::from_bytes(&guest).unwrap();
    let uart = guest.get_by_path("/soc/serial@9000000").unwrap().as_node();

    for property in [
        "dmas",
        "dma-names",
        "iommus",
        "msi-parent",
        "interrupts-extended",
    ] {
        assert!(uart.get_property(property).is_none(), "leaked {property}");
    }
    assert!(uart.get_property("interrupts").is_some());
    assert!(uart.get_property("interrupt-parent").is_some());
    assert!(uart.get_property("clocks").is_some());
}

#[test]
fn required_host_dependency_is_rejected_during_machine_planning() {
    let host = host_fdt_with_host_exclusive_dependency();
    let snapshot = whole_machine_snapshot(&host);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_memory(GuestMemoryRegion::new(
            AddressRange::new(0x8000_0000, 0x2000_0000).unwrap(),
        ));
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();

    let consumer = plan
        .host_devices()
        .iter()
        .find(|device| device.id().as_str() == "/soc/consumer@a100000")
        .unwrap();
    assert_eq!(consumer.disposition(), DeviceDisposition::Unrepresentable);
    assert!(!plan.claims().contains(consumer.id()));

    let guest = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0])).unwrap();
    let guest = Fdt::from_bytes(&guest).unwrap();
    assert!(guest.get_by_path_id("/soc/consumer@a100000").is_none());
    assert!(
        guest
            .get_by_path_id("/soc/consumer@a100000/child")
            .is_none()
    );
    assert!(guest.get_by_path_id("/soc/serial@9000000").is_none());
    assert!(
        guest
            .get_by_path("/aliases")
            .unwrap()
            .as_node()
            .get_property("blocked-device")
            .is_none()
    );
}

#[test]
fn optional_host_dependency_is_removed_without_exposing_its_provider() {
    let host = host_fdt_with_optional_host_exclusive_dependency();
    let snapshot = whole_machine_snapshot(&host);
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();

    let guest = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0])).unwrap();
    let guest = Fdt::from_bytes(&guest).unwrap();
    let consumer = guest
        .get_by_path("/soc/consumer@a100000")
        .unwrap()
        .as_node();

    assert!(consumer.get_property("reset-gpios").is_none());
    assert!(guest.get_by_path_id("/soc/serial@9000000").is_none());
}

fn pl011() -> VirtualDeviceDescriptor {
    let requirements = DeviceRequirements::new()
        .with_mmio(ResourceSlot::new("registers").unwrap(), 0x1000, 0x1000)
        .unwrap()
        .with_wired_irq(
            ResourceSlot::new("irq").unwrap(),
            InterruptTriggerMode::LevelTriggered,
            InterruptSourceKind::Software,
            axdevice::InterruptSharing::Exclusive,
        )
        .unwrap();
    VirtualDeviceDescriptor::new(
        DeviceInstanceId::new("console0").unwrap(),
        DeviceModelId::new("arm-pl011").unwrap(),
        requirements,
    )
    .with_compatible("arm,pl011")
}

fn whole_machine_snapshot(host: &[u8]) -> HostPlatformSnapshot {
    let mut snapshot =
        HostPlatformSnapshot::from_fdt(7, host, FdtInterruptEncoding::ArmGic).unwrap();
    snapshot.grant_whole_machine_assignment().unwrap();
    snapshot
}

fn aarch64_profile() -> MachineProfile {
    MachineProfile::new(
        AddressRange::new(0x0900_0000, 0x0100_0000).unwrap(),
        32..=511,
    )
    .unwrap()
    .with_interrupt_controller(InterruptControllerProfile::Aarch64GicV3(
        Aarch64GicV3Profile::new(
            AddressRange::new(0x0800_0000, 0x1_0000).unwrap(),
            0x080a_0000,
            0x2_0000,
            None,
            480,
        )
        .unwrap(),
    ))
}

fn host_fdt() -> Vec<u8> {
    host_fdt_with_uart_interrupt_flags(4)
}

fn host_fdt_with_uart_interrupt_flags(interrupt_flags: u32) -> Vec<u8> {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.node_mut(root)
        .unwrap()
        .set_property(u32_property("#address-cells", &[2]));
    fdt.node_mut(root)
        .unwrap()
        .set_property(u32_property("#size-cells", &[2]));
    fdt.node_mut(root)
        .unwrap()
        .set_property(u32_property("interrupt-parent", &[1]));

    let chosen = fdt.add_node(root, Node::new("chosen"));
    fdt.node_mut(chosen)
        .unwrap()
        .set_property(string_property("bootargs", "host-only"));
    let cpus = fdt.add_node(root, Node::new("cpus"));
    fdt.node_mut(cpus)
        .unwrap()
        .set_property(u32_property("#address-cells", &[2]));
    fdt.node_mut(cpus)
        .unwrap()
        .set_property(u32_property("#size-cells", &[0]));
    for cpu in 0..2 {
        let node = fdt.add_node(cpus, Node::new(&format!("cpu@{cpu}")));
        fdt.view_typed_mut(node)
            .unwrap()
            .set_regs(&[RegInfo::new(cpu, None)]);
    }

    let memory = fdt.add_node(root, Node::new("memory@40000000"));
    fdt.node_mut(memory)
        .unwrap()
        .set_property(string_property("device_type", "memory"));
    fdt.view_typed_mut(memory)
        .unwrap()
        .set_regs(&[RegInfo::new(0x4000_0000, Some(0x1000_0000))]);

    let soc = fdt.add_node(root, Node::new("soc"));
    fdt.node_mut(soc)
        .unwrap()
        .set_property(u32_property("#address-cells", &[2]));
    fdt.node_mut(soc)
        .unwrap()
        .set_property(u32_property("#size-cells", &[2]));
    fdt.node_mut(soc)
        .unwrap()
        .set_property(Property::new("ranges", Vec::new()));

    let gic = fdt.add_node(soc, Node::new("interrupt-controller@8000000"));
    fdt.node_mut(gic)
        .unwrap()
        .set_property(string_property("compatible", "arm,gic-v3"));
    fdt.node_mut(gic)
        .unwrap()
        .set_property(Property::new("interrupt-controller", Vec::new()));
    fdt.node_mut(gic)
        .unwrap()
        .set_property(u32_property("#interrupt-cells", &[3]));
    fdt.node_mut(gic)
        .unwrap()
        .set_property(u32_property("phandle", &[1]));
    fdt.view_typed_mut(gic).unwrap().set_regs(&[
        RegInfo::new(0x0800_0000, Some(0x1_0000)),
        RegInfo::new(0x080a_0000, Some(0x4_0000)),
    ]);

    let timer = fdt.add_node(root, Node::new("timer"));
    fdt.node_mut(timer)
        .unwrap()
        .set_property(string_property("compatible", "arm,armv8-timer"));

    let uart = fdt.add_node(soc, Node::new("serial@9000000"));
    fdt.node_mut(uart)
        .unwrap()
        .set_property(string_property("compatible", "arm,pl011"));
    fdt.view_typed_mut(uart)
        .unwrap()
        .set_regs(&[RegInfo::new(0x0900_0000, Some(0x1000))]);
    fdt.node_mut(uart)
        .unwrap()
        .set_property(u32_property("interrupts", &[0, 1, interrupt_flags]));

    let denied = fdt.add_node(soc, Node::new("denied@a000000"));
    fdt.node_mut(denied)
        .unwrap()
        .set_property(string_property("compatible", "vendor,denied"));
    fdt.view_typed_mut(denied)
        .unwrap()
        .set_regs(&[RegInfo::new(0x0a00_0000, Some(0x1000))]);
    fdt.encode().as_ref().to_vec()
}

fn host_fdt_with_rockchip_fiq_debugger_console() -> Vec<u8> {
    let bytes = host_fdt();
    let mut fdt = Fdt::from_bytes(&bytes).unwrap();
    let root = fdt.root_id();
    let aliases = fdt.add_node(root, Node::new("aliases"));
    fdt.node_mut(aliases)
        .unwrap()
        .set_property(string_property("serial2", "/soc/serial@9000000"));
    let chosen = fdt.get_by_path_id("/chosen").unwrap();
    fdt.node_mut(chosen).unwrap().set_property(string_property(
        "bootargs",
        "earlycon=uart8250,mmio32,0x9000000 console=ttyFIQ0 rootwait",
    ));
    let uart = fdt.get_by_path_id("/soc/serial@9000000").unwrap();
    let uart = fdt.node_mut(uart).unwrap();
    uart.set_property(string_property(
        "compatible",
        "rockchip,rk3568-uart\0snps,dw-apb-uart",
    ));
    uart.set_property(string_property("status", "disabled"));

    let fiq = fdt.add_node(root, Node::new("fiq-debugger"));
    let fiq = fdt.node_mut(fiq).unwrap();
    fiq.set_property(string_property("compatible", "rockchip,fiq-debugger"));
    fiq.set_property(u32_property("rockchip,serial-id", &[2]));
    fiq.set_property(u32_property("rockchip,baudrate", &[1_500_000]));
    fiq.set_property(u32_property("interrupts", &[0, 252, 8]));
    fiq.set_property(string_property("status", "okay"));

    fdt.encode().as_ref().to_vec()
}

fn host_fdt_with_non_identity_cpu_unit_addresses() -> Vec<u8> {
    let bytes = host_fdt();
    let mut fdt = Fdt::from_bytes(&bytes).unwrap();
    let cpu0 = fdt.get_by_path_id("/cpus/cpu@0").unwrap();
    fdt.view_typed_mut(cpu0)
        .unwrap()
        .set_regs(&[RegInfo::new(0x100, None)]);
    let cpu1 = fdt.get_by_path_id("/cpus/cpu@1").unwrap();
    fdt.view_typed_mut(cpu1)
        .unwrap()
        .set_regs(&[RegInfo::new(0, None)]);
    fdt.encode().as_ref().to_vec()
}

fn host_fdt_with_pcie_legacy_interrupt_controller() -> Vec<u8> {
    let bytes = host_fdt();
    let mut fdt = Fdt::from_bytes(&bytes).unwrap();
    let soc = fdt.get_by_path_id("/soc").unwrap();
    let pcie = fdt.add_node(soc, Node::new("pcie@fe180000"));
    fdt.node_mut(pcie)
        .unwrap()
        .set_property(string_property("compatible", "rockchip,rk3588-pcie"));
    fdt.view_typed_mut(pcie)
        .unwrap()
        .set_regs(&[RegInfo::new(0xfe18_0000, Some(0x10_0000))]);

    let legacy = fdt.add_node(pcie, Node::new("legacy-interrupt-controller"));
    let legacy = fdt.node_mut(legacy).unwrap();
    legacy.set_property(Property::new("interrupt-controller", Vec::new()));
    legacy.set_property(u32_property("#address-cells", &[0]));
    legacy.set_property(u32_property("#interrupt-cells", &[1]));
    legacy.set_property(u32_property("phandle", &[42]));

    fdt.encode().as_ref().to_vec()
}

fn host_fdt_with_gpio_interrupt_controller() -> Vec<u8> {
    let bytes = host_fdt();
    let mut fdt = Fdt::from_bytes(&bytes).unwrap();
    let soc = fdt.get_by_path_id("/soc").unwrap();
    let gpio = fdt.add_node(soc, Node::new("gpio@b000000"));
    fdt.node_mut(gpio)
        .unwrap()
        .set_property(string_property("compatible", "vendor,gpio-controller"));
    fdt.node_mut(gpio)
        .unwrap()
        .set_property(Property::new("interrupt-controller", Vec::new()));
    fdt.node_mut(gpio)
        .unwrap()
        .set_property(u32_property("#interrupt-cells", &[2]));
    fdt.node_mut(gpio)
        .unwrap()
        .set_property(u32_property("interrupts", &[0, 80, 4]));
    fdt.view_typed_mut(gpio)
        .unwrap()
        .set_regs(&[RegInfo::new(0x0b00_0000, Some(0x1000))]);

    fdt.encode().as_ref().to_vec()
}

fn host_fdt_with_physical_its() -> Vec<u8> {
    let bytes = host_fdt();
    let mut fdt = Fdt::from_bytes(&bytes).unwrap();
    let soc = fdt.get_by_path_id("/soc").unwrap();
    let its = fdt.add_node(soc, Node::new("gic-its@8080000"));
    fdt.node_mut(its)
        .unwrap()
        .set_property(string_property("compatible", "arm,gic-v3-its"));
    fdt.node_mut(its)
        .unwrap()
        .set_property(Property::new("msi-controller", Vec::new()));
    fdt.view_typed_mut(its)
        .unwrap()
        .set_regs(&[RegInfo::new(0x0808_0000, Some(0x2_0000))]);

    fdt.encode().as_ref().to_vec()
}

fn host_fdt_with_pcie_using_physical_its() -> Vec<u8> {
    let bytes = host_fdt_with_physical_its();
    let mut fdt = Fdt::from_bytes(&bytes).unwrap();
    let its = fdt.get_by_path_id("/soc/gic-its@8080000").unwrap();
    fdt.node_mut(its)
        .unwrap()
        .set_property(u32_property("phandle", &[44]));

    let soc = fdt.get_by_path_id("/soc").unwrap();
    let pcie = fdt.add_node(soc, Node::new("pcie@40000000"));
    fdt.node_mut(pcie)
        .unwrap()
        .set_property(string_property("compatible", "pci-host-ecam-generic"));
    fdt.node_mut(pcie)
        .unwrap()
        .set_property(u32_property("msi-parent", &[44]));
    fdt.view_typed_mut(pcie)
        .unwrap()
        .set_regs(&[RegInfo::new(0x4000_0000, Some(0x1000_0000))]);

    fdt.encode().as_ref().to_vec()
}

fn host_fdt_with_psci(method: &str) -> Vec<u8> {
    let bytes = host_fdt();
    let mut fdt = Fdt::from_bytes(&bytes).unwrap();
    let psci = fdt.add_node(fdt.root_id(), Node::new("psci"));
    fdt.node_mut(psci)
        .unwrap()
        .set_property(string_property("compatible", "arm,psci-1.0"));
    fdt.node_mut(psci)
        .unwrap()
        .set_property(string_property("method", method));
    fdt.encode().as_ref().to_vec()
}

fn host_fdt_with_mixed_cpu_interrupt_contexts() -> Vec<u8> {
    let bytes = host_fdt();
    let mut fdt = Fdt::from_bytes(&bytes).unwrap();
    for (cpu_path, phandle) in [("/cpus/cpu@0", 10), ("/cpus/cpu@1", 11)] {
        let cpu = fdt.get_by_path_id(cpu_path).unwrap();
        let intc = fdt.add_node(cpu, Node::new("interrupt-controller"));
        let intc = fdt.node_mut(intc).unwrap();
        intc.set_property(Property::new("interrupt-controller", Vec::new()));
        intc.set_property(u32_property("#interrupt-cells", &[1]));
        intc.set_property(u32_property("phandle", &[phandle]));
    }
    let soc = fdt.get_by_path_id("/soc").unwrap();
    let plic = fdt.add_node(soc, Node::new("plic@c000000"));
    let plic_node = fdt.node_mut(plic).unwrap();
    plic_node.set_property(string_property("compatible", "riscv,plic0"));
    plic_node.set_property(Property::new("interrupt-controller", Vec::new()));
    plic_node.set_property(u32_property("#interrupt-cells", &[1]));
    plic_node.set_property(u32_property(
        "interrupts-extended",
        &[10, 3, 10, 7, 11, 3, 11, 7],
    ));
    fdt.view_typed_mut(plic)
        .unwrap()
        .set_regs(&[RegInfo::new(0x0c00_0000, Some(0x0040_0000))]);
    fdt.encode().as_ref().to_vec()
}

fn host_fdt_with_host_exclusive_dependency() -> Vec<u8> {
    let bytes = host_fdt();
    let mut fdt = Fdt::from_bytes(&bytes).unwrap();
    let serial = fdt.get_by_path_id("/soc/serial@9000000").unwrap();
    fdt.node_mut(serial)
        .unwrap()
        .set_property(u32_property("phandle", &[9]));
    fdt.node_mut(serial)
        .unwrap()
        .set_property(u32_property("#clock-cells", &[0]));
    let soc = fdt.get_by_path_id("/soc").unwrap();
    let consumer = fdt.add_node(soc, Node::new("consumer@a100000"));
    fdt.node_mut(consumer)
        .unwrap()
        .set_property(string_property("compatible", "vendor,consumer"));
    fdt.node_mut(consumer)
        .unwrap()
        .set_property(u32_property("clocks", &[9]));
    fdt.view_typed_mut(consumer)
        .unwrap()
        .set_regs(&[RegInfo::new(0x0a10_0000, Some(0x1000))]);
    let child = fdt.add_node(consumer, Node::new("child"));
    fdt.node_mut(child)
        .unwrap()
        .set_property(string_property("compatible", "vendor,child"));
    let aliases = fdt.add_node(fdt.root_id(), Node::new("aliases"));
    fdt.node_mut(aliases).unwrap().set_property(string_property(
        "blocked-device",
        "/soc/consumer@a100000/child",
    ));
    fdt.encode().as_ref().to_vec()
}

fn host_fdt_with_optional_host_exclusive_dependency() -> Vec<u8> {
    let bytes = host_fdt();
    let mut fdt = Fdt::from_bytes(&bytes).unwrap();
    let serial = fdt.get_by_path_id("/soc/serial@9000000").unwrap();
    fdt.node_mut(serial)
        .unwrap()
        .set_property(u32_property("phandle", &[9]));
    fdt.node_mut(serial)
        .unwrap()
        .set_property(u32_property("#gpio-cells", &[2]));
    let soc = fdt.get_by_path_id("/soc").unwrap();
    let consumer = fdt.add_node(soc, Node::new("consumer@a100000"));
    fdt.node_mut(consumer)
        .unwrap()
        .set_property(string_property("compatible", "vendor,consumer"));
    fdt.node_mut(consumer)
        .unwrap()
        .set_property(u32_property("reset-gpios", &[9, 1, 0]));
    fdt.view_typed_mut(consumer)
        .unwrap()
        .set_regs(&[RegInfo::new(0x0a10_0000, Some(0x1000))]);
    fdt.encode().as_ref().to_vec()
}

fn host_fdt_with_virtual_uart_host_capabilities() -> Vec<u8> {
    let bytes = host_fdt();
    let mut fdt = Fdt::from_bytes(&bytes).unwrap();
    let uart = fdt.get_by_path_id("/soc/serial@9000000").unwrap();
    let uart = fdt.node_mut(uart).unwrap();
    uart.set_property(u32_property("dmas", &[1, 0]));
    uart.set_property(string_property("dma-names", "rx"));
    uart.set_property(u32_property("iommus", &[1, 0]));
    uart.set_property(u32_property("msi-parent", &[1]));
    uart.set_property(u32_property("interrupts-extended", &[1, 0, 1, 4]));
    fdt.encode().as_ref().to_vec()
}

fn string_property(name: &str, value: &str) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_string(value);
    property
}

fn u32_property(name: &str, values: &[u32]) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_u32_ls(values);
    property
}
