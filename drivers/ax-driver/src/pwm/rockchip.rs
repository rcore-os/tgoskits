extern crate alloc;

use alloc::format;

use log::{info, warn};
use rdif_pwm::{DriverGeneric, Interface as PwmInterface, Pwm, PwmError, PwmPolarity, PwmState};
use rdrive::{
    probe::{OnProbeError, fdt::ClockLine},
    register::{FdtInfo, ProbeFdt},
};
use rockchip_pwm::{RK_PWM_CLOCK_HZ, RK_PWM_MMIO_SIZE, RockchipPwm};

use crate::mmio::iomap;

const PWM_CHANNELS_PER_CHIP: usize = 1;

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

struct RockchipPwmDevice {
    inner: RockchipPwm,
    pwm_clock: ClockLine,
    pclk: ClockLine,
    initialized: bool,
}

// rdrive serializes access through the device lock. The MMIO mapping is stable
// for the kernel lifetime after `iomap()` succeeds.
unsafe impl Send for RockchipPwmDevice {}

impl DriverGeneric for RockchipPwmDevice {
    fn name(&self) -> &str {
        "rockchip-pwm"
    }
}

impl PwmInterface for RockchipPwmDevice {
    fn channel_count(&self) -> usize {
        PWM_CHANNELS_PER_CHIP
    }

    fn apply(&mut self, channel: usize, state: PwmState) -> Result<(), PwmError> {
        self.ensure_initialized()?;
        self.inner.apply(channel, state)
    }

    fn disable(&mut self, channel: usize) -> Result<(), PwmError> {
        self.ensure_initialized()?;
        self.inner.disable(channel)
    }
}

impl RockchipPwmDevice {
    fn ensure_initialized(&mut self) -> Result<(), PwmError> {
        if self.initialized {
            return Ok(());
        }
        if let Err(err) = configure_clocks(&self.pwm_clock, &self.pclk) {
            warn!("failed to enable Rockchip PWM clocks: {err}");
            return Err(PwmError::InvalidPeriod);
        }
        self.inner.init(PwmPolarity::Normal)?;
        self.initialized = true;
        Ok(())
    }
}

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    let (pwm_clock, pclk) = pwm_clock_lines(&info)?;

    let base = iomap(
        reg.address as usize,
        reg.size.unwrap_or(RK_PWM_MMIO_SIZE as u64) as usize,
    )?;
    let inner = unsafe { RockchipPwm::new(base, RK_PWM_CLOCK_HZ, PWM_CHANNELS_PER_CHIP) };

    plat_dev.register(Pwm::new(RockchipPwmDevice {
        inner,
        pwm_clock,
        pclk,
        initialized: false,
    }));
    info!(
        "Rockchip PWM registered: node={} base={:#x}",
        info.node.name(),
        reg.address
    );
    Ok(())
}

fn pwm_clock_lines(info: &FdtInfo<'_>) -> Result<(ClockLine, ClockLine), OnProbeError> {
    let pwm_clock = info
        .find_clock_line_by_name("pwm")?
        .ok_or_else(|| OnProbeError::other(format!("[{}] missing pwm clock", info.node.name())))?;
    let pclk = info
        .find_clock_line_by_name("pclk")?
        .ok_or_else(|| OnProbeError::other(format!("[{}] missing pclk clock", info.node.name())))?;
    Ok((pwm_clock, pclk))
}

fn configure_clocks(pwm_clock: &ClockLine, pclk: &ClockLine) -> Result<(), OnProbeError> {
    pwm_clock.set_rate(RK_PWM_CLOCK_HZ)?;
    pwm_clock.enable()?;
    pclk.enable()?;
    Ok(())
}
