//! Portable interrupt-driven serial runtime primitives.
//!
//! The reusable stack is intentionally split by synchronization ownership:
//! raw UART drivers expose only register-level operations, `TxQueue` and
//! `RxQueue` own independent lock-free software queues, and `SerialIrqHandler`
//! is the only endpoint allowed to touch runtime UART registers.
//!
//! OS glue must route the hardware IRQ and software TX kick to the handler's
//! owner CPU and pass an `OwnerLease`. Task or worker context drains `RxItem`s
//! and enqueues TX bytes, but never polls the shared UART IRQ/status register
//! to rediscover readiness. This keeps the fast path bounded and leaves wakeups,
//! wait queues, poll sets, and line discipline processing to OS-specific layers
//! above this crate.

#![no_std]

extern crate alloc;

use core::fmt::Display;

use bitflags::bitflags;
pub use rdif_base::{DriverGeneric, KError};

mod queue;
mod raw;
#[path = "core.rs"]
mod serial_core;
mod types;

pub use self::{queue::*, raw::*, serial_core::*, types::*};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    InvalidBaudrate,
    UnsupportedDataBits,
    UnsupportedStopBits,
    UnsupportedParity,
    RegisterError,
    Timeout,
}

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransBytesError {
    pub bytes_transferred: usize,
    pub kind: TransferError,
}

impl Display for TransBytesError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Transfer error after transferring {} bytes: {}",
            self.bytes_transferred, self.kind
        )
    }
}

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferError {
    #[error("Data overrun by `{0:#x}`")]
    Overrun(u8),
    #[error("Parity error")]
    Parity,
    #[error("Framing error")]
    Framing,
    #[error("Break condition")]
    Break,
    #[error("Serial closed")]
    Closed,
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

bitflags! {
    /// Polling-only serial events for direct raw users such as someboot.
    ///
    /// Runtime `SerialIrqHandler` does not use this high-level snapshot type;
    /// it uses `IrqSnapshot`, `RxSample`, and TX/RX software FIFOs instead.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SerialEvent: u32 {
        const RX_READY = 0x01;
        const TX_READY = 0x02;
        const RX_ERROR = 0x04;
        const TX_ERROR = 0x08;
        const OVERRUN = 0x10;
        const MODEM_STATUS = 0x20;
        const IRQ_ACK = 0x40;
    }
}

impl SerialEvent {
    pub fn rx_ready(&self) -> bool {
        self.contains(Self::RX_READY)
    }

    pub fn tx_ready(&self) -> bool {
        self.contains(Self::TX_READY)
    }

    pub fn rx_error(&self) -> bool {
        self.intersects(Self::RX_ERROR | Self::OVERRUN)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialDirection {
    Input,
    Output,
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub baudrate: Option<u32>,
    pub data_bits: Option<DataBits>,
    pub stop_bits: Option<StopBits>,
    pub parity: Option<Parity>,
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn baudrate(mut self, baudrate: u32) -> Self {
        self.baudrate = Some(baudrate);
        self
    }

    pub fn data_bits(mut self, data_bits: DataBits) -> Self {
        self.data_bits = Some(data_bits);
        self
    }

    pub fn stop_bits(mut self, stop_bits: StopBits) -> Self {
        self.stop_bits = Some(stop_bits);
        self
    }

    pub fn parity(mut self, parity: Parity) -> Self {
        self.parity = Some(parity);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_event_reports_readiness_and_errors() {
        let event = SerialEvent::RX_READY | SerialEvent::OVERRUN;

        assert!(event.rx_ready());
        assert!(!event.tx_ready());
        assert!(event.rx_error());
    }
}
