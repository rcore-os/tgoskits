//! VM address-space reservations and fixed internal ABI allowlists.

use std::vec::Vec;

use axdevice::*;
use axdevice_base::*;

use crate::{AxVmError, AxVmResult};

pub(super) fn reserve_guest_address_ranges(
    config: &crate::config::AxVMConfig,
    replacement_ranges: &[core::ops::Range<u64>],
    pools: &mut ResourcePools,
) -> AxVmResult {
    let memory_ranges = normalized_ranges(
        config
            .memory_regions()
            .iter()
            .map(|region| checked_u64_range(region.gpa, region.size, "guest memory"))
            .collect::<AxVmResult<Vec<_>>>()?,
    );
    for (index, range) in memory_ranges.iter().cloned().enumerate() {
        pools.reserve_mmio(std::format!("guest-memory-{index}"), range)?;
    }

    let reserved_ranges = config
        .reserved_address_ranges()
        .iter()
        .map(|region| checked_u64_range(region.base_gpa, region.length, "guest reserved address"))
        .collect::<AxVmResult<Vec<_>>>()?;
    let mut occupied_ranges = memory_ranges;
    occupied_ranges.extend_from_slice(replacement_ranges);
    let reserved_ranges = subtract_ranges(
        normalized_ranges(reserved_ranges),
        &normalized_ranges(occupied_ranges),
    );
    for (index, range) in normalized_ranges(reserved_ranges).into_iter().enumerate() {
        pools.reserve_mmio(std::format!("guest-reserved-address-{index}"), range)?;
    }
    Ok(())
}

fn normalized_ranges(mut ranges: Vec<core::ops::Range<u64>>) -> Vec<core::ops::Range<u64>> {
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
    merged
}

fn subtract_ranges(
    mut ranges: Vec<core::ops::Range<u64>>,
    removed_ranges: &[core::ops::Range<u64>],
) -> Vec<core::ops::Range<u64>> {
    for removed in removed_ranges {
        ranges = ranges
            .into_iter()
            .flat_map(|range| range_without(range, removed))
            .collect();
    }
    ranges
}

pub(super) fn range_without(
    range: core::ops::Range<u64>,
    removed: &core::ops::Range<u64>,
) -> Vec<core::ops::Range<u64>> {
    if range.start >= removed.end || removed.start >= range.end {
        return std::vec![range];
    }

    let mut fragments = Vec::with_capacity(2);
    if range.start < removed.start {
        fragments.push(range.start..range.end.min(removed.start));
    }
    if removed.end < range.end {
        fragments.push(range.start.max(removed.end)..range.end);
    }
    fragments
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
