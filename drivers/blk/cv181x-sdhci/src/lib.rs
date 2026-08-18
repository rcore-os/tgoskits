//! CV181x/SG2002 SD-card host wrapper for the generic SDHCI backend.
//!
//! This crate owns only the Cvitek-specific top/pinmux/PHY programming around
//! the controller. Command, data, interrupt-status caching, and owned-DMA
//! transaction progression are delegated to [`sdhci_host::Sdhci`].

#![no_std]

use core::num::NonZeroU32;

use dma_api::DeviceDma;
use sdhci_host::Sdhci;
use sdmmc_protocol::Error as ProtocolError;

mod board;
mod clock;
mod host2;
mod platform;
#[cfg(test)]
mod tests;

pub use host2::BusRequest;
pub use platform::{CV181X_SYSCON_REQUIRED_SIZE, CV181X_TOP_SYSCON_BASE, Cv181xConfig, Cv181xMmio};

/// CV181x SD-card host endpoint.
pub struct Cv181xSdhci {
    inner: Sdhci,
    mmio: Cv181xMmio,
    config: Cv181xConfig,
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
        let config = config.normalized();
        let mut inner = unsafe { Sdhci::new(mmio.core()) };
        let source_clock = NonZeroU32::new(config.src_frequency_hz)
            .expect("normalized CV181x source clock must be non-zero");
        inner
            .set_fixed_base_clock_hz(source_clock)
            .expect("a newly constructed SDHCI host must be idle");
        let mut this = Self {
            inner,
            mmio,
            config,
        };
        this.restore_ds_hs_phy();
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
