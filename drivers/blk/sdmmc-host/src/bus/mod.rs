use core::{fmt, num::NonZeroU16};

use crate::command::Command;

/// SD/SDIO/MMC bus width.
///
/// This is a closed protocol set. Keeping it exhaustive makes every host
/// choose the exact hardware encoding instead of silently guessing for an
/// unknown width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusWidth {
    Bit1,
    Bit4,
    Bit8,
}

/// Named card clock modes used by SD/MMC protocol state machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClockSpeed {
    Identification,
    Default,
    HighSpeed,
    Sdr12,
    Sdr25,
    Sdr50,
    Sdr104,
    Ddr50,
    Hs200,
}

/// Concrete clock frequency request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClockHz(pub u32);

/// Bus signaling voltage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignalVoltage {
    V330,
    V180,
    V120,
}

/// Non-data bus operation that may itself need asynchronous completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BusOp {
    ResetAll,
    ResetCommandLine,
    ResetDataLine,
    PowerOn,
    PowerOff,
    SetClock(ClockSpeed),
    SetClockHz(ClockHz),
    SetBusWidth(BusWidth),
    SetSignalVoltage(SignalVoltage),
    ExecuteTuning {
        command: Command,
        block_size: NonZeroU16,
    },
}

/// Host/bus-layer error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    Busy,
    Timeout,
    Crc,
    NoCard,
    Unsupported,
    InvalidArgument,
    Misaligned,
    Bus,
    Controller,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Busy => "host bus is busy",
            Self::Timeout => "host bus timeout",
            Self::Crc => "host bus CRC error",
            Self::NoCard => "no card present",
            Self::Unsupported => "operation is not supported",
            Self::InvalidArgument => "invalid host bus argument",
            Self::Misaligned => "misaligned host bus buffer",
            Self::Bus => "host bus error",
            Self::Controller => "host controller error",
        };
        f.write_str(s)
    }
}

impl core::error::Error for Error {}
