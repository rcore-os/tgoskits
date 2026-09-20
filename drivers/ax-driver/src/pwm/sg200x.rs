use alloc::format;

use rdrive::{probe::OnProbeError, register::ProbeFdt};
use sg200x_pwm::{MMIO_SIZE, Sg200xPwm};

crate::model_register!(
    name: "SG200x PWM",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt { compatibles: &["cvitek,cvi-pwm"], on_probe: probe }
    ],
);
fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, device) = probe.into_parts();
    let clocks = info.clock_lines()?;
    if clocks.len() != 1 {
        return Err(OnProbeError::other("SG200x PWM requires one clock"));
    }
    let clock = &clocks[0];
    let rate = clock.rate()?;
    if rate == 0 {
        return Err(OnProbeError::other("PWM clock rate is zero"));
    }
    let node = info.node.as_node();
    if node.get_property("no-polarity").is_some()
        || node
            .get_property("pwm-num")
            .and_then(|p| p.get_u32())
            .is_some_and(|n| n != 4)
    {
        return Err(OnProbeError::other("unsupported SG200x PWM variant"));
    }
    let mmio = super::map_registers(&info, MMIO_SIZE)?;
    clock.enable()?;
    // SAFETY: this platform device owns the permanent mapping; the PWM clock
    // provider does not change the parent, divider or rate during its lifetime.
    let pwm = unsafe { Sg200xPwm::new(mmio, rate) }
        .map_err(|err| OnProbeError::other(format!("PWM initialization: {err}")))?;
    device.register(rdif_pwm::Pwm::new(pwm));
    Ok(())
}
