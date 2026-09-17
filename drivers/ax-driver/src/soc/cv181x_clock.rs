use alloc::format;

use cv181x_clock::{Cv181xClock, MMIO_SIZE};
use rdrive::{probe::OnProbeError, register::ProbeFdt};

crate::model_register!(
    name: "CV181x PWM clock",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[
        ProbeKind::Fdt { compatibles: &["cvitek,cv181x-clk"], on_probe: probe }
    ],
);
fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, device) = probe.into_parts();
    let clocks = info.clocks()?;
    let oscillator = clocks
        .first()
        .filter(|_| clocks.len() == 1)
        .and_then(|clock| info.get_by_phandle(clock.phandle))
        .ok_or_else(|| OnProbeError::other("CV181x oscillator missing"))?;
    if !oscillator
        .as_node()
        .compatibles()
        .any(|c| c == "fixed-clock")
    {
        return Err(OnProbeError::other("CV181x requires fixed oscillator"));
    }
    let hz = oscillator
        .as_node()
        .get_property("clock-frequency")
        .and_then(|p| p.get_u32())
        .filter(|hz| *hz != 0)
        .ok_or_else(|| OnProbeError::other("invalid CV181x oscillator"))?;
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other("CV181x clock reg missing"))?;
    let address =
        usize::try_from(reg.address).map_err(|_| OnProbeError::other("clock address overflow"))?;
    let size = reg
        .size
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n >= MMIO_SIZE)
        .ok_or_else(|| OnProbeError::other("CV181x clock reg too short"))?;
    if address % 4 != 0 || address.checked_add(size).is_none() {
        return Err(OnProbeError::other("invalid clock register range"));
    }
    let mmio = axklib::mmio::ioremap_raw(address.into(), size)
        .map_err(|err| OnProbeError::other(format!("CV181x clock mapping: {err}")))?;
    // SAFETY: rdrive owns the unique provider mapping and serializes accesses.
    let clock = unsafe { Cv181xClock::new(mmio, u64::from(hz)) }
        .map_err(|err| OnProbeError::other(format!("CV181x clock: {err}")))?;
    device.register(rdif_clk::Clk::new(clock));
    Ok(())
}
