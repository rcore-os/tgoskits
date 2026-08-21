use std::{string::String, vec::Vec};

use axdevice::{DeviceFirmwareProperty, FdtContributionSpec, ResolvedDeviceGraph};
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

/// Resolves conventional FDT contributions from the authoritative graph.
pub(crate) fn resolve_fdt_devices(
    graph: &ResolvedDeviceGraph,
) -> AxVmResult<Vec<ResolvedFdtDevice>> {
    graph
        .validate_fdt_support()
        .map_err(|error| AxVmError::invalid_config(std::format!("{error}")))?;
    let mut devices = Vec::new();
    for graph_node in graph.nodes() {
        let Some(contributions) = graph_node.firmware().fdt() else {
            continue;
        };
        let resources = graph.resources_for(graph_node.id())?;
        for contribution in contributions {
            let FdtContributionSpec::Conventional(node) = contribution else {
                continue;
            };
            if node.compatible().is_empty() {
                return Err(AxVmError::invalid_config(std::format!(
                    "device {} has an FDT node without compatible strings",
                    graph_node.id()
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
                                "device {} interrupt exceeds one FDT cell",
                                graph_node.id()
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
                    DeviceFirmwareProperty::Empty { name } => {
                        Ok(ResolvedFdtProperty::Empty(name.clone()))
                    }
                    DeviceFirmwareProperty::U32 { name, value } => {
                        Ok(ResolvedFdtProperty::U32(name.clone(), *value))
                    }
                    DeviceFirmwareProperty::String { name, value } => {
                        Ok(ResolvedFdtProperty::String(name.clone(), value.clone()))
                    }
                    DeviceFirmwareProperty::InterruptInput { name, slot } => {
                        let value = u32::try_from(resources.wired_irq(slot)?.input().value())
                            .map_err(|_| {
                                AxVmError::invalid_config(std::format!(
                                    "device {} interrupt property exceeds one FDT cell",
                                    graph_node.id()
                                ))
                            })?;
                        Ok(ResolvedFdtProperty::U32(name.clone(), value))
                    }
                })
                .collect::<AxVmResult<Vec<_>>>()?;
            devices.push(ResolvedFdtDevice {
                id: graph_node.id().to_string(),
                node_name: node.node_name().into(),
                compatible: node.compatible().to_vec(),
                registers,
                interrupts,
                properties,
            });
        }
    }
    Ok(devices)
}
