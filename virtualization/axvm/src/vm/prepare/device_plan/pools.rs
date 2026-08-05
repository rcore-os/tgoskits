//! Fixed allowlists derived from model-declared internal ABI resources.

use axdevice::{
    DevicePlanRequest, DeviceRequirement, MsiResourceRequest, ResourcePools, ResourceRequest,
};
use axdevice_base::{ControllerInputId, LpiId, MsiDeviceId, MsiEventId};

use crate::{AxVmError, AxVmResult};

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
                        AxVmError::invalid_config(alloc::format!(
                            "device {} fixed IRQ input overflows",
                            request.id()
                        ))
                    })?;
                    pools.allow_fixed_controller_inputs(
                        *controller,
                        *input..ControllerInputId::new(end),
                    )?;
                }
                DeviceRequirement::Msi { request: msi, .. } => {
                    allow_fixed_msi(request.id(), *msi, pools)?
                }
                DeviceRequirement::Mmio { .. }
                | DeviceRequirement::Pio { .. }
                | DeviceRequirement::WiredIrq { .. } => {}
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
        AxVmError::invalid_config(alloc::format!("device {device_id} MSI DeviceID overflows"))
    })?;
    let event_end = event.value().checked_add(request.count()).ok_or_else(|| {
        AxVmError::invalid_config(alloc::format!(
            "device {device_id} MSI EventID range overflows"
        ))
    })?;
    let lpi_end = lpi.value().checked_add(request.count()).ok_or_else(|| {
        AxVmError::invalid_config(alloc::format!("device {device_id} LPI range overflows"))
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
        AxVmError::invalid_config(alloc::format!(
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
        AxVmError::invalid_config(alloc::format!(
            "device {device_id} fixed {kind} range overflows"
        ))
    })?;
    Ok(base..end)
}
