#![no_std]

use core::ptr::NonNull;

use rdif_base::DriverGeneric;
use rdif_pwm::{Interface, PwmError, PwmPolarity, PwmState};

const PWM_CNTR: usize = 0x00;
const PWM_PERIOD: usize = 0x04;
const PWM_DUTY: usize = 0x08;
const PWM_CTRL: usize = 0x0c;

const PWM_ENABLE: u32 = 1 << 0;
const PWM_CONTINUOUS: u32 = 1 << 1;
const PWM_MODE_MASK: u32 = 0x3 << 1;
const PWM_DUTY_NEGATIVE: u32 = 0;
const PWM_DUTY_POSITIVE: u32 = 1 << 3;
const PWM_INACTIVE_POSITIVE: u32 = 1 << 4;
const PWM_POLARITY_MASK: u32 = PWM_DUTY_POSITIVE | PWM_INACTIVE_POSITIVE;
const PWM_OUTPUT_LEFT: u32 = 0 << 5;
const PWM_LOCK_EN: u32 = 1 << 6;
const PWM_LP_DISABLE: u32 = 0 << 8;
const PWM_ENABLE_CONF: u32 = PWM_OUTPUT_LEFT | PWM_LP_DISABLE | PWM_CONTINUOUS | PWM_ENABLE;
const PWM_ENABLE_CONF_MASK: u32 = PWM_ENABLE | PWM_MODE_MASK | (1 << 5) | (1 << 8);

pub const RK_PWM_MMIO_SIZE: usize = 0x10;
pub const RK_PWM_CLOCK_HZ: u64 = 24_000_000;

pub struct RockchipPwm {
    base: NonNull<u8>,
    clock_hz: u64,
    channels: usize,
}

// `RockchipPwm` owns one MMIO register window. Callers must provide external
// synchronization when sharing it across contexts.
unsafe impl Send for RockchipPwm {}

impl RockchipPwm {
    /// # Safety
    ///
    /// `base` must point to a valid mapped Rockchip PWM register window for
    /// the whole lifetime of the returned driver. The caller owns serialization
    /// of accesses to that register window.
    pub unsafe fn new(base: NonNull<u8>, clock_hz: u64, channels: usize) -> Self {
        Self {
            base,
            clock_hz,
            channels,
        }
    }

    pub fn init(&mut self, polarity: PwmPolarity) -> Result<(), PwmError> {
        self.write_reg(PWM_CTRL, polarity_conf(polarity)?);
        self.write_reg(PWM_CNTR, 0);
        Ok(())
    }

    fn ns_to_cycles(&self, ns: u64) -> Result<u32, PwmError> {
        let cycles = (u128::from(ns) * u128::from(self.clock_hz)) / 1_000_000_000u128;
        if cycles > u128::from(u32::MAX) {
            return Err(PwmError::InvalidPeriod);
        }
        Ok(cycles as u32)
    }

    fn write_reg(&self, offset: usize, value: u32) {
        unsafe { core::ptr::write_volatile(self.base.as_ptr().add(offset) as *mut u32, value) };
    }

    fn read_reg(&self, offset: usize) -> u32 {
        unsafe { core::ptr::read_volatile(self.base.as_ptr().add(offset) as *const u32) }
    }
}

impl DriverGeneric for RockchipPwm {
    fn name(&self) -> &str {
        "rockchip-pwm"
    }
}

impl Interface for RockchipPwm {
    fn channel_count(&self) -> usize {
        self.channels
    }

    fn apply(&mut self, channel: usize, state: PwmState) -> Result<(), PwmError> {
        if channel >= self.channels {
            return Err(PwmError::InvalidChannel);
        }
        if state.period_ns == 0 {
            return Err(PwmError::InvalidPeriod);
        }
        if state.duty_ns > state.period_ns {
            return Err(PwmError::InvalidDuty);
        }

        let period_cycles = self.ns_to_cycles(state.period_ns)?;
        if period_cycles == 0 {
            return Err(PwmError::InvalidPeriod);
        }
        let duty_cycles = self
            .ns_to_cycles(state.duty_ns)
            .map_err(|_| PwmError::InvalidDuty)?;
        let polarity = polarity_conf(state.polarity)?;

        let mut ctrl = self.read_reg(PWM_CTRL);
        // Rockchip PWM latches period/duty updates while LOCK_EN is set, then
        // applies them when the bit is cleared. Keep this sequence aligned with
        // the Linux driver.
        self.write_reg(PWM_CTRL, ctrl | PWM_LOCK_EN);
        self.write_reg(PWM_PERIOD, period_cycles);
        self.write_reg(PWM_DUTY, duty_cycles);
        ctrl &= !(PWM_LOCK_EN | PWM_POLARITY_MASK);
        ctrl |= polarity;
        self.write_reg(PWM_CTRL, ctrl);
        self.write_reg(PWM_CTRL, enable_ctrl(ctrl, state.enabled));
        Ok(())
    }

    fn disable(&mut self, channel: usize) -> Result<(), PwmError> {
        if channel >= self.channels {
            return Err(PwmError::InvalidChannel);
        }
        let ctrl = self.read_reg(PWM_CTRL) & !PWM_LOCK_EN;
        self.write_reg(PWM_CTRL, enable_ctrl(ctrl, false));
        Ok(())
    }
}

fn polarity_conf(polarity: PwmPolarity) -> Result<u32, PwmError> {
    match polarity {
        // Match the RK3588 Linux state observed through debugfs on Orange Pi 5
        // Plus: disabled ctrl=0x10 and enabled ctrl=0x13 for normal sysfs PWM
        // output.
        PwmPolarity::Normal => Ok(PWM_DUTY_NEGATIVE | PWM_INACTIVE_POSITIVE),
        PwmPolarity::Inversed => Err(PwmError::UnsupportedPolarity),
    }
}

fn enable_ctrl(ctrl: u32, enabled: bool) -> u32 {
    let ctrl = ctrl & !PWM_ENABLE_CONF_MASK;
    if enabled {
        ctrl | PWM_ENABLE_CONF
    } else {
        ctrl
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_polarity_matches_linux_observed_ctrl_bits() {
        assert_eq!(polarity_conf(PwmPolarity::Normal), Ok(0x10));
        assert_eq!(enable_ctrl(0x10, false), 0x10);
        assert_eq!(enable_ctrl(0x10, true), 0x13);
    }
}
