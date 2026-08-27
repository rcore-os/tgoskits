//! CV181x/SG2002 SD-card host wrapper for the generic SDHCI backend.
//!
//! This crate owns only the Cvitek-specific top/pinmux/PHY programming around
//! the controller. Command, data, interrupt-status caching, and owned-DMA
//! transaction progression are delegated to [`sdhci_host::Sdhci`].

#![no_std]

use core::num::NonZeroU32;

use dma_api::DeviceDma;
use sdhci_host::{HostResetHook, Sdhci};
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
}

struct Cv181xResetHook {
    mmio: Cv181xMmio,
}

impl Cv181xResetHook {
    const fn new(mmio: Cv181xMmio) -> Self {
        Self { mmio }
    }
}

// SAFETY: The hook is owned by the corresponding `Sdhci` instance and is only
// invoked while that host is exclusively borrowed for a controller reset. Its
// MMIO pointer aliases the wrapper's mapping but all accesses are serialized by
// the host's mutable request path.
unsafe impl Send for Cv181xResetHook {}
// SAFETY: See the `Send` implementation. Hook callbacks serialize writes
// through the exclusively borrowed host even though the callback receiver is
// shared by the generic capability contract.
unsafe impl Sync for Cv181xResetHook {}

impl HostResetHook for Cv181xResetHook {
    fn after_reset(&self, _host: &mut Sdhci) -> Result<(), ProtocolError> {
        board::restore_ds_hs_phy(self.mmio);
        Ok(())
    }
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
        inner.set_reset_hook(Cv181xResetHook::new(mmio));
        let mut this = Self {
            inner,
            mmio,
            config,
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
        mmio.initialize();
        unsafe { Self::new(mmio.host(), config) }
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

#[cfg(test)]
mod tests;
