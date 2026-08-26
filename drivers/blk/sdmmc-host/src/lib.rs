//! Physical SD/SDIO/MMC host-bus transaction traits.
//!
//! This crate intentionally models the shared CMD/DAT bus rather than a card,
//! block device, filesystem, or runtime queue. A host accepts one transaction
//! at a time: a command, an optional data phase, and explicit task-side causes
//! that advance the controller state machine. Higher-level SD/MMC card
//! protocols live in `sdmmc-protocol`.

#![no_std]

extern crate alloc;

mod bus;
mod command;
mod data;
mod host;
mod irq;

#[cfg(test)]
mod tests;

pub use bus::{BusOp, BusWidth, ClockHz, ClockSpeed, Error, SignalVoltage};
pub use command::{Command, RawResponse, ResponseType};
pub use data::{DataBuffer, DataDirection, DataPhase, DataTransfer, DmaPhaseError};
pub use host::{
    AdvanceRequestError, ProgressCause, RequestProgress, SdMmcHost, SubmitTransactionError,
    Transaction,
};
pub use irq::HostParts;
