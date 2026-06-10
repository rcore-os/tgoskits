//! ArceOS timing glue for the `sdhci-cv1800` driver core.
//!
//! Implements [`sdhci_cv1800::SdhciDelay`] on top of `ax-task` and installs it
//! into the driver core. The core crate itself has no ArceOS dependency; this
//! crate is the OS adapter layer, kept separate so the core can sit below
//! `ax-hal` in the crate graph (e.g. be used by `ax-driver`).

#![no_std]

use core::time::Duration;

use sdhci_cv1800::SdhciDelay;

/// ArceOS-backed delay/yield provider.
pub struct ArceosDelay;

impl SdhciDelay for ArceosDelay {
    fn delay_ms(&self, ms: u64) {
        ax_task::sleep(Duration::from_millis(ms));
    }

    fn yield_now(&self) {
        ax_task::yield_now();
    }
}

static ARCEOS_DELAY: ArceosDelay = ArceosDelay;

/// Installs the ArceOS timing provider into the driver core. Call once during
/// init, before any SDHCI operation that delays.
pub fn install_delay() {
    sdhci_cv1800::set_delay(&ARCEOS_DELAY);
}
