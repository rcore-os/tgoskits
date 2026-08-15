//! Resolved firmware metadata for Axvisor IVC channel nodes.

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use std::vec::Vec;

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use axdevice::{DeviceFirmwareSpec, ResolvedDeviceGraph, ResolvedDeviceResources};

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use crate::{AxVmError, AxVmResult};

/// One resolved IVC channel described to guest firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestIvcChannel {
    pub base_gpa: usize,
    pub length: usize,
    pub notify_irq: u32,
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn resolved_ivc_channels(
    graph: &ResolvedDeviceGraph,
) -> AxVmResult<Vec<GuestIvcChannel>> {
    graph
        .nodes()
        .filter_map(|node| {
            let firmware = node.firmware();
            is_ivc_channel(&firmware).then_some((node.id(), firmware))
        })
        .map(|(device_id, firmware)| {
            let resources = graph.resources_for(device_id)?;
            ivc_channel_from_resources(device_id.as_str(), &firmware, resources)
        })
        .collect()
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn is_ivc_channel(firmware: &DeviceFirmwareSpec) -> bool {
    firmware
        .compatible()
        .iter()
        .any(|compatible| compatible == "axvisor,ivc-channel")
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn ivc_channel_from_resources(
    device_id: &str,
    firmware: &DeviceFirmwareSpec,
    resources: &ResolvedDeviceResources,
) -> AxVmResult<GuestIvcChannel> {
    let [registers] = firmware.register_slots() else {
        return Err(AxVmError::invalid_config(std::format!(
            "IVC firmware model for {device_id} must declare exactly one register slot"
        )));
    };
    let (base_gpa, length) = resources.mmio(registers)?;
    let [notify] = firmware.interrupt_slots() else {
        return Err(AxVmError::invalid_config(std::format!(
            "IVC firmware model for {device_id} must declare exactly one interrupt slot"
        )));
    };
    let notify_irq = u32::try_from(resources.wired_irq(notify)?.input().value()).map_err(|_| {
        AxVmError::invalid_config(std::format!(
            "resolved IVC notify IRQ for {device_id} exceeds the FDT cell width"
        ))
    })?;
    Ok(GuestIvcChannel {
        base_gpa: usize::try_from(base_gpa).map_err(|_| {
            AxVmError::invalid_config(std::format!(
                "resolved IVC base for {device_id} exceeds the target address width"
            ))
        })?,
        length: usize::try_from(length).map_err(|_| {
            AxVmError::invalid_config(std::format!(
                "resolved IVC length for {device_id} exceeds the target address width"
            ))
        })?,
        notify_irq,
    })
}
