use axdevice::{DeviceModelId, DeviceRequirements, InterruptSourceKind, ResourceSlot};
use axvm::machine::{
    Aarch64GicV3Profile, AddressRange, DeviceInstanceId, FdtInterruptEncoding, GuestMemoryRegion,
    HostDeviceId, HostDeviceSelector, HostFdtConfig, HostPlatformSnapshot,
    InterruptControllerProfile, MachineProfile, VirtualDeviceDescriptor, VirtualDeviceSource,
    VmMachinePlanner, VmMachineRequest, generate_host_fdt,
};
use axvm_types::{GuestFirmwareKind, InterruptTriggerMode, VmMachineMode};
use fdt_edit::{Fdt, Node, Property};
use fdt_raw::RegInfo;

#[test]
fn host_fdt_is_filtered_and_rebuilt_from_the_machine_plan() {
    let host = host_fdt();
    let snapshot = HostPlatformSnapshot::from_fdt(7, &host, FdtInterruptEncoding::ArmGic).unwrap();
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
fn dynamic_pl011_uses_gic_cells_fixed_clock_and_console_alias() {
    let host = host_fdt();
    let snapshot = HostPlatformSnapshot::from_fdt(7, &host, FdtInterruptEncoding::ArmGic).unwrap();
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
    let snapshot = HostPlatformSnapshot::from_fdt(7, &host, FdtInterruptEncoding::ArmGic).unwrap();
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
    let snapshot = HostPlatformSnapshot::from_fdt(7, &host, FdtInterruptEncoding::ArmGic).unwrap();
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
fn passthrough_dependency_cannot_reintroduce_a_host_exclusive_device() {
    let host = host_fdt_with_host_exclusive_dependency();
    let snapshot = HostPlatformSnapshot::from_fdt(7, &host, FdtInterruptEncoding::ArmGic).unwrap();
    let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt)
        .with_memory(GuestMemoryRegion::new(
            AddressRange::new(0x8000_0000, 0x2000_0000).unwrap(),
        ));
    let plan = VmMachinePlanner::new(aarch64_profile())
        .plan(&request, &snapshot)
        .unwrap();

    let error = generate_host_fdt(&plan, &snapshot, &HostFdtConfig::new([0])).unwrap_err();

    assert!(error.to_string().contains("host-exclusive"));
    assert!(error.to_string().contains("/soc/serial@9000000"));
}

fn pl011() -> VirtualDeviceDescriptor {
    let requirements = DeviceRequirements::new()
        .with_mmio(ResourceSlot::new("registers").unwrap(), 0x1000, 0x1000)
        .unwrap()
        .with_wired_irq(
            ResourceSlot::new("irq").unwrap(),
            InterruptTriggerMode::LevelTriggered,
            InterruptSourceKind::Software,
        )
        .unwrap();
    VirtualDeviceDescriptor::new(
        DeviceInstanceId::new("console0").unwrap(),
        DeviceModelId::new("arm-pl011").unwrap(),
        requirements,
    )
    .with_compatible("arm,pl011")
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
