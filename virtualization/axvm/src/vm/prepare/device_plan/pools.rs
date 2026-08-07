//! VM address-space reservations and fixed internal ABI allowlists.

use std::vec::Vec;

use axdevice::*;
use axdevice_base::*;

use crate::{AxVmError, AxVmResult};

pub(super) fn reserve_guest_memory(
    config: &crate::config::AxVMConfig,
    pools: &mut ResourcePools,
    fixed_internal_ranges: &[core::ops::Range<u64>],
) -> AxVmResult {
    let mut ranges = config
        .memory_regions()
        .iter()
        .map(|region| checked_u64_range(region.gpa, region.size, "guest memory"))
        .collect::<AxVmResult<Vec<_>>>()?;
    for device_range in fixed_internal_ranges {
        ranges = subtract_ranges(ranges, device_range);
    }
    ranges.sort_by_key(|range| range.start);

    let mut merged: Vec<core::ops::Range<u64>> = Vec::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
            continue;
        }
        merged.push(range);
    }
    for (index, range) in merged.into_iter().enumerate() {
        pools.reserve_mmio(std::format!("guest-memory-{index}"), range)?;
    }
    Ok(())
}

pub(super) fn fixed_mmio_ranges(
    requests: &[DevicePlanRequest],
) -> AxVmResult<Vec<core::ops::Range<u64>>> {
    let mut ranges = Vec::new();
    for request in requests {
        for requirement in request.requirements().entries() {
            if let DeviceRequirement::Mmio {
                size,
                request: ResourceRequest::Fixed(base),
                ..
            } = requirement
            {
                ranges.push(fixed_u64_range(*base, *size, request.id(), "MMIO")?);
            }
        }
    }
    Ok(ranges)
}

fn subtract_ranges(
    ranges: Vec<core::ops::Range<u64>>,
    removed: &core::ops::Range<u64>,
) -> Vec<core::ops::Range<u64>> {
    ranges
        .into_iter()
        .flat_map(|range| subtract_range(range, removed))
        .collect()
}

fn subtract_range(
    range: core::ops::Range<u64>,
    removed: &core::ops::Range<u64>,
) -> Vec<core::ops::Range<u64>> {
    if range.start >= removed.end || removed.start >= range.end {
        return std::vec![range];
    }

    let mut remaining = Vec::new();
    if range.start < removed.start {
        remaining.push(range.start..removed.start.min(range.end));
    }
    if removed.end < range.end {
        remaining.push(removed.end.max(range.start)..range.end);
    }
    remaining
}

pub(super) fn allow_fixed_requirements(
    requests: &[DevicePlanRequest],
    pools: &mut ResourcePools,
) -> AxVmResult {
    for request in requests {
        for requirement in request.requirements().entries() {
            match requirement {
                DeviceRequirement::Mmio {
                    size,
                    request: ResourceRequest::Fixed(base),
                    ..
                } => {
                    pools.allow_fixed_mmio(fixed_u64_range(*base, *size, request.id(), "MMIO")?)?
                }
                DeviceRequirement::Pio {
                    size,
                    request: ResourceRequest::Fixed(base),
                    ..
                } => pools.allow_fixed_pio(fixed_u16_range(*base, *size, request.id(), "PIO")?)?,
                DeviceRequirement::WiredIrq {
                    controller,
                    request: ResourceRequest::Fixed(input),
                    ..
                } => {
                    let end = input.value().checked_add(1).ok_or_else(|| {
                        AxVmError::invalid_config(std::format!(
                            "device {} fixed IRQ input overflows",
                            request.id()
                        ))
                    })?;
                    pools.allow_fixed_controller_inputs(
                        *controller,
                        *input..ControllerInputId::new(end),
                    )?;
                }
                DeviceRequirement::HostIrq {
                    request: ResourceRequest::Fixed(irq),
                    ..
                } => {
                    let end = irq.value().checked_add(1).ok_or_else(|| {
                        AxVmError::invalid_config(std::format!(
                            "device {} fixed host IRQ overflows",
                            request.id()
                        ))
                    })?;
                    pools.allow_fixed_host_irqs(*irq..HostIrqId::new(end))?;
                }
                DeviceRequirement::Msi { request: msi, .. } => {
                    allow_fixed_msi(request.id(), *msi, pools)?
                }
                DeviceRequirement::Mmio { .. }
                | DeviceRequirement::Pio { .. }
                | DeviceRequirement::WiredIrq { .. }
                | DeviceRequirement::HostIrq { .. } => {}
            }
        }
    }
    Ok(())
}

fn allow_fixed_msi(
    device_id: &str,
    request: MsiResourceRequest,
    pools: &mut ResourcePools,
) -> AxVmResult {
    let (
        ResourceRequest::Fixed(device),
        ResourceRequest::Fixed(event),
        ResourceRequest::Fixed(lpi),
    ) = (request.device(), request.event(), request.lpi())
    else {
        return Ok(());
    };
    let device_end = device.value().checked_add(1).ok_or_else(|| {
        AxVmError::invalid_config(std::format!("device {device_id} MSI DeviceID overflows"))
    })?;
    let event_end = event.value().checked_add(request.count()).ok_or_else(|| {
        AxVmError::invalid_config(std::format!(
            "device {device_id} MSI EventID range overflows"
        ))
    })?;
    let lpi_end = lpi.value().checked_add(request.count()).ok_or_else(|| {
        AxVmError::invalid_config(std::format!("device {device_id} LPI range overflows"))
    })?;
    pools.allow_fixed_msi_domain(
        request.controller(),
        request.its(),
        device..MsiDeviceId::new(device_end),
        event..MsiEventId::new(event_end),
        lpi..LpiId::new(lpi_end),
    )?;
    Ok(())
}

fn fixed_u64_range(
    base: u64,
    size: u64,
    device_id: &str,
    kind: &'static str,
) -> AxVmResult<core::ops::Range<u64>> {
    let end = base.checked_add(size).ok_or_else(|| {
        AxVmError::invalid_config(std::format!(
            "device {device_id} fixed {kind} range overflows"
        ))
    })?;
    Ok(base..end)
}

fn fixed_u16_range(
    base: u16,
    size: u16,
    device_id: &str,
    kind: &'static str,
) -> AxVmResult<core::ops::Range<u16>> {
    let end = base.checked_add(size).ok_or_else(|| {
        AxVmError::invalid_config(std::format!(
            "device {device_id} fixed {kind} range overflows"
        ))
    })?;
    Ok(base..end)
}

pub(super) fn checked_u64_range(
    base: usize,
    size: usize,
    kind: &'static str,
) -> AxVmResult<core::ops::Range<u64>> {
    let base = u64::try_from(base)
        .map_err(|_| AxVmError::invalid_config(std::format!("{kind} base does not fit u64")))?;
    let size = u64::try_from(size)
        .map_err(|_| AxVmError::invalid_config(std::format!("{kind} size does not fit u64")))?;
    if size == 0 {
        return Err(AxVmError::invalid_config(std::format!(
            "{kind} range is empty"
        )));
    }
    let end = base.checked_add(size).ok_or_else(|| {
        AxVmError::invalid_config(std::format!("{kind} range overflows the address space"))
    })?;
    Ok(base..end)
}
