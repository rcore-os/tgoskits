use std::{string::String, vec::Vec};

use axdevice::{
    DeviceFirmwareProperty, FdtContributionSpec, FdtNodeSpec, ResolvedDeviceGraph,
    ResolvedDeviceResources,
};
use axdevice_base::{InterruptControllerId, InterruptTrigger};

use crate::{AxVmError, AxVmResult};

/// One conventional FDT node with every resource slot resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedFdtDevice {
    pub(crate) id: String,
    pub(crate) node_name: String,
    pub(crate) compatible: Vec<String>,
    pub(crate) registers: Vec<(u64, u64)>,
    pub(crate) interrupts: Vec<ResolvedFdtInterrupt>,
    pub(crate) properties: Vec<ResolvedFdtProperty>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedFdtInterrupt {
    pub(crate) controller: InterruptControllerId,
    pub(crate) input: u32,
    pub(crate) trigger: InterruptTrigger,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedFdtProperty {
    Empty(String),
    U32(String, u32),
    String(String, String),
}

/// Architecture-owned FDT topology category with resolved controller identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedFdtSpecialKind {
    InterruptController(InterruptControllerId),
    Timer,
    PciHostBridge,
    Console,
    FirmwareTransport,
}

/// One architecture-owned FDT contribution with every resource slot resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedFdtSpecial {
    pub(crate) id: String,
    pub(crate) kind: ResolvedFdtSpecialKind,
    pub(crate) node_name: String,
    pub(crate) compatible: Vec<String>,
    pub(crate) registers: Vec<(u64, u64)>,
    pub(crate) interrupts: Vec<ResolvedFdtInterrupt>,
    pub(crate) properties: Vec<ResolvedFdtProperty>,
}

/// Complete FDT contribution plan selected from one resolved device graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedFdtFirmware {
    pub(crate) devices: Vec<ResolvedFdtDevice>,
    pub(crate) specials: Vec<ResolvedFdtSpecial>,
}

/// Checks that one console contribution describes the same serial runtime.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn fdt_console_matches_serial(
    console: &ResolvedFdtSpecial,
    serial: &crate::machine::ResolvedSerialDevice,
    controller: InterruptControllerId,
) -> bool {
    use crate::machine::{GuestSerialModel, GuestSerialTransport};

    let profile = serial.profile();
    let (node_name, compatible) = match profile.model {
        GuestSerialModel::Pl011 => ("pl011", "arm,pl011"),
        GuestSerialModel::Uart16550 => ("serial", "ns16550a"),
    };
    let GuestSerialTransport::Mmio {
        base,
        length,
        register_shift,
        register_width,
    } = profile.transport
    else {
        return false;
    };
    let Ok(base) = u64::try_from(base) else {
        return false;
    };
    let Ok(length) = u64::try_from(length) else {
        return false;
    };
    let Ok(input) = u32::try_from(profile.irq) else {
        return false;
    };
    console.id == serial.id()
        && console.node_name == node_name
        && console.compatible.len() == 1
        && console
            .compatible
            .first()
            .is_some_and(|item| item == compatible)
        && console.registers.as_slice() == [(base, length)]
        && matches!(
            console.interrupts.as_slice(),
            [interrupt] if interrupt.controller == controller && interrupt.input == input
        )
        && console.properties.len() == 3
        && has_u32_property(&console.properties, "clock-frequency", profile.clock_hz)
        && has_u32_property(&console.properties, "reg-shift", u32::from(register_shift))
        && has_u32_property(
            &console.properties,
            "reg-io-width",
            u32::try_from(register_width.size())
                .expect("a serial access width is at most eight bytes"),
        )
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn has_u32_property(properties: &[ResolvedFdtProperty], name: &str, value: u32) -> bool {
    properties
        .iter()
        .any(|property| matches!(property, ResolvedFdtProperty::U32(key, item) if key == name && *item == value))
}

/// Resolves conventional FDT contributions from the authoritative graph.
#[cfg(test)]
pub(crate) fn resolve_fdt_devices(
    graph: &ResolvedDeviceGraph,
) -> AxVmResult<Vec<ResolvedFdtDevice>> {
    Ok(resolve_fdt_firmware(graph)?.devices)
}

/// Resolves every FDT contribution, including architecture-owned topology.
pub(crate) fn resolve_fdt_firmware(graph: &ResolvedDeviceGraph) -> AxVmResult<ResolvedFdtFirmware> {
    graph
        .validate_fdt_support()
        .map_err(|error| AxVmError::invalid_config(std::format!("{error}")))?;
    let mut devices = Vec::new();
    let mut specials = Vec::new();
    for graph_node in graph.nodes() {
        let Some(contributions) = graph_node.firmware().fdt() else {
            continue;
        };
        let resources = graph.resources_for(graph_node.id())?;
        for contribution in contributions {
            let (kind, node) = match contribution {
                FdtContributionSpec::Conventional(node) => (None, node),
                FdtContributionSpec::InterruptController { controller, node } => (
                    Some(ResolvedFdtSpecialKind::InterruptController(*controller)),
                    node,
                ),
                FdtContributionSpec::Timer(node) => (Some(ResolvedFdtSpecialKind::Timer), node),
                FdtContributionSpec::PciHostBridge(node) => {
                    (Some(ResolvedFdtSpecialKind::PciHostBridge), node)
                }
                FdtContributionSpec::Console(node) => (Some(ResolvedFdtSpecialKind::Console), node),
                FdtContributionSpec::FirmwareTransport(node) => {
                    (Some(ResolvedFdtSpecialKind::FirmwareTransport), node)
                }
            };
            let resolved = resolve_node(graph_node.id().as_str(), node, resources)?;
            if let Some(kind) = kind {
                specials.push(ResolvedFdtSpecial {
                    id: graph_node.id().to_string(),
                    kind,
                    node_name: resolved.node_name,
                    compatible: resolved.compatible,
                    registers: resolved.registers,
                    interrupts: resolved.interrupts,
                    properties: resolved.properties,
                });
            } else {
                devices.push(ResolvedFdtDevice {
                    id: graph_node.id().to_string(),
                    node_name: resolved.node_name,
                    compatible: resolved.compatible,
                    registers: resolved.registers,
                    interrupts: resolved.interrupts,
                    properties: resolved.properties,
                });
            }
        }
    }
    Ok(ResolvedFdtFirmware { devices, specials })
}

struct ResolvedFdtNode {
    node_name: String,
    compatible: Vec<String>,
    registers: Vec<(u64, u64)>,
    interrupts: Vec<ResolvedFdtInterrupt>,
    properties: Vec<ResolvedFdtProperty>,
}

fn resolve_node(
    device_id: &str,
    node: &FdtNodeSpec,
    resources: &ResolvedDeviceResources,
) -> AxVmResult<ResolvedFdtNode> {
    if node.compatible().is_empty() {
        return Err(AxVmError::invalid_config(std::format!(
            "device {device_id} has an FDT node without compatible strings"
        )));
    }
    let registers = node
        .register_slots()
        .iter()
        .map(|slot| resources.mmio(slot))
        .collect::<Result<Vec<_>, _>>()?;
    let interrupts = node
        .interrupt_slots()
        .iter()
        .map(|slot| {
            let irq = resources.wired_irq(slot)?;
            Ok(ResolvedFdtInterrupt {
                controller: irq.controller(),
                input: u32::try_from(irq.input().value()).map_err(|_| {
                    AxVmError::invalid_config(std::format!(
                        "device {device_id} interrupt exceeds one FDT cell"
                    ))
                })?,
                trigger: irq.trigger(),
            })
        })
        .collect::<AxVmResult<Vec<_>>>()?;
    let properties = node
        .properties()
        .iter()
        .map(|property| match property {
            DeviceFirmwareProperty::Empty { name } => Ok(ResolvedFdtProperty::Empty(name.clone())),
            DeviceFirmwareProperty::U32 { name, value } => {
                Ok(ResolvedFdtProperty::U32(name.clone(), *value))
            }
            DeviceFirmwareProperty::String { name, value } => {
                Ok(ResolvedFdtProperty::String(name.clone(), value.clone()))
            }
            DeviceFirmwareProperty::InterruptInput { name, slot } => {
                let value =
                    u32::try_from(resources.wired_irq(slot)?.input().value()).map_err(|_| {
                        AxVmError::invalid_config(std::format!(
                            "device {device_id} interrupt property exceeds one FDT cell"
                        ))
                    })?;
                Ok(ResolvedFdtProperty::U32(name.clone(), value))
            }
        })
        .collect::<AxVmResult<Vec<_>>>()?;
    Ok(ResolvedFdtNode {
        node_name: node.node_name().into(),
        compatible: node.compatible().to_vec(),
        registers,
        interrupts,
        properties,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axdevice::*;
    use axdevice_base::{InterruptControllerId, InterruptTrigger};

    use super::resolve_fdt_devices;

    struct InvalidFirmwareTransportPropertyModel;

    impl DeviceModel for InvalidFirmwareTransportPropertyModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            DeviceRequirements::new()
                .with_mmio(
                    ResourceSlot::new("registers")?,
                    0x1000,
                    0x1000,
                    ResourceRequest::Auto,
                )?
                .with_wired_irq(
                    ResourceSlot::new("irq")?,
                    InterruptControllerId::new(0),
                    InterruptTrigger::LevelTriggered,
                    axdevice_base::InterruptSharing::Exclusive,
                    ResourceRequest::Auto,
                )
        }

        fn firmware(&self) -> DeviceFirmwareSpec {
            DeviceFirmwareSpec::interfaces(
                Some(std::vec![FdtContributionSpec::FirmwareTransport(
                    FdtNodeSpec::new("fw_cfg")
                        .with_compatible("qemu,fw-cfg-mmio")
                        .with_register(
                            ResourceSlot::new("registers").expect("static slot is valid")
                        )
                        .with_interrupt_input_property(
                            "interrupt-input",
                            ResourceSlot::new("misspelled-irq")
                                .expect("static regression slot is valid"),
                        ),
                )]),
                None,
            )
        }

        fn build(
            &self,
            _context: &mut DeviceBuildContext<'_>,
        ) -> DeviceManagerResult<DeviceBundle> {
            unreachable!("firmware-resolution regression does not build devices")
        }
    }

    #[test]
    fn special_fdt_contribution_rejects_unknown_property_slot() {
        let mut builder = DeviceGraphBuilder::new();
        builder
            .add(DeviceNodeSpec::virtual_device(
                DeviceNodeId::new("fw-cfg").unwrap(),
                Arc::new(InvalidFirmwareTransportPropertyModel),
            ))
            .unwrap();
        let mut pools = ResourcePools::new();
        pools.add_auto_mmio(0x1000..0x3000).unwrap();
        pools
            .add_auto_controller_inputs(
                InterruptControllerId::new(0),
                axdevice_base::ControllerInputId::new(1)..axdevice_base::ControllerInputId::new(2),
            )
            .unwrap();
        let graph = builder.declare().unwrap().resolve(pools).unwrap();

        assert!(resolve_fdt_devices(&graph).is_err());
    }
}
