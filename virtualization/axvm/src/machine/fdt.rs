//! Host FDT normalization for VM machine planning.

use alloc::{format, string::String, vec::Vec};

use axvm_types::InterruptTriggerMode;
use fdt_edit::{Fdt, NodeType, PciSpace};

use super::{
    AddressRange, HostDeviceDependency, HostDeviceDescriptor, HostDeviceId, HostDeviceOwnership,
    HostFirmwareActivation, HostInterruptResource, HostPlatformSnapshot, MachinePlanError,
    MachinePlanResult, host_fdt::dependencies::FdtDependencyIndex,
};

/// Interrupt-cell encoding used by a host device tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FdtInterruptEncoding {
    /// Arm GIC three-cell encoding (`type`, `number`, `flags`).
    ArmGic,
    /// A controller whose first cell is the complete hardware interrupt ID.
    FirstCell,
}

impl HostPlatformSnapshot {
    /// Normalizes a host device tree into stable planning descriptors.
    ///
    /// The built-in ownership classifier is deliberately conservative for
    /// CPUs, memory, architectural interrupt infrastructure, timers, and host
    /// consoles. Cascaded device controllers remain assignable with their
    /// parent devices. A platform claim provider still performs the
    /// authoritative live ownership transition before any `Assignable` or
    /// `Transferable` device is exposed to a VM.
    pub fn from_fdt(
        generation: u64,
        bytes: &[u8],
        interrupt_encoding: FdtInterruptEncoding,
    ) -> MachinePlanResult<Self> {
        let fdt = Fdt::from_bytes(bytes).map_err(|error| MachinePlanError::InvalidFirmware {
            detail: format!("failed to parse host FDT: {error:?}"),
        })?;
        let mut snapshot = Self::new(generation);
        snapshot.set_source_fdt(bytes);
        let console_path = selected_console_path(&fdt);
        let dependencies = FdtDependencyIndex::new(&fdt);

        for node_id in fdt.iter_node_ids() {
            let Some(node) = fdt.node(node_id) else {
                continue;
            };
            let path = fdt.path_of(node_id);
            let compatibles = node.compatibles().map(String::from).collect::<Vec<_>>();
            let mut descriptor = HostDeviceDescriptor::described(
                HostDeviceId::new(path.clone())?,
                classify_ownership(&path, node, &compatibles, console_path.as_deref()),
            )
            .with_firmware_activation(HostFirmwareActivation::Preserve);
            for compatible in compatibles {
                descriptor = descriptor.with_compatible(compatible);
            }
            for dependency in dependencies.dependencies(node) {
                descriptor = descriptor.with_dependency(HostDeviceDependency::new(
                    HostDeviceId::new(dependency.provider())?,
                    dependency.property(),
                    dependency.kind(),
                )?);
            }
            if let Some(parent) = parent_path(&path) {
                descriptor = descriptor.with_dependency(HostDeviceDependency::new(
                    HostDeviceId::new(parent)?,
                    "fdt-parent",
                    super::HostDeviceDependencyKind::Required,
                )?);
            }

            let mut ranges = fdt
                .view_typed(node_id)
                .map(|view| {
                    view.regs()
                        .into_iter()
                        .filter_map(|reg| {
                            AddressRange::new(reg.address, reg.size.unwrap_or(0)).ok()
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if let Some(NodeType::Pci(pci)) = fdt.view_typed(node_id) {
                ranges.extend(
                    pci.ranges()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|range| range.space != PciSpace::IO)
                        .filter_map(|range| AddressRange::new(range.cpu_address, range.size).ok()),
                );
            }
            for range in ranges {
                if is_io_aperture(&path, node) {
                    snapshot = snapshot.with_io_aperture(range);
                }
                descriptor = descriptor.with_mmio(range);
            }

            if let Some(view) = fdt.view_typed(node_id) {
                let interrupts = view.interrupts();
                if node.get_property("interrupts").is_some() && interrupts.is_empty() {
                    return Err(MachinePlanError::InvalidFirmware {
                        detail: format!(
                            "host FDT device '{path}' has interrupts but no resolvable \
                             interrupt-parent"
                        ),
                    });
                }
                for interrupt in interrupts {
                    let controller_id = fdt
                        .get_by_phandle_id(interrupt.interrupt_parent)
                        .ok_or_else(|| MachinePlanError::InvalidFirmware {
                            detail: format!(
                                "host FDT device '{path}' refers to a missing interrupt controller"
                            ),
                        })?;
                    if !controller_supports_encoding(&fdt, controller_id, interrupt_encoding) {
                        // This is a controller-local input, not an input of the VM's root
                        // interrupt controller. The original specifier remains in the source
                        // FDT for a passthrough controller cascade; only root inputs enter the
                        // machine plan's host IRQ ownership and delivery topology.
                        continue;
                    }
                    let controller = HostDeviceId::new(fdt.path_of(controller_id))?;
                    let (input, trigger) =
                        decode_interrupt(interrupt_encoding, interrupt.specifier.as_slice())
                            .map_err(|detail| MachinePlanError::InvalidFirmware {
                                detail: format!(
                                    "host FDT device '{path}' has invalid interrupt: {detail}"
                                ),
                            })?;
                    descriptor = descriptor.with_interrupt(HostInterruptResource::fdt(
                        input,
                        trigger,
                        controller,
                        interrupt.specifier,
                    )?);
                }
            }
            snapshot = snapshot.with_device(descriptor);
        }
        if let Some(path) = console_path {
            snapshot.set_console_device(HostDeviceId::new(path)?)?;
        }
        Ok(snapshot)
    }
}

fn parent_path(path: &str) -> Option<&str> {
    if path == "/" {
        return None;
    }
    let (parent, _) = path.rsplit_once('/')?;
    Some(if parent.is_empty() { "/" } else { parent })
}

fn controller_supports_encoding(
    fdt: &Fdt,
    controller_id: fdt_edit::NodeId,
    encoding: FdtInterruptEncoding,
) -> bool {
    let Some(controller) = fdt.node(controller_id) else {
        return false;
    };
    match encoding {
        FdtInterruptEncoding::ArmGic => controller
            .compatibles()
            .any(|compatible| compatible == "arm,gic-v3"),
        FdtInterruptEncoding::FirstCell => controller.compatibles().any(|compatible| {
            compatible.starts_with("riscv,plic") || compatible.starts_with("riscv,cpu-intc")
        }),
    }
}

fn selected_console_path(fdt: &Fdt) -> Option<String> {
    let chosen = fdt.get_by_path("/chosen")?;
    ["stdout-path", "linux,stdout-path"]
        .into_iter()
        .find_map(|name| {
            chosen
                .as_node()
                .get_property(name)?
                .as_str()
                .and_then(|value| resolve_console_path(fdt, value))
        })
}

fn resolve_console_path(fdt: &Fdt, value: &str) -> Option<String> {
    let reference = value.split(':').next().unwrap_or(value);
    let path = if reference.starts_with('/') {
        reference
    } else {
        fdt.get_by_path("/aliases")?
            .as_node()
            .get_property(reference)?
            .as_str()?
    };
    fdt.get_by_path(path).map(|_| String::from(path))
}

fn classify_ownership(
    path: &str,
    node: &fdt_edit::Node,
    compatibles: &[alloc::string::String],
    console_path: Option<&str>,
) -> HostDeviceOwnership {
    if node
        .get_property("status")
        .and_then(|property| property.as_str())
        == Some("disabled")
    {
        return HostDeviceOwnership::Unrepresentable;
    }
    if path == "/"
        || matches!(path, "/aliases" | "/chosen" | "/cpus")
        || path.starts_with("/cpus/")
        || (!node.children().is_empty() && node.get_property("reg").is_none())
        || is_embedded_interrupt_controller(node)
        || path.ends_with("-clock")
        || compatibles.iter().any(|compatible| {
            compatible.contains("fixed-clock")
                || matches!(compatible.as_str(), "simple-bus" | "simple-mfd")
        })
    {
        return HostDeviceOwnership::Structural;
    }
    if path.starts_with("/memory")
        || path.starts_with("/reserved-memory")
        || compatibles.iter().any(|compatible| {
            compatible.contains("armv8-timer")
                || is_architectural_interrupt_infrastructure(compatible)
                || compatible == "rockchip,fiq-debugger"
        })
        || console_path == Some(path)
        || (is_pl011(compatibles) && console_path.is_none())
    {
        return HostDeviceOwnership::HostExclusive;
    }
    if compatibles.iter().any(|compatible| {
        compatible.contains("sdhci")
            || compatible.contains("dw-mshc")
            || compatible.contains("dwmmc")
    }) {
        return HostDeviceOwnership::Transferable;
    }
    HostDeviceOwnership::Assignable
}

fn is_embedded_interrupt_controller(node: &fdt_edit::Node) -> bool {
    node.get_property("interrupt-controller").is_some() && node.get_property("reg").is_none()
}

fn is_architectural_interrupt_infrastructure(compatible: &str) -> bool {
    matches!(
        compatible,
        "arm,gic-v3" | "arm,gic-v3-its" | "arm,cortex-a15-gic" | "arm,gic-400"
    ) || compatible.starts_with("riscv,plic")
        || compatible.starts_with("sifive,plic")
}

fn is_pl011(compatibles: &[String]) -> bool {
    compatibles
        .iter()
        .any(|compatible| compatible == "arm,pl011")
}

fn is_io_aperture(path: &str, node: &fdt_edit::Node) -> bool {
    !path.starts_with("/memory")
        && !path.starts_with("/reserved-memory")
        && !node.name().starts_with("memory@")
}

fn decode_interrupt(
    encoding: FdtInterruptEncoding,
    cells: &[u32],
) -> Result<(u32, InterruptTriggerMode), String> {
    match encoding {
        FdtInterruptEncoding::FirstCell => {
            let input = cells
                .first()
                .copied()
                .filter(|input| *input != 0)
                .ok_or_else(|| String::from("controller input 0 is reserved or absent"))?;
            Ok((input, InterruptTriggerMode::LevelTriggered))
        }
        FdtInterruptEncoding::ArmGic => decode_gic_interrupt(cells),
    }
}

fn decode_gic_interrupt(cells: &[u32]) -> Result<(u32, InterruptTriggerMode), String> {
    let [interrupt_type, number, flags] = cells else {
        return Err(format!(
            "GIC specifier must contain exactly three cells, got {}",
            cells.len()
        ));
    };
    let input = match *interrupt_type {
        0 if *number < 988 => number + 32,
        1 if *number < 16 => number + 16,
        0 => return Err(format!("GIC SPI number {number} is outside 0..988")),
        1 => return Err(format!("GIC PPI number {number} is outside 0..16")),
        other => return Err(format!("unsupported GIC interrupt type {other}")),
    };
    let trigger = match flags & 0xf {
        1 | 2 => InterruptTriggerMode::EdgeTriggered,
        4 | 8 => InterruptTriggerMode::LevelTriggered,
        value => return Err(format!("unsupported GIC trigger flags {value:#x}")),
    };
    Ok((input, trigger))
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use axvm_types::{GuestFirmwareKind, VmMachineMode};
    use fdt_edit::{Fdt, Node, Property};
    use fdt_raw::RegInfo;

    use super::*;
    use crate::machine::{
        DeviceDisposition, HostConsoleEvidence, HostConsoleLocation, HostDeviceAssignment,
        MachineProfile, VmMachinePlanner, VmMachineRequest,
    };

    #[test]
    fn host_console_is_protected_but_remains_a_virtual_template() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let uart = fdt.add_node(root, Node::new("serial@9000000"));
        fdt.node_mut(uart)
            .unwrap()
            .set_property(string_list("compatible", &["arm,pl011", "arm,primecell"]));
        fdt.view_typed_mut(uart)
            .unwrap()
            .set_regs(&[RegInfo::new(0x0900_0000, Some(0x1000))]);

        let snapshot =
            HostPlatformSnapshot::from_fdt(3, fdt.encode().as_ref(), FdtInterruptEncoding::ArmGic)
                .unwrap();

        let uart = snapshot
            .devices()
            .iter()
            .find(|device| device.id().as_str() == "/serial@9000000")
            .unwrap();
        assert_eq!(uart.ownership(), HostDeviceOwnership::HostExclusive);
        assert_eq!(uart.mmio()[0].base(), 0x0900_0000);
    }

    #[test]
    fn only_the_firmware_selected_pl011_is_treated_as_the_host_console() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let aliases = fdt.add_node(root, Node::new("aliases"));
        fdt.node_mut(aliases)
            .unwrap()
            .set_property(string_property("serial0", "/serial@2800c000"));
        fdt.node_mut(aliases)
            .unwrap()
            .set_property(string_property("serial1", "/serial@2800d000"));
        let chosen = fdt.add_node(root, Node::new("chosen"));
        fdt.node_mut(chosen)
            .unwrap()
            .set_property(string_property("stdout-path", "serial1:115200n8"));
        for (name, base) in [
            ("serial@2800c000", 0x2800_c000),
            ("serial@2800d000", 0x2800_d000),
        ] {
            let uart = fdt.add_node(root, Node::new(name));
            fdt.node_mut(uart)
                .unwrap()
                .set_property(string_list("compatible", &["arm,pl011", "arm,primecell"]));
            fdt.view_typed_mut(uart)
                .unwrap()
                .set_regs(&[RegInfo::new(base, Some(0x1000))]);
        }

        let mut snapshot =
            HostPlatformSnapshot::from_fdt(5, fdt.encode().as_ref(), FdtInterruptEncoding::ArmGic)
                .unwrap();
        assert_eq!(
            snapshot.console_device().map(HostDeviceId::as_str),
            Some("/serial@2800d000")
        );
        {
            let ownership = |path: &str| {
                snapshot
                    .devices()
                    .iter()
                    .find(|device| device.id().as_str() == path)
                    .unwrap()
                    .ownership()
            };
            assert_eq!(
                ownership("/serial@2800c000"),
                HostDeviceOwnership::Assignable
            );
            assert_eq!(
                ownership("/serial@2800d000"),
                HostDeviceOwnership::HostExclusive
            );
        }

        snapshot
            .grant_console_transfer(
                HostConsoleLocation::Device(HostDeviceId::new("/serial@2800d000").unwrap()),
                HostConsoleEvidence::Firmware,
            )
            .unwrap();
        assert_eq!(
            snapshot
                .devices()
                .iter()
                .find(|device| device.id().as_str() == "/serial@2800d000")
                .unwrap()
                .ownership(),
            HostDeviceOwnership::Transferable
        );
    }

    #[test]
    fn authoritative_console_grant_authorizes_disabled_console_resources() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let chosen = fdt.add_node(root, Node::new("chosen"));
        fdt.node_mut(chosen)
            .unwrap()
            .set_property(string_property("stdout-path", "/serial@feb50000:1500000"));
        let uart = fdt.add_node(root, Node::new("serial@feb50000"));
        {
            let node = fdt.node_mut(uart).unwrap();
            node.set_property(string_list(
                "compatible",
                &["rockchip,rk3588-uart", "snps,dw-apb-uart"],
            ));
            node.set_property(string_property("status", "disabled"));
        }
        fdt.view_typed_mut(uart)
            .unwrap()
            .set_regs(&[RegInfo::new(0xfeb5_0000, Some(0x1000))]);

        let mut snapshot =
            HostPlatformSnapshot::from_fdt(6, fdt.encode().as_ref(), FdtInterruptEncoding::ArmGic)
                .unwrap();
        let console = HostDeviceId::new("/serial@feb50000").unwrap();
        assert_eq!(
            snapshot
                .devices()
                .iter()
                .find(|device| device.id() == &console)
                .unwrap()
                .ownership(),
            HostDeviceOwnership::Unrepresentable
        );

        assert!(
            snapshot
                .grant_console_transfer(
                    HostConsoleLocation::Device(console.clone()),
                    HostConsoleEvidence::Firmware,
                )
                .is_err()
        );
        snapshot
            .grant_console_transfer(
                HostConsoleLocation::MmioBase(0xfeb5_0000),
                HostConsoleEvidence::LivePlatform,
            )
            .unwrap();

        assert_eq!(
            snapshot
                .devices()
                .iter()
                .find(|device| device.id() == &console)
                .unwrap()
                .ownership(),
            HostDeviceOwnership::Transferable
        );
        assert_eq!(
            snapshot
                .devices()
                .iter()
                .find(|device| device.id() == &console)
                .unwrap()
                .firmware_activation(),
            HostFirmwareActivation::Preserve
        );
    }

    #[test]
    fn authoritative_console_grant_rejects_ambiguous_mmio_base() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        for name in ["serial@feb50000", "debug-uart@feb50000"] {
            let uart = fdt.add_node(root, Node::new(name));
            fdt.node_mut(uart)
                .unwrap()
                .set_property(string_list("compatible", &["snps,dw-apb-uart"]));
            fdt.view_typed_mut(uart)
                .unwrap()
                .set_regs(&[RegInfo::new(0xfeb5_0000, Some(0x1000))]);
        }
        let mut snapshot =
            HostPlatformSnapshot::from_fdt(7, fdt.encode().as_ref(), FdtInterruptEncoding::ArmGic)
                .unwrap();

        let result = snapshot.grant_console_transfer(
            HostConsoleLocation::MmioBase(0xfeb5_0000),
            HostConsoleEvidence::LivePlatform,
        );

        assert!(matches!(
            result,
            Err(MachinePlanError::InvalidFirmware { .. })
        ));
    }

    #[test]
    fn generic_primecell_device_is_not_treated_as_the_host_console() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let gpio = fdt.add_node(root, Node::new("gpio@9030000"));
        fdt.node_mut(gpio)
            .unwrap()
            .set_property(string_list("compatible", &["arm,pl061", "arm,primecell"]));
        fdt.view_typed_mut(gpio)
            .unwrap()
            .set_regs(&[RegInfo::new(0x0903_0000, Some(0x1000))]);

        let snapshot =
            HostPlatformSnapshot::from_fdt(4, fdt.encode().as_ref(), FdtInterruptEncoding::ArmGic)
                .unwrap();

        let gpio = snapshot
            .devices()
            .iter()
            .find(|device| device.id().as_str() == "/gpio@9030000")
            .unwrap();
        assert_eq!(gpio.ownership(), HostDeviceOwnership::Assignable);
    }

    #[test]
    fn physical_its_is_host_exclusive_without_an_isolated_backend() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let its = fdt.add_node(root, Node::new("its@8080000"));
        fdt.node_mut(its)
            .unwrap()
            .set_property(string_property("compatible", "arm,gic-v3-its"));

        let snapshot =
            HostPlatformSnapshot::from_fdt(8, fdt.encode().as_ref(), FdtInterruptEncoding::ArmGic)
                .unwrap();
        let its = snapshot
            .devices()
            .iter()
            .find(|device| device.id().as_str() == "/its@8080000")
            .unwrap();

        assert_eq!(its.ownership(), HostDeviceOwnership::HostExclusive);
    }

    #[test]
    fn resource_less_bus_nodes_are_structural_not_claimable_devices() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let soc = fdt.add_node(root, Node::new("soc"));
        fdt.node_mut(soc)
            .unwrap()
            .set_property(string_list("compatible", &["simple-bus"]));
        fdt.add_node(soc, Node::new("device@1000"));

        let snapshot =
            HostPlatformSnapshot::from_fdt(3, fdt.encode().as_ref(), FdtInterruptEncoding::ArmGic)
                .unwrap();
        let soc = snapshot
            .devices()
            .iter()
            .find(|device| device.id().as_str() == "/soc")
            .unwrap();

        assert_eq!(soc.ownership(), HostDeviceOwnership::Structural);
    }

    #[test]
    fn nested_controller_interrupts_are_not_decoded_as_root_inputs() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let gpio = fdt.add_node(root, Node::new("gpio@1000"));
        let gpio_node = fdt.node_mut(gpio).unwrap();
        gpio_node.set_property(Property::new("interrupt-controller", vec![]));
        gpio_node.set_property(u32_property("#interrupt-cells", &[2]));
        gpio_node.set_property(u32_property("phandle", &[2]));

        let consumer = fdt.add_node(root, Node::new("consumer@2000"));
        let consumer_node = fdt.node_mut(consumer).unwrap();
        consumer_node.set_property(string_property("compatible", "vendor,consumer"));
        consumer_node.set_property(u32_property("interrupt-parent", &[2]));
        consumer_node.set_property(u32_property("interrupts", &[5, 4]));

        let snapshot =
            HostPlatformSnapshot::from_fdt(3, fdt.encode().as_ref(), FdtInterruptEncoding::ArmGic)
                .unwrap();
        let consumer = snapshot
            .devices()
            .iter()
            .find(|device| device.id().as_str() == "/consumer@2000")
            .unwrap();

        assert_eq!(consumer.ownership(), HostDeviceOwnership::Assignable);
        assert!(consumer.interrupts().is_empty());
    }

    #[test]
    fn firmware_description_does_not_authorize_physical_device_assignment() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let device = fdt.add_node(root, Node::new("device@2000"));
        fdt.node_mut(device)
            .unwrap()
            .set_property(string_property("compatible", "vendor,leaf-device"));
        fdt.view_typed_mut(device)
            .unwrap()
            .set_regs(&[RegInfo::new(0x2000, Some(0x1000))]);

        let snapshot =
            HostPlatformSnapshot::from_fdt(9, fdt.encode().as_ref(), FdtInterruptEncoding::ArmGic)
                .unwrap();
        let profile =
            MachineProfile::new(AddressRange::new(0x1_0000, 0x1_0000).unwrap(), 32..=63).unwrap();
        let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);
        let plan = VmMachinePlanner::new(profile)
            .plan(&request, &snapshot)
            .unwrap();
        let device = plan
            .host_devices()
            .iter()
            .find(|device| device.id().as_str() == "/device@2000")
            .unwrap();

        assert_eq!(device.disposition(), DeviceDisposition::Unrepresentable);
        assert!(plan.claims().is_empty());
        assert!(
            !plan
                .identity_mappings()
                .iter()
                .any(|range| range.contains(0x2000))
        );
    }

    #[test]
    fn structural_mmio_requires_physical_assignment_authority() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let syscon = fdt.add_node(root, Node::new("syscon@3000"));
        fdt.node_mut(syscon)
            .unwrap()
            .set_property(string_list("compatible", &["vendor,syscon", "simple-mfd"]));
        fdt.view_typed_mut(syscon)
            .unwrap()
            .set_regs(&[RegInfo::new(0x3000, Some(0x1000))]);

        let snapshot =
            HostPlatformSnapshot::from_fdt(10, fdt.encode().as_ref(), FdtInterruptEncoding::ArmGic)
                .unwrap();
        let profile =
            MachineProfile::new(AddressRange::new(0x1_0000, 0x1_0000).unwrap(), 32..=63).unwrap();
        let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);
        let plan = VmMachinePlanner::new(profile)
            .plan(&request, &snapshot)
            .unwrap();
        let syscon = plan
            .host_devices()
            .iter()
            .find(|device| device.id().as_str() == "/syscon@3000")
            .unwrap();

        assert_eq!(syscon.disposition(), DeviceDisposition::Unrepresentable);
        assert!(plan.claims().is_empty());
        assert!(
            !plan
                .identity_mappings()
                .iter()
                .any(|range| range.contains(0x3000))
        );
    }

    #[test]
    fn trusted_assignment_authority_exposes_and_claims_physical_resources() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let device = fdt.add_node(root, Node::new("device@2000"));
        fdt.node_mut(device)
            .unwrap()
            .set_property(string_property("compatible", "vendor,leaf-device"));
        fdt.view_typed_mut(device)
            .unwrap()
            .set_regs(&[RegInfo::new(0x2000, Some(0x1000))]);
        let syscon = fdt.add_node(root, Node::new("syscon@3000"));
        fdt.node_mut(syscon)
            .unwrap()
            .set_property(string_list("compatible", &["vendor,syscon", "simple-mfd"]));
        fdt.view_typed_mut(syscon)
            .unwrap()
            .set_regs(&[RegInfo::new(0x3000, Some(0x1000))]);

        let mut snapshot =
            HostPlatformSnapshot::from_fdt(11, fdt.encode().as_ref(), FdtInterruptEncoding::ArmGic)
                .unwrap();
        let device = HostDeviceId::new("/device@2000").unwrap();
        let syscon = HostDeviceId::new("/syscon@3000").unwrap();
        snapshot
            .grant_device_assignment(&device, HostDeviceAssignment::StaticPartition)
            .unwrap();
        snapshot
            .grant_device_assignment(&syscon, HostDeviceAssignment::StaticPartition)
            .unwrap();
        let profile =
            MachineProfile::new(AddressRange::new(0x1_0000, 0x1_0000).unwrap(), 32..=63).unwrap();
        let request = VmMachineRequest::new(VmMachineMode::Passthrough, GuestFirmwareKind::Fdt);

        let plan = VmMachinePlanner::new(profile)
            .plan(&request, &snapshot)
            .unwrap();

        assert_eq!(
            plan.host_devices()
                .iter()
                .find(|planned| planned.id() == &device)
                .unwrap()
                .disposition(),
            DeviceDisposition::Passthrough
        );
        assert_eq!(
            plan.host_devices()
                .iter()
                .find(|planned| planned.id() == &syscon)
                .unwrap()
                .disposition(),
            DeviceDisposition::Structural
        );
        assert!(plan.claims().contains(&device));
        assert!(plan.claims().contains(&syscon));
        assert!(
            plan.identity_mappings()
                .iter()
                .any(|range| range.contains(0x2000))
        );
        assert!(
            plan.identity_mappings()
                .iter()
                .any(|range| range.contains(0x3000))
        );
    }

    fn string_list(name: &str, values: &[&str]) -> Property {
        let mut bytes = vec![];
        for value in values {
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }
        Property::new(name, bytes)
    }

    fn string_property(name: &str, value: &str) -> Property {
        let mut property = Property::new(name, vec![]);
        property.set_string(value);
        property
    }

    fn u32_property(name: &str, values: &[u32]) -> Property {
        let mut property = Property::new(name, vec![]);
        property.set_u32_ls(values);
        property
    }
}
