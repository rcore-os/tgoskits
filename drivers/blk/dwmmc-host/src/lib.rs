//! Synopsys DesignWare Mobile Storage Host Controller (DW_mshc) backend
//! for the [`sdmmc-protocol`](sdmmc_protocol) driver crate.
//!
//! Implements [`sdio_host2::SdioHost`] for the IP block known
//! variously as DWC_mobile_storage, dw_mshc, dw_mmc (Linux), or simply
//! the "Synopsys SD/MMC controller" — the same core used in Rockchip
//! RK33xx/RK35xx, Allwinner A-series, StarFive JH7110, and a long
//! tail of mid-range SoCs. Initialization may use the FIFO path; the RDIF
//! runtime exposes only owned, interrupt-driven IDMAC requests.
//!
//! # Scope
//!
//! - **Implemented**: PIO data transfer over the 0x100/0x200/0x400
//!   FIFO (configurable), IDMAC descriptor transfers,
//!   1-bit / 4-bit / 8-bit bus selection,
//!   default / high-speed / UHS-I / HS200 clocking, DW_mshc UHS DDR
//!   and 1.8 V signaling bits, R1/R1b/R2/R3/R4/R5/R6/R7 response
//!   decoding, software reset.
//! - **Out of scope (for now)**: external-DMA path, controller-specific
//!   DLL/strobe/tuning window setup (CMD19/CMD21).
//!
//! # Usage
//!
//! ```rust,no_run
//! use core::ptr::NonNull;
//!
//! use dwmmc_host::DwMmc;
//! use sdmmc_protocol::sdio::{card::SdioSdmmc, init::SdioInitScratch};
//!
//! // SAFETY: 0xFE2B_0000 must point at a valid DW_mshc register file
//! // the caller has exclusive access to.
//! let mmio = NonNull::new(0xFE2B_0000 as *mut u8).unwrap();
//! let mut host = unsafe { DwMmc::new(mmio) };
//! host.set_reference_clock(50_000_000);
//! // Optional DMA capability can be installed here before the protocol layer
//! // owns the host.
//!
//! let mut card = SdioSdmmc::new_host2(host);
//! let mut scratch = SdioInitScratch::new();
//! let mut request = card.submit_init(&mut scratch)?;
//! // Re-enter the initialization state machine according to its schedule.
//! # Ok::<(), sdmmc_protocol::Error>(())
//! ```
//!
//! The shared worker, IRQ registration, and blocking policy belong in
//! OS/platform glue. The reusable driver exposes the RDIF owned queue through
//! [`rdif`] and does not provide a task-side completion-poll fallback:
//!
//! ```compile_fail
//! use dwmmc_host::BlockQueue;
//! ```
//!
//! Construction is `unsafe` because the caller must guarantee that
//! the supplied address is a valid, exclusively-owned DW_mshc
//! register file.

#![no_std]

extern crate alloc;

use alloc::sync::Arc;
use core::{marker::PhantomData, num::NonZeroUsize, ptr::NonNull};

use log::warn;

mod command;
mod dma;
mod event;
mod host;
mod host2;
mod lifecycle;
mod protocol;
pub mod rdif;
mod regs;
mod timing;

#[cfg(test)]
mod tests;

pub use sdmmc_protocol::block::{
    BlockBufferConfig, BlockPoll, BlockRequestId, BlockTransferDirection, BlockTransferMode,
    BlockTransferState,
};
use sdmmc_protocol::{
    DataCommandPoll, OperationPoll,
    cmd::{Command, DataDirection},
    error::{Error, ErrorContext, Phase},
    sdio::{
        host::{
            BusWidth, ClockSpeed, HostEvent, HostEventKind, HostEventSource, ReadyBusRequest,
            SdioBusOp, SdioHost as ProtocolSdioHost, SdioIrqHandle, SdioIrqHost, SignalVoltage,
            poll_ready_bus_op,
        },
        host2::SdioHost2Lifecycle,
    },
};

use crate::regs::RegisterBlockVolatileFieldAccess;
pub use crate::{
    dma::{
        BlockRequest, BlockRequestSlot, IDMAC_DESC_ALIGN, IDMAC_DESC_SIZE, PreparedDmaSubmitError,
        RequestId,
    },
    event::{DwMmcIrq, Event},
    host::{CardDetect, DEFAULT_FIFO_OFFSET, DwMmc, HostClock},
    host2::{BusRequest, DataRequest, TransactionRequest},
    lifecycle::DwMmcRecoveryState,
};
pub(crate) use crate::{
    event::{
        DWMMC_INT_COMMAND_DONE, DWMMC_INT_DATA_TRANSFER_OVER, DWMMC_INT_ERROR_MASK, DWMMC_INT_RXDR,
        DWMMC_INT_TXDR,
    },
    timing::{UhsBits, uhs_bits_after_speed, uhs_bits_after_voltage, volt_mask_for_signal},
};
