//! Phytium MCI/FSDIF host controller backend for `sdmmc-protocol`.
//!
//! The register layout is the Phytium Memory Card Interface found on E2000
//! class SoCs. It is close to the DesignWare MSHC programming model, with
//! Phytium-specific clock-source and timing registers.
//!
//! # Scope
//!
//! - **Implemented**: controller/FIFO reset, power and clock setup, Phytium
//!   timing tables, 1-bit / 4-bit / 8-bit bus selection, command response
//!   decoding, FIFO and IDMAC block transfers, and stable IRQ event extraction.
//! - **Out of scope for this crate**: FDT/ACPI probe, MMIO remapping, IRQ
//!   registration, pad-controller programming, OS sleeps/wakeups, and rdif-block
//!   registration.
//! - **Implemented for block I/O**: IDMAC descriptor setup, DMA buffer mapping,
//!   an owned IRQ-driven RDIF queue, and FIFO transfers limited to the explicit
//!   initialization/native-protocol path.

#![no_std]
#![allow(clippy::missing_safety_doc)]

extern crate alloc;

use alloc::sync::Arc;
use core::{marker::PhantomData, num::NonZeroUsize, ptr::NonNull};

mod command;
mod dma;
mod host;
mod lifecycle;
pub mod rdif;
mod regs;
mod timing;

pub use dma::{BlockRequest, BlockRequestSlot, RequestId};
use host::uhs_bits_after_voltage;
pub use host::{DEFAULT_FIFO_OFFSET, PhytiumMci};
pub use lifecycle::PhytiumMciRecoveryState;
use regs::RegisterBlockVolatileFieldAccess;
pub use sdmmc_protocol::block::{
    BlockBufferConfig, BlockPoll, BlockRequestId, BlockTransferDirection, BlockTransferMode,
    BlockTransferState,
};
use sdmmc_protocol::{
    DataCommandPoll, OperationPoll,
    cmd::{Command, DataDirection},
    error::{Error, ErrorContext, Phase},
    sdio::host::{
        BusWidth, ClockSpeed, HostEvent, HostEventKind, HostEventSource, ReadyBusRequest,
        SdioBusOp, SdioHost as ProtocolSdioHost, SdioIrqHost, SdioIrqSource, SignalVoltage,
        poll_ready_bus_op,
    },
};

mod event;
mod host2;
mod protocol;

#[cfg(test)]
pub(crate) use event::MCI_INT_DATA_CRC_ERROR;
pub use event::{Event, PhytiumMciIrqControl, PhytiumMciIrqEndpoint, PhytiumMciIrqSource};
pub(crate) use event::{
    MCI_IDSTS_ERROR_MASK, MCI_IDSTS_RECEIVE, MCI_IDSTS_TRANSMIT, MCI_INT_COMMAND_DONE,
    MCI_INT_DATA_TRANSFER_OVER, MCI_INT_ERROR_MASK, MCI_INT_RXDR, MCI_INT_TXDR,
};
pub use host2::{BusRequest, DataRequest, TransactionRequest};

#[cfg(test)]
mod tests;
