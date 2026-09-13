#![no_std]
//! OS-independent AIC8800 Wi-Fi driver core.
//!
//! The core owns protocol state and emits typed SDIO operations. It never owns
//! an executor, interrupt registration, clock source, or synchronization
//! primitive. A single outer owner advances [`AicDevice`] with explicit time,
//! SDIO completions, card-interrupt snapshots, control requests, and TX data.

extern crate alloc;

pub mod common;
mod device;
mod firmware;
mod lmac;
mod profile;
mod protocol;
#[cfg(feature = "rdif")]
mod rdif;
mod registers;
mod rx;
mod tx;
mod wpa2;

pub use common::ChipVariant;
pub use device::{
    AicAction, AicDevice, AicError, AicEvent, AicInput, AicInputEvent, AicState, ControlRequest,
    Entropy, IrqSnapshot, MailboxRequest, MailboxWaitPhase, MonotonicTime, Pmk, SdioCompletion,
    SdioFailure, SdioRequest, SdioRequestKind, SdioResponse, TxToken,
};
#[cfg(feature = "rdif")]
pub use rdif::{AicRdifDevice, AicRdifError, AicRdifOptions, AicSdioIdentity};
pub use sdmmc_protocol::sdio::io::{AddressMode, FunctionNumber, IoAddress, TransferMode};
