//! RK3328-compatible PWM v3, following Orange Pi's RK35xx 6.1 BSP.
#![no_std]

pub use mmio_api::MmioRaw;
use rdif_pwm::{DriverGeneric, Interface, PwmError, PwmPolarity, PwmState};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
    registers::ReadWrite,
};

register_structs! {
    Registers {
        (0x00 => counter: ReadWrite<u32>),
        (0x04 => period: ReadWrite<u32>),
        (0x08 => duty: ReadWrite<u32>),
        (0x0c => control: ReadWrite<u32, Control::Register>),
        (0x10 => @END),
    }
}
register_bitfields![u32,
    Control [
        ENABLE OFFSET(0) NUMBITS(1) [],
        MODE OFFSET(1) NUMBITS(2) [Continuous = 1],
        DUTY_POSITIVE OFFSET(3) NUMBITS(1) [],
        INACTIVE_POSITIVE OFFSET(4) NUMBITS(1) [],
        CENTER OFFSET(5) NUMBITS(1) [],
        LOCK OFFSET(6) NUMBITS(1) [],
        LOW_POWER OFFSET(8) NUMBITS(1) [],
        SCALED_CLOCK OFFSET(9) NUMBITS(1) [],
        PRESCALER OFFSET(12) NUMBITS(2) [],
        SCALE OFFSET(16) NUMBITS(8) [],
        ONESHOT_COUNT OFFSET(24) NUMBITS(8) []
    ]
];
pub const RK_PWM_MMIO_SIZE: usize = 0x10;

/// Exclusive owner of a PWM v3 register window; synchronization belongs to the caller.
pub struct RockchipPwm {
    mmio: MmioRaw,
    clock_hz: u64,
    delay: fn(core::time::Duration),
}
impl RockchipPwm {
    /// `delay` must busy-wait for at least the supplied duration.
    ///
    /// # Safety
    /// The mapping must remain live, device-mapped and exclusively owned for this
    /// object's lifetime. No cloned mapping may access the same registers.
    pub unsafe fn new(
        mmio: MmioRaw,
        clock_hz: u64,
        delay: fn(core::time::Duration),
    ) -> Result<Self, PwmError> {
        if mmio.size() < RK_PWM_MMIO_SIZE || mmio.as_ptr().align_offset(4) != 0 {
            return Err(PwmError::InvalidMapping);
        }
        if clock_hz == 0 {
            return Err(PwmError::Clock);
        }
        Ok(Self {
            mmio,
            clock_hz,
            delay,
        })
    }
    fn regs(&self) -> &Registers {
        // SAFETY: construction checks layout/alignment; the mapping contract
        // covers lifetime and exclusivity. Register cells perform volatile IO.
        unsafe { &*self.mmio.as_ptr().cast::<Registers>() }
    }
    fn ticks(&self, ns: u64) -> Result<u32, PwmError> {
        u32::try_from((u128::from(ns) * u128::from(self.clock_hz) + 500_000_000) / 1_000_000_000)
            .map_err(|_| PwmError::InvalidPeriod)
    }
    fn nanos(&self, ticks: u32) -> Result<u64, PwmError> {
        u64::try_from(
            (u128::from(ticks) * 1_000_000_000 + u128::from(self.clock_hz) / 2)
                / u128::from(self.clock_hz),
        )
        .map_err(|_| PwmError::InvalidPeriod)
    }
}
impl DriverGeneric for RockchipPwm {
    fn name(&self) -> &str {
        "rockchip-pwm"
    }
}
impl Interface for RockchipPwm {
    fn channel_count(&self) -> usize {
        1
    }
    fn get_state(&mut self, channel: usize) -> Result<PwmState, PwmError> {
        if channel != 0 {
            return Err(PwmError::InvalidChannel);
        }
        let regs = self.regs();
        let ctrl = regs.control.extract();
        Ok(PwmState {
            period_ns: self.nanos(regs.period.get())?,
            duty_ns: self.nanos(regs.duty.get())?,
            enabled: ctrl.is_set(Control::ENABLE) && ctrl.matches_all(Control::MODE::Continuous),
            polarity: if ctrl.is_set(Control::DUTY_POSITIVE) {
                PwmPolarity::Normal
            } else {
                PwmPolarity::Inversed
            },
        })
    }
    fn apply(&mut self, channel: usize, state: PwmState) -> Result<(), PwmError> {
        if channel != 0 {
            return Err(PwmError::InvalidChannel);
        }
        if !state.enabled {
            return self.disable(channel);
        }
        if state.period_ns == 0 {
            return Err(PwmError::InvalidPeriod);
        }
        if state.duty_ns > state.period_ns {
            return Err(PwmError::InvalidDuty);
        }
        let period = self.ticks(state.period_ns)?;
        if period == 0 {
            return Err(PwmError::InvalidPeriod);
        }
        let duty = self
            .ticks(state.duty_ns)
            .map_err(|_| PwmError::InvalidDuty)?;
        let polarity = match state.polarity {
            PwmPolarity::Normal => Control::DUTY_POSITIVE::SET + Control::INACTIVE_POSITIVE::CLEAR,
            PwmPolarity::Inversed => {
                Control::DUTY_POSITIVE::CLEAR + Control::INACTIVE_POSITIVE::SET
            }
        };
        let regs = self.regs();
        // BSP v3 locks period/duty until polarity and LOCK are committed together.
        regs.control.modify(Control::LOCK::SET);
        regs.period.set(period);
        regs.duty.set(duty);
        // Continuous, left-aligned output on the unscaled peripheral clock.
        // The BSP requires ten input-clock cycles before releasing LOCK.
        (self.delay)(core::time::Duration::from_nanos(
            10_000_000_000u64.div_ceil(self.clock_hz),
        ));
        regs.control.modify(
            polarity
                + Control::LOCK::CLEAR
                + Control::MODE::Continuous
                + Control::CENTER::CLEAR
                + Control::LOW_POWER::CLEAR
                + Control::SCALED_CLOCK::CLEAR
                + Control::PRESCALER.val(0)
                + Control::SCALE.val(0)
                + Control::ONESHOT_COUNT.val(0),
        );
        regs.control.modify(Control::ENABLE::SET);
        Ok(())
    }
    fn disable(&mut self, channel: usize) -> Result<(), PwmError> {
        if channel != 0 {
            return Err(PwmError::InvalidChannel);
        }
        let regs = self.regs();
        regs.control
            .modify(Control::ENABLE::CLEAR + Control::MODE.val(0));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    static DELAY_NS: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn apply_uses_bsp_polarity_and_rejects_without_writes() {
        let mut regs = [0u32; 4];
        // SAFETY: aligned, exclusive register storage outlives the driver.
        let mut pwm = unsafe {
            RockchipPwm::new(
                MmioRaw::new(
                    0usize.into(),
                    core::ptr::NonNull::new(regs.as_mut_ptr().cast()).unwrap(),
                    16,
                ),
                24_000_000,
                |duration| {
                    DELAY_NS.store(duration.as_nanos() as u64, Ordering::Relaxed);
                },
            )
            .unwrap()
        };
        pwm.apply(0, PwmState::normal(1_000_000, 250_000, true))
            .unwrap();
        assert_eq!(regs[1], 24_000);
        assert_eq!(regs[2], 6_000);
        assert_eq!(regs[3] & 0x18, 0x08);
        assert_eq!(DELAY_NS.load(Ordering::Relaxed), 417);
        assert_eq!(
            pwm.get_state(0).unwrap(),
            PwmState::normal(1_000_000, 250_000, true)
        );
        pwm.apply(
            0,
            PwmState {
                polarity: PwmPolarity::Inversed,
                ..PwmState::normal(1_000_021, 250_021, true)
            },
        )
        .unwrap();
        assert_eq!(regs[1], 24_001);
        assert_eq!(regs[2], 6_001);
        assert_eq!(pwm.get_state(0).unwrap().polarity, PwmPolarity::Inversed);
        let before = regs;
        assert_eq!(
            pwm.apply(0, PwmState::normal(1_000, 2_000, true)),
            Err(PwmError::InvalidDuty)
        );
        assert_eq!(regs, before);
        pwm.disable(0).unwrap();
        assert_eq!(regs[3] & 1, 0);
    }
}
