#![no_std]

extern crate alloc;

use rdif_base::def_driver;
pub use rdif_base::{DriverGeneric, KError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PwmPolarity {
    Normal,
    Inversed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PwmState {
    pub period_ns: u64,
    pub duty_ns: u64,
    pub enabled: bool,
    pub polarity: PwmPolarity,
}

impl PwmState {
    pub const fn normal(period_ns: u64, duty_ns: u64, enabled: bool) -> Self {
        Self {
            period_ns,
            duty_ns,
            enabled,
            polarity: PwmPolarity::Normal,
        }
    }
}

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PwmError {
    #[error("invalid PWM channel")]
    InvalidChannel,
    #[error("invalid PWM period")]
    InvalidPeriod,
    #[error("invalid PWM duty cycle")]
    InvalidDuty,
    #[error("unsupported PWM polarity")]
    UnsupportedPolarity,
    #[error("invalid PWM register mapping")]
    InvalidMapping,
    #[error("PWM clock is unavailable or invalid")]
    Clock,
}

pub trait Interface: DriverGeneric {
    fn channel_count(&self) -> usize;

    /// Read the actual, quantized hardware state, not the last requested state.
    fn get_state(&mut self, channel: usize) -> Result<PwmState, PwmError>;

    /// Apply a complete state. Disabled requests ignore waveform parameters.
    /// Validation failures must leave the hardware unchanged.
    fn apply(&mut self, channel: usize, state: PwmState) -> Result<(), PwmError>;

    fn disable(&mut self, channel: usize) -> Result<(), PwmError> {
        self.apply(channel, PwmState::normal(0, 0, false))
    }
}

def_driver!(Pwm, Interface);
