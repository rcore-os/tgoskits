mod rockchip;
mod sg200x;

use rdrive::{probe::OnProbeError, register::FdtInfo};

fn map_registers(info: &FdtInfo<'_>, minimum: usize) -> Result<mmio_api::MmioRaw, OnProbeError> {
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other("missing PWM reg"))?;
    let address =
        usize::try_from(reg.address).map_err(|_| OnProbeError::other("PWM address overflow"))?;
    let size = reg
        .size
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n >= minimum)
        .ok_or_else(|| OnProbeError::other("PWM register range too short"))?;
    if address % 4 != 0 || address.checked_add(size).is_none() {
        return Err(OnProbeError::other("invalid PWM register range"));
    }
    axklib::mmio::ioremap_raw(address.into(), size)
        .map_err(|err| OnProbeError::other(alloc::format!("PWM mapping: {err}")))
}
