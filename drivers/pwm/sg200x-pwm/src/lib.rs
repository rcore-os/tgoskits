//! SG200x PWM output, following the CV181x Linux 5.10 BSP.
#![no_std]

pub use mmio_api::MmioRaw;
use rdif_pwm::{DriverGeneric, Interface, PwmError, PwmPolarity, PwmState};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
    registers::ReadWrite,
};

register_structs! {
    ChannelRegisters {
        (0x00 => low_period: ReadWrite<u32>),
        (0x04 => period: ReadWrite<u32>),
        (0x08 => @END),
    },
    Registers {
        (0x00 => channels: [ChannelRegisters; 4]),
        (0x20 => _reserved0),
        (0x40 => polarity: ReadWrite<u32, Control::Register>),
        (0x44 => start: ReadWrite<u32, Channels::Register>),
        (0x48 => _reserved1),
        (0xd0 => output_enable: ReadWrite<u32, Channels::Register>),
        (0xd4 => @END),
    }
}

register_bitfields![u32,
    Control [POLARITY OFFSET(0) NUMBITS(4) []],
    Channels [ENABLED OFFSET(0) NUMBITS(4) []]
];

pub const MMIO_SIZE: usize = 0xd4;
// Conservative supported range; the vendor driver does not specify a counter
// width. Do not infer additional high-bit register semantics from mainline.
const MAX_PERIOD: u128 = (1 << 30) - 1;

/// Owns all four channels, including their shared control registers.
pub struct Sg200xPwm {
    mmio: MmioRaw,
    clock_hz: u64,
}
impl Sg200xPwm {
    /// # Safety
    /// The device mapping must remain live and exclusively owned for the
    /// lifetime of the controller. The supplied clock rate must remain stable.
    pub unsafe fn new(mmio: MmioRaw, clock_hz: u64) -> Result<Self, PwmError> {
        if mmio.size() < MMIO_SIZE || mmio.as_ptr().align_offset(4) != 0 {
            return Err(PwmError::InvalidMapping);
        }
        if clock_hz == 0 {
            return Err(PwmError::Clock);
        }
        Ok(Self { mmio, clock_hz })
    }
    fn regs(&self) -> &Registers {
        // SAFETY: new checks size and alignment; its caller owns mapping
        // lifetime/exclusivity. ReadWrite cells use volatile register access.
        unsafe { &*self.mmio.as_ptr().cast::<Registers>() }
    }
    fn nanos(&self, ticks: u32) -> Result<u64, PwmError> {
        u64::try_from((u128::from(ticks) * 1_000_000_000).div_ceil(u128::from(self.clock_hz)))
            .map_err(|_| PwmError::InvalidPeriod)
    }
}
impl DriverGeneric for Sg200xPwm {
    fn name(&self) -> &str {
        "sg200x-pwm"
    }
}
impl Interface for Sg200xPwm {
    fn channel_count(&self) -> usize {
        4
    }
    fn get_state(&mut self, channel: usize) -> Result<PwmState, PwmError> {
        let regs = self.regs();
        let ch = regs.channels.get(channel).ok_or(PwmError::InvalidChannel)?;
        let period = ch.period.get();
        let mask = 1 << channel;
        let enabled =
            regs.start.read(Channels::ENABLED) & regs.output_enable.read(Channels::ENABLED) & mask
                != 0;
        // BSP disable writes PERIOD=1/HLPERIOD=2, which describes no waveform.
        let duty = if enabled {
            period
                .checked_sub(ch.low_period.get())
                .ok_or(PwmError::InvalidDuty)?
        } else {
            0
        };
        Ok(PwmState {
            period_ns: self.nanos(period)?,
            duty_ns: self.nanos(duty)?,
            enabled,
            polarity: if regs.polarity.read(Control::POLARITY) & mask == 0 {
                PwmPolarity::Normal
            } else {
                PwmPolarity::Inversed
            },
        })
    }
    /// BSP quantization clamps the active duration to 1..period_ticks-1.
    /// Updating a running channel restarts it; glitch-free updates are not promised.
    fn apply(&mut self, channel: usize, state: PwmState) -> Result<(), PwmError> {
        if channel >= 4 {
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
        let period = u128::from(self.clock_hz) * u128::from(state.period_ns) / 1_000_000_000;
        if !(2..=MAX_PERIOD).contains(&period) {
            return Err(PwmError::InvalidPeriod);
        }
        // BSP first rounds the period, then computes duty from that rounded period.
        let duty =
            (period * u128::from(state.duty_ns) / u128::from(state.period_ns)).clamp(1, period - 1);
        let regs = self.regs();
        let mask = 1 << channel;
        let polarity = regs.polarity.read(Control::POLARITY) & !mask;
        regs.polarity.modify(Control::POLARITY.val(
            polarity
                | if state.polarity == PwmPolarity::Inversed {
                    mask
                } else {
                    0
                },
        ));
        regs.channels[channel].period.set(period as u32);
        regs.channels[channel]
            .low_period
            .set((period - duty) as u32);
        let start = regs.start.read(Channels::ENABLED);
        regs.start.modify(Channels::ENABLED.val(start & !mask));
        // Unlike the BSP's whole-register assignment, preserve other channels' OE.
        regs.output_enable
            .modify(Channels::ENABLED.val(regs.output_enable.read(Channels::ENABLED) | mask));
        regs.start.modify(Channels::ENABLED.val(start | mask));
        Ok(())
    }
    fn disable(&mut self, channel: usize) -> Result<(), PwmError> {
        if channel >= 4 {
            return Err(PwmError::InvalidChannel);
        }
        let regs = self.regs();
        let mask = 1 << channel;
        regs.output_enable
            .modify(Channels::ENABLED.val(regs.output_enable.read(Channels::ENABLED) & !mask));
        regs.start
            .modify(Channels::ENABLED.val(regs.start.read(Channels::ENABLED) & !mask));
        // BSP stop sentinel, after disconnecting and stopping this channel.
        regs.channels[channel].period.set(1);
        regs.channels[channel].low_period.set(2);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_quantization_isolated_channels_and_rejection() {
        let mut regs = [0u32; MMIO_SIZE / 4];
        // SAFETY: exclusively owned aligned register storage outlives pwm.
        let mut pwm = unsafe {
            Sg200xPwm::new(
                MmioRaw::new(
                    0usize.into(),
                    core::ptr::NonNull::new(regs.as_mut_ptr().cast()).unwrap(),
                    MMIO_SIZE,
                ),
                100_000_000,
            )
            .unwrap()
        };
        pwm.apply(2, PwmState::normal(105, 35, true)).unwrap();
        assert_eq!(regs[5], 10);
        assert_eq!(regs[4], 7);
        assert_eq!(pwm.get_state(2).unwrap().duty_ns, 30);
        let before = regs;
        assert_eq!(
            pwm.apply(2, PwmState::normal(10, 5, true)),
            Err(PwmError::InvalidPeriod)
        );
        assert_eq!(regs, before);
        assert_eq!(
            pwm.apply(2, PwmState::normal(u64::MAX, 0, true)),
            Err(PwmError::InvalidPeriod)
        );
        assert_eq!(
            pwm.apply(2, PwmState::normal(100, 101, true)),
            Err(PwmError::InvalidDuty)
        );
        assert_eq!(pwm.disable(4), Err(PwmError::InvalidChannel));
        assert_eq!(regs, before);
        pwm.apply(
            1,
            PwmState {
                polarity: PwmPolarity::Inversed,
                ..PwmState::normal(100, 100, true)
            },
        )
        .unwrap();
        assert_eq!(pwm.get_state(1).unwrap().duty_ns, 90);
        assert_eq!(pwm.get_state(1).unwrap().polarity, PwmPolarity::Inversed);
        pwm.apply(1, PwmState::normal(100, 0, true)).unwrap();
        assert_eq!(pwm.get_state(1).unwrap().duty_ns, 10);
        pwm.apply(1, PwmState::normal(0, u64::MAX, false)).unwrap();
        assert!(!pwm.get_state(1).unwrap().enabled);
        assert!(pwm.get_state(2).unwrap().enabled);
        assert_eq!(regs[4..6], before[4..6]);
    }
}
