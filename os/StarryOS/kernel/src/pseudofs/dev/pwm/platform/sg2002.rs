use axfs_ng_vfs::{VfsError, VfsResult};
use sg200x_bsp::{
    pwm::{Pwm, PwmChannel, PwmMode, PwmPolarity},
    soc::PWM0_BASE,
};

const PWM_SYSFS_CHIPS: u8 = 4;
const PWM_SYSFS_CHANNELS_PER_CHIP: u8 = 4;

pub(in crate::pseudofs::dev::pwm) struct PwmHardware {
    pwm: Pwm,
}

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
            pwm: unsafe { Pwm::new(pwm_addr) },
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
    let frequency_hz = (super::NANOS_PER_SECOND / period_ns) as u32;
    if frequency_hz == 0 {
        return Err(VfsError::InvalidInput);
    }
    let high_percent = (duty_ns * 100 / period_ns) as u8;
    let low_percent = 100u8.saturating_sub(high_percent);
    let channel = PwmChannel::from_u8(channel_index).ok_or(VfsError::InvalidInput)?;
    let result = if running {
        hw.pwm
            .update_frequency_duty(channel, frequency_hz, low_percent)
    } else {
        hw.pwm
            .configure_channel(channel, frequency_hz, low_percent, PwmPolarity::ActiveHigh)
    };
    result.map_err(|_| VfsError::InvalidInput)?;
    if running {
        hw.pwm.set_mode(channel, PwmMode::Continuous);
        hw.pwm.enable_output(channel);
        hw.pwm.start(channel);
    }
    Ok(())
}

pub(in crate::pseudofs::dev::pwm) fn disable_channel(
    hw: &mut PwmHardware,
    channel_index: u8,
) -> VfsResult<()> {
    let channel = PwmChannel::from_u8(channel_index).ok_or(VfsError::InvalidInput)?;
    hw.pwm.stop(channel);
    hw.pwm.disable_output(channel);
    Ok(())
}
