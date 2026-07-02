use axfs_ng_vfs::{VfsError, VfsResult};
use rdif_pwm::{DriverGeneric, Interface as PwmInterface, PwmError, PwmPolarity, PwmState};
use sg200x_bsp::{
    pwm::{Pwm, PwmChannel, PwmMode, PwmPolarity as SgPwmPolarity},
    soc::PWM0_BASE,
};

const PWM_SYSFS_CHIPS: u8 = 4;
const PWM_SYSFS_CHANNELS_PER_CHIP: u8 = 4;
const NANOS_PER_SECOND: u64 = 1_000_000_000;

pub(in crate::pseudofs::dev::pwm) struct PwmHardware {
    controller: Sg2002Pwm,
}

struct Sg2002Pwm {
    pwm: Pwm,
}

// `Sg2002Pwm` owns one PWM register window and is only accessed through the
// global sysfs PWM mutex.
unsafe impl Send for Sg2002Pwm {}

pub(in crate::pseudofs::dev::pwm) fn pwm_chip_count() -> u8 {
    PWM_SYSFS_CHIPS
}

pub(in crate::pseudofs::dev::pwm) fn pwm_channels_per_chip(_index: u8) -> u8 {
    PWM_SYSFS_CHANNELS_PER_CHIP
}

pub(in crate::pseudofs::dev::pwm) fn pwmchip_number(index: u8) -> u8 {
    index * PWM_SYSFS_CHANNELS_PER_CHIP
}

pub(in crate::pseudofs::dev::pwm) fn pwmchip_index(chip_number: u8) -> Option<u8> {
    if !chip_number.is_multiple_of(PWM_SYSFS_CHANNELS_PER_CHIP) {
        return None;
    }
    let index = chip_number / PWM_SYSFS_CHANNELS_PER_CHIP;
    (index < PWM_SYSFS_CHIPS).then_some(index)
}

impl PwmHardware {
    pub(in crate::pseudofs::dev::pwm) fn new(index: u8) -> Self {
        let pwm_addr = PWM0_BASE + index as usize * 0x1000 + ax_config::plat::PHYS_VIRT_OFFSET;
        Self {
            controller: Sg2002Pwm {
                pwm: unsafe { Pwm::new(pwm_addr) },
            },
        }
    }
}

pub(in crate::pseudofs::dev::pwm) fn apply_channel(
    hw: &mut PwmHardware,
    channel_index: u8,
    period_ns: u64,
    duty_ns: u64,
    running: bool,
) -> VfsResult<()> {
    hw.controller
        .apply(
            channel_index as usize,
            PwmState::normal(period_ns, duty_ns, running),
        )
        .map_err(map_pwm_error)
}

pub(in crate::pseudofs::dev::pwm) fn disable_channel(
    hw: &mut PwmHardware,
    channel_index: u8,
) -> VfsResult<()> {
    hw.controller
        .disable(channel_index as usize)
        .map_err(map_pwm_error)
}

impl DriverGeneric for Sg2002Pwm {
    fn name(&self) -> &str {
        "sg2002-pwm"
    }
}

impl PwmInterface for Sg2002Pwm {
    fn channel_count(&self) -> usize {
        PWM_SYSFS_CHANNELS_PER_CHIP as usize
    }

    fn apply(&mut self, channel: usize, state: PwmState) -> Result<(), PwmError> {
        if channel >= self.channel_count() {
            return Err(PwmError::InvalidChannel);
        }
        if state.polarity != PwmPolarity::Normal {
            return Err(PwmError::UnsupportedPolarity);
        }
        if state.period_ns == 0 {
            return Err(PwmError::InvalidPeriod);
        }
        if state.duty_ns > state.period_ns {
            return Err(PwmError::InvalidDuty);
        }
        let frequency_hz = (NANOS_PER_SECOND / state.period_ns) as u32;
        if frequency_hz == 0 {
            return Err(PwmError::InvalidPeriod);
        }
        let high_percent = (state.duty_ns * 100 / state.period_ns) as u8;
        let low_percent = 100u8.saturating_sub(high_percent);
        let channel = PwmChannel::from_u8(channel as u8).ok_or(PwmError::InvalidChannel)?;
        let result = if state.enabled {
            self.pwm
                .update_frequency_duty(channel, frequency_hz, low_percent)
        } else {
            self.pwm.configure_channel(
                channel,
                frequency_hz,
                low_percent,
                SgPwmPolarity::ActiveHigh,
            )
        };
        result.map_err(|_| PwmError::InvalidPeriod)?;
        if state.enabled {
            self.pwm.set_mode(channel, PwmMode::Continuous);
            self.pwm.enable_output(channel);
            self.pwm.start(channel);
        }
        Ok(())
    }

    fn disable(&mut self, channel: usize) -> Result<(), PwmError> {
        if channel >= self.channel_count() {
            return Err(PwmError::InvalidChannel);
        }
        let channel = PwmChannel::from_u8(channel as u8).ok_or(PwmError::InvalidChannel)?;
        self.pwm.stop(channel);
        self.pwm.disable_output(channel);
        Ok(())
    }
}

fn map_pwm_error(err: PwmError) -> VfsError {
    match err {
        PwmError::InvalidChannel
        | PwmError::InvalidPeriod
        | PwmError::InvalidDuty
        | PwmError::UnsupportedPolarity => VfsError::InvalidInput,
    }
}
