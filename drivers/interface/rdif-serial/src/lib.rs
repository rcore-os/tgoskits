//! Portable UART capability boundary.
//!
//! This crate contains no software queues, task policy, IRQ registration, or
//! OS wakeups. Concrete drivers split into one task-owned data/control endpoint
//! and one IRQ-owned event endpoint; the consuming runtime owns all buffering
//! and scheduling policy.

#![no_std]

mod raw;
mod types;

pub use self::{raw::*, types::*};

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    #[error("invalid baud rate")]
    InvalidBaudrate,
    #[error("unsupported data bits")]
    UnsupportedDataBits,
    #[error("unsupported stop bits")]
    UnsupportedStopBits,
    #[error("unsupported parity")]
    UnsupportedParity,
    #[error("UART register access failed")]
    RegisterError,
    #[error("UART operation timed out")]
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DataBits {
    Five  = 5,
    Six   = 6,
    Seven = 7,
    Eight = 8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StopBits {
    One = 1,
    Two = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parity {
    None,
    Even,
    Odd,
    Mark,
    Space,
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub baudrate: Option<u32>,
    pub data_bits: Option<DataBits>,
    pub stop_bits: Option<StopBits>,
    pub parity: Option<Parity>,
}

impl Config {
    pub const fn new() -> Self {
        Self {
            baudrate: None,
            data_bits: None,
            stop_bits: None,
            parity: None,
        }
    }

    pub const fn baudrate(mut self, baudrate: u32) -> Self {
        self.baudrate = Some(baudrate);
        self
    }

    pub const fn data_bits(mut self, data_bits: DataBits) -> Self {
        self.data_bits = Some(data_bits);
        self
    }

    pub const fn stop_bits(mut self, stop_bits: StopBits) -> Self {
        self.stop_bits = Some(stop_bits);
        self
    }

    pub const fn parity(mut self, parity: Parity) -> Self {
        self.parity = Some(parity);
        self
    }
}

#[cfg(all(axtest, feature = "axtest"))]
pub mod axtest;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_event_reports_readiness_and_errors() {
        let event = SerialEventSet::RX_DATA | SerialEventSet::FAULT;

        assert!(event.has_rx());
        assert!(!event.has_tx());
        assert!(event.contains(SerialEventSet::FAULT));
    }
}
