use alloc::format;

use rdrive::{probe::OnProbeError, register::ProbeFdt};
use rockchip_pwm::{RK_PWM_MMIO_SIZE, RockchipPwm};

crate::model_register!(
    name: "Rockchip PWM",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-pwm", "rockchip,rk3328-pwm"],
            on_probe: probe
        }
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, device) = probe.into_parts();
    let pwm = info
        .find_clock_line_by_name("pwm")?
        .ok_or_else(|| OnProbeError::other(format!("[{}] missing pwm clock", info.node.name())))?;
    let pclk = info
        .find_clock_line_by_name("pclk")?
        .ok_or_else(|| OnProbeError::other(format!("[{}] missing pclk", info.node.name())))?;
    let rate = pwm.rate()?;
    if rate == 0 {
        return Err(OnProbeError::other("PWM clock rate is zero"));
    }
    let mmio = super::map_registers(&info, RK_PWM_MMIO_SIZE)?;
    // Clock gates are kept on for the registered device's kernel lifetime.
    // Preserve the BSP/bootloader source and waveform instead of resetting them.
    pclk.enable()?;
    pwm.enable()?;
    // SAFETY: the platform device exclusively owns this validated, permanent mapping.
    let pwm = unsafe { RockchipPwm::new(mmio, rate, axklib::time::busy_wait) }
        .map_err(|err| OnProbeError::other(format!("PWM initialization: {err}")))?;
    device.register(rdif_pwm::Pwm::new(pwm));
    Ok(())
}
