//! CV181x/SG2002 SD-card host wrapper for the generic SDHCI backend.
//!
//! This crate owns only the Cvitek-specific top/pinmux/PHY programming around
//! the controller. Command, data, interrupt-status caching, and owned-DMA
//! transaction progression are delegated to [`sdhci_host::Sdhci`].

#![no_std]

use dma_api::DeviceDma;
use sdhci_host::Sdhci;
use sdmmc_protocol::Error as ProtocolError;

mod board;
mod clock;
mod host2;
mod platform;
mod sdio1;

pub use host2::BusRequest;
pub use platform::{Cv181xConfig, Cv181xMmio};
pub use sdio1::{CV181X_SDIO1_RESET_SETTLE, Cv181xSdio1Mmio};

/// CV181x SD-card host endpoint.
pub struct Cv181xSdhci {
    inner: Sdhci,
    mmio: Cv181xMmio,
    config: Cv181xConfig,
    controller: ControllerResources,
}

#[derive(Clone, Copy)]
enum ControllerResources {
    Sd,
    Sdio1(Cv181xSdio1Mmio),
}

// SAFETY: The wrapper owns exclusive access to one SDHCI register file and the
// board-level syscon/pinmux window for the controller lifetime. It does not
// expose shared mutable access; IRQ extraction uses the cloned SDHCI IRQ core.
unsafe impl Send for Cv181xSdhci {}

impl Cv181xSdhci {
    /// Construct a CV181x SD-card host over already-mapped MMIO.
    ///
    /// # Safety
    ///
    /// `mmio.core` must point to an exclusively-owned CV181x SDHCI register
    /// block and `mmio.syscon` must cover TOP_BASE including the pinmux block.
    pub unsafe fn new(mmio: Cv181xMmio, config: Cv181xConfig) -> Self {
        let inner = unsafe { Sdhci::new(mmio.core()) };
        let mut this = Self {
            inner,
            mmio,
            config: config.normalized(),
            controller: ControllerResources::Sd,
        };
        this.restore_ds_hs_phy();
        this
    }

    /// Construct the SDIO1 instance after applying its SoC clock, reset,
    /// pinmux, pull-up, and card-detect policy.
    ///
    /// # Safety
    ///
    /// Every mapping in `mmio` must be valid and exclusively owned for the
    /// returned controller lifetime. The runtime must observe
    /// [`CV181X_SDIO1_RESET_SETTLE`] before issuing the first card command.
    pub unsafe fn new_sdio1(mmio: Cv181xSdio1Mmio, config: Cv181xConfig) -> Self {
        let host = mmio.host();
        let inner = unsafe { Sdhci::new(host.core()) };
        let mut this = Self {
            inner,
            mmio: host,
            config: config.normalized(),
            controller: ControllerResources::Sdio1(mmio),
        };
        this.restore_controller_after_reset();
        this
    }

    pub const fn config(&self) -> Cv181xConfig {
        self.config
    }

    pub fn inner(&self) -> &Sdhci {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut Sdhci {
        &mut self.inner
    }

    pub fn into_inner(self) -> Sdhci {
        self.inner
    }

    pub fn configure_dma(&mut self, dma: DeviceDma) -> Result<(), ProtocolError> {
        self.inner.configure_dma(dma)
    }
}

fn map_protocol_error(err: ProtocolError) -> sdmmc_host::Error {
    match err {
        ProtocolError::Timeout(_) => sdmmc_host::Error::Timeout,
        ProtocolError::Crc(_) => sdmmc_host::Error::Crc,
        ProtocolError::NoCard => sdmmc_host::Error::NoCard,
        ProtocolError::Busy => sdmmc_host::Error::Busy,
        ProtocolError::UnsupportedCommand => sdmmc_host::Error::Unsupported,
        ProtocolError::Misaligned => sdmmc_host::Error::Misaligned,
        ProtocolError::InvalidArgument => sdmmc_host::Error::InvalidArgument,
        ProtocolError::BusError(_) => sdmmc_host::Error::Bus,
        ProtocolError::ReadError(_)
        | ProtocolError::WriteError(_)
        | ProtocolError::BadResponse(_) => sdmmc_host::Error::Bus,
        ProtocolError::CardError(_) | ProtocolError::CardLocked => sdmmc_host::Error::Controller,
        _ => sdmmc_host::Error::Controller,
    }
}

#[cfg(test)]
mod tests;
