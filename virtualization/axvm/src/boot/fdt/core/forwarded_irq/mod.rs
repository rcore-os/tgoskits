//! Authoritative host-FDT interrupt route discovery.

mod decode;
mod selection;

use alloc::{format, vec::Vec};

use axvm_types::VMInterruptMode;
use fdt_edit::Fdt;

use crate::{
    AxVmError, AxVmResult, ax_err_type,
    config::{Aarch64ForwardedIrq, AxVMConfig},
};

/// Populates the AArch64 Hybrid route set from the authoritative host FDT.
pub(crate) fn prepare_aarch64_hybrid_routes(
    config: &mut AxVMConfig,
    host_fdt_bytes: Option<&[u8]>,
) -> AxVmResult {
    if config.interrupt_mode() != VMInterruptMode::Hybrid {
        return Ok(());
    }
    let bytes = host_fdt_bytes.ok_or_else(|| AxVmError::Unsupported {
        operation: "discover AArch64 Hybrid IRQ routes",
        detail: "host FDT is unavailable".into(),
    })?;
    let host_fdt = Fdt::from_bytes(bytes).map_err(|error| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse host FDT for Hybrid IRQ routes: {error:#?}")
        )
    })?;
    let routes = discover_aarch64_hybrid_routes(config, &host_fdt)?;
    config.replace_aarch64_hybrid_forwarded_irqs(routes);
    Ok(())
}

/// Discovers AArch64 Hybrid routes from selected host-FDT producer nodes.
pub(crate) fn discover_aarch64_hybrid_routes(
    config: &AxVMConfig,
    host_fdt: &Fdt,
) -> AxVmResult<Vec<Aarch64ForwardedIrq>> {
    let selected_paths = selection::selected_producer_paths(config, host_fdt)?;
    let mut routes = Vec::new();
    for path in &selected_paths {
        let Some(node_id) = host_fdt.get_by_path_id(path) else {
            continue;
        };
        let Some(node) = host_fdt.node(node_id) else {
            continue;
        };
        if node.is_interrupt_controller() || is_architectural_private_interrupt_node(node) {
            continue;
        }
        routes.extend(decode::decode_gic_spi_routes(host_fdt, path)?);
    }
    Ok(routes)
}

fn is_architectural_private_interrupt_node(node: &fdt_edit::Node) -> bool {
    node.compatibles().any(|compatible| {
        matches!(
            compatible,
            "arm,armv7-timer" | "arm,armv8-timer" | "arm,armv8-pmuv3" | "arm,cortex-a15-pmu"
        )
    })
}
