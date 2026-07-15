//! Strict GIC SPI decoding for selected host-FDT device nodes.

use alloc::{string::ToString, vec::Vec};

use axvm_types::Aarch64GicSpi;
use fdt_edit::{Fdt, NodeId, Phandle};

use crate::{AxVmError, AxVmResult, ForwardedIrqConfigError, config::Aarch64ForwardedIrq};

pub(super) fn decode_gic_spi_routes(
    host_fdt: &Fdt,
    node_path: &str,
) -> AxVmResult<Vec<Aarch64ForwardedIrq>> {
    let node_id = host_fdt.get_by_path_id(node_path).ok_or_else(|| {
        route_error(ForwardedIrqConfigError::InvalidSelection {
            selection: node_path.to_string(),
        })
    })?;
    let node = host_fdt.node(node_id).ok_or_else(|| {
        route_error(ForwardedIrqConfigError::InvalidSelection {
            selection: node_path.to_string(),
        })
    })?;

    if let Some(property) = node.get_property("interrupts-extended") {
        return decode_extended(host_fdt, node_path, property.get_u32_iter().collect());
    }

    let raw = node
        .get_property("interrupts")
        .map(|property| property.get_u32_iter().collect::<Vec<_>>())
        .unwrap_or_default();
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let (controller_id, phandle) = resolve_interrupt_parent(host_fdt, node_id, node_path, &raw)?;
    decode_controller_specifiers(host_fdt, node_path, controller_id, phandle, &raw)
}

fn decode_extended(
    host_fdt: &Fdt,
    node_path: &str,
    raw: Vec<u32>,
) -> AxVmResult<Vec<Aarch64ForwardedIrq>> {
    let mut routes = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        let phandle = raw[index];
        let remaining = &raw[index + 1..];
        let controller_id = controller_by_phandle(host_fdt, node_path, phandle, remaining)?;
        let cells = interrupt_cells(host_fdt, node_path, controller_id, Some(phandle), remaining)?;
        if remaining.len() < cells {
            return Err(route_error(ForwardedIrqConfigError::TruncatedSpecifier {
                node: node_path.to_string(),
                controller: host_fdt.path_of(controller_id),
                phandle: Some(phandle),
                raw: remaining.to_vec(),
            }));
        }
        routes.push(decode_gic_specifier(
            host_fdt,
            node_path,
            controller_id,
            Some(phandle),
            &remaining[..cells],
        )?);
        index += cells + 1;
    }
    Ok(routes)
}

fn decode_controller_specifiers(
    host_fdt: &Fdt,
    node_path: &str,
    controller_id: NodeId,
    phandle: Option<u32>,
    raw: &[u32],
) -> AxVmResult<Vec<Aarch64ForwardedIrq>> {
    let cells = interrupt_cells(host_fdt, node_path, controller_id, phandle, raw)?;
    if !raw.len().is_multiple_of(cells) {
        return Err(route_error(ForwardedIrqConfigError::TruncatedSpecifier {
            node: node_path.to_string(),
            controller: host_fdt.path_of(controller_id),
            phandle,
            raw: raw.to_vec(),
        }));
    }
    raw.chunks(cells)
        .map(|specifier| {
            decode_gic_specifier(host_fdt, node_path, controller_id, phandle, specifier)
        })
        .collect()
}

fn decode_gic_specifier(
    host_fdt: &Fdt,
    node_path: &str,
    controller_id: NodeId,
    phandle: Option<u32>,
    raw: &[u32],
) -> AxVmResult<Aarch64ForwardedIrq> {
    let controller = host_fdt.node(controller_id).ok_or_else(|| {
        route_error(ForwardedIrqConfigError::UnknownController {
            node: node_path.to_string(),
            phandle: phandle.unwrap_or_default(),
            raw: raw.to_vec(),
        })
    })?;
    let compatibles = controller.compatibles().collect::<Vec<_>>();
    if !compatibles
        .iter()
        .any(|compatible| is_supported_gic(compatible))
    {
        return Err(route_error(
            ForwardedIrqConfigError::UnsupportedController {
                node: node_path.to_string(),
                controller: host_fdt.path_of(controller_id),
                phandle,
                compatible: compatibles.join(","),
                raw: raw.to_vec(),
            },
        ));
    }
    let [0, spi_offset, _flags] = raw else {
        return Err(route_error(ForwardedIrqConfigError::UnsupportedGicSource {
            node: node_path.to_string(),
            controller: host_fdt.path_of(controller_id),
            phandle,
            raw: raw.to_vec(),
        }));
    };
    let spi = Aarch64GicSpi::new(*spi_offset).ok_or_else(|| {
        route_error(ForwardedIrqConfigError::UnsupportedGicSource {
            node: node_path.to_string(),
            controller: host_fdt.path_of(controller_id),
            phandle,
            raw: raw.to_vec(),
        })
    })?;
    Ok(Aarch64ForwardedIrq::identity(spi))
}

fn resolve_interrupt_parent(
    host_fdt: &Fdt,
    node_id: NodeId,
    node_path: &str,
    raw: &[u32],
) -> AxVmResult<(NodeId, Option<u32>)> {
    let mut current = Some(node_id);
    while let Some(id) = current {
        let node = host_fdt.node(id).ok_or_else(|| {
            route_error(ForwardedIrqConfigError::MissingInterruptParent {
                node: node_path.to_string(),
                raw: raw.to_vec(),
            })
        })?;
        if id != node_id && node.is_interrupt_controller() {
            return Ok((id, node.phandle().map(|phandle| phandle.raw())));
        }
        if let Some(parent) = node
            .get_property("interrupt-parent")
            .and_then(|property| property.get_u32())
        {
            return controller_by_phandle(host_fdt, node_path, parent, raw)
                .map(|controller_id| (controller_id, Some(parent)));
        }
        current = host_fdt.parent_of(id);
    }
    Err(route_error(
        ForwardedIrqConfigError::MissingInterruptParent {
            node: node_path.to_string(),
            raw: raw.to_vec(),
        },
    ))
}

fn controller_by_phandle(
    host_fdt: &Fdt,
    node_path: &str,
    phandle: u32,
    raw: &[u32],
) -> AxVmResult<NodeId> {
    host_fdt
        .get_by_phandle_id(Phandle::from(phandle))
        .ok_or_else(|| {
            route_error(ForwardedIrqConfigError::UnknownController {
                node: node_path.to_string(),
                phandle,
                raw: raw.to_vec(),
            })
        })
}

fn interrupt_cells(
    host_fdt: &Fdt,
    node_path: &str,
    controller_id: NodeId,
    phandle: Option<u32>,
    raw: &[u32],
) -> AxVmResult<usize> {
    let controller = host_fdt.node(controller_id).ok_or_else(|| {
        route_error(ForwardedIrqConfigError::UnknownController {
            node: node_path.to_string(),
            phandle: phandle.unwrap_or_default(),
            raw: raw.to_vec(),
        })
    })?;
    controller
        .interrupt_cells()
        .filter(|&cells| cells != 0)
        .map(|cells| cells as usize)
        .ok_or_else(|| {
            route_error(ForwardedIrqConfigError::MissingInterruptCells {
                node: node_path.to_string(),
                controller: host_fdt.path_of(controller_id),
                phandle,
                raw: raw.to_vec(),
            })
        })
}

fn is_supported_gic(compatible: &str) -> bool {
    matches!(
        compatible,
        "arm,gic-v3" | "arm,gic-400" | "arm,cortex-a15-gic"
    )
}

fn route_error(source: ForwardedIrqConfigError) -> AxVmError {
    AxVmError::ForwardedIrqConfig { source }
}
