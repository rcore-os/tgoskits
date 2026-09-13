//! Synopsys DesignWare Mobile Storage Host Controller (DW_mshc) backend
//! for the [`sdmmc-protocol`](sdmmc_protocol) driver crate.
//!
//! Implements [`sdmmc_host::SdMmcHost`] for the IP block known
//! variously as DWC_mobile_storage, dw_mshc, dw_mmc (Linux), or simply
//! the "Synopsys SD/MMC controller" — the same core used in Rockchip
//! RK33xx/RK35xx, Allwinner A-series, StarFive JH7110, and a long
//! tail of mid-range SoCs. Block I/O uses the internal DMAC (IDMAC)
//! exclusively and completes through acknowledged IRQ events.
//!
//! # Scope
//!
//! - **Implemented**: IDMAC descriptor transfers,
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
//! use dma_api::DeviceDma;
//! use dwmmc_host::DwMmc;
//! use sdmmc_protocol::sdio::native::SdMmcCard;
//!
//! // SAFETY: 0xFE2B_0000 must point at a valid DW_mshc register file
//! // the caller has exclusive access to.
//! let mmio = NonNull::new(0xFE2B_0000 as *mut u8).unwrap();
//! let mut host = unsafe { DwMmc::new(mmio) };
//! host.set_reference_clock(50_000_000);
//! let dma: DeviceDma = todo!("install the platform DMA capability");
//! host.configure_dma(dma)?;
//!
//! let mut card = SdMmcCard::new(host);
//! let mut request = card.submit_init()?;
//! // Advance `request` only for acknowledged IRQs or bounded register retries.
//! # Ok::<(), sdmmc_protocol::Error>(())
//! ```
//!
//! The runtime block queue adapter belongs in OS/platform glue. The reusable
//! driver crate exposes request state and event-driven advance primitives instead:
//!
//! ```compile_fail
//! use dwmmc_host::BlockQueue;
//! ```
//!
//! Construction is `unsafe` because the caller must guarantee that
//! the supplied address is a valid, exclusively-owned DW_mshc
//! register file.

#![no_std]
#![allow(clippy::missing_safety_doc)]

extern crate alloc;

use alloc::sync::Arc;
use core::{marker::PhantomData, ptr::NonNull, time::Duration};

use log::warn;

mod command;
mod dma;
mod fifo;
mod host;
mod regs;

pub use sdmmc_protocol::block::{
    BlockRequestId, BlockTransferDirection, BlockTransferMode, BlockTransferState,
};
use sdmmc_protocol::{
    CommandResponseProgress, DataCommandProgress,
    cmd::{Command, DataDirection},
    error::{Error, ErrorContext, Phase},
    sdio::host::{
        BusWidth, ClockSpeed, HostEvent, HostEventKind, HostEventSource, SdMmcIrqHandle,
        SdMmcIrqHost, SignalVoltage,
    },
};

use crate::regs::RegisterBlockVolatileFieldAccess;
pub use crate::{
    dma::{
        BlockRequest, BlockRequestSlot, IDMAC_DESC_ALIGN, IDMAC_DESC_SIZE, IDMAC_MAX_BLOCKS,
        IDMAC_MAX_TRANSFER_SIZE, RequestId,
    },
    fifo::{FifoConfig, FifoDataWidth},
    host::{CardDetect, DwMmc, HostClock},
};

/// Stable controller event extracted from DW_mshc raw interrupt status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Event {
    /// No status bit requiring runtime action is currently pending.
    #[default]
    None,
    /// A command response has completed.
    CommandComplete,
    /// A data transfer has completed.
    TransferComplete,
    /// One or more controller error bits are pending.
    Error { raw_status: u32 },
    /// Status bits are pending but do not map to a high-level event yet.
    Other { raw_status: u32 },
}

mod host2;
pub use host2::{BusRequest, DataRequest, DwMmcIrq, TransactionRequest};
pub(crate) use host2::{
    DWMMC_INT_COMMAND_DONE, DWMMC_INT_DATA_TRANSFER_OVER, DWMMC_INT_ERROR_MASK,
    DWMMC_LATCH_IDMAC_COMPLETE, DWMMC_LATCH_IDMAC_ERROR,
};

fn clock_hz_for_speed(speed: ClockSpeed) -> u32 {
    match speed {
        ClockSpeed::Identification => 400_000,
        ClockSpeed::Default | ClockSpeed::Sdr12 => 25_000_000,
        ClockSpeed::HighSpeed | ClockSpeed::Sdr25 => 50_000_000,
        ClockSpeed::Sdr50 | ClockSpeed::Ddr50 => 50_000_000,
        ClockSpeed::Sdr104 => 104_000_000,
        ClockSpeed::Hs200 => 200_000_000,
        // Future ClockSpeed variants: unknown frequency, signal 0.
        _ => 0,
    }
}

fn dwmmc_clock_divisor(ref_clock_hz: u32, target_hz: u32) -> u8 {
    if ref_clock_hz == 0 || target_hz == 0 || target_hz >= ref_clock_hz {
        0
    } else {
        ref_clock_hz.div_ceil(2 * target_hz).min(0xFF) as u8
    }
}

pub(crate) fn ddr_mask_for_speed(speed: ClockSpeed) -> u16 {
    match speed {
        ClockSpeed::Ddr50 => 1,
        _ => 0,
    }
}

pub(crate) fn volt_mask_for_signal(voltage: SignalVoltage) -> Result<u16, Error> {
    match voltage {
        SignalVoltage::V330 => Ok(0),
        SignalVoltage::V180 => Ok(1),
        SignalVoltage::V120 => Err(Error::UnsupportedCommand),
        // Future SignalVoltage variants are not supported by this controller.
        _ => Err(Error::UnsupportedCommand),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UhsBits {
    pub ddr: u16,
    pub volt: u16,
}

pub(crate) fn uhs_bits_after_speed(cur: UhsBits, speed: ClockSpeed) -> UhsBits {
    UhsBits {
        ddr: ddr_mask_for_speed(speed),
        ..cur
    }
}

pub(crate) fn uhs_bits_after_voltage(
    cur: UhsBits,
    voltage: SignalVoltage,
) -> Result<UhsBits, Error> {
    Ok(UhsBits {
        volt: volt_mask_for_signal(voltage)?,
        ..cur
    })
}

#[cfg(test)]
mod tests;
