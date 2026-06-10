//! ArceOS runtime glue for the `aic8800` Wi-Fi driver core.
//!
//! Implements [`wifi_host::WifiRuntime`] on top of ArceOS's `ax-task` / `ax-hal`
//! and installs it into the driver core. The core crate itself contains no
//! reference to any ArceOS crate; this crate is the OS adapter layer, kept
//! separate so the core can sit below `ax-hal` in the crate graph (e.g. be used
//! by `ax-driver`).
//!
//! [`install_runtime`] also installs the sibling `sdhci-cv1800` delay glue, so a
//! single call wires up the whole SG2002 Wi-Fi stack's runtime capabilities.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::{future::poll_fn, time::Duration};

use wifi_host::{PollFn, SendPollFn, TimedOut, WifiRuntime};

/// ArceOS-backed implementation of the Wi-Fi driver's runtime capabilities.
pub struct ArceosWifiRuntime;

impl WifiRuntime for ArceosWifiRuntime {
    fn now_nanos(&self) -> u64 {
        ax_hal::time::monotonic_time_nanos()
    }

    fn sleep_ms(&self, ms: u64) {
        ax_task::sleep(Duration::from_millis(ms));
    }

    fn yield_now(&self) {
        ax_task::yield_now();
    }

    fn spawn_poll_task(&self, name: &str, mut poll: Box<SendPollFn>) {
        ax_task::spawn_with_name(
            move || {
                ax_task::future::block_on(poll_fn(move |cx| poll(cx)));
            },
            name.into(),
        );
    }

    fn block_until(&self, timeout_ms: Option<u64>, poll: &mut PollFn<'_>) -> Result<(), TimedOut> {
        let fut = poll_fn(|cx| poll(cx));
        match timeout_ms {
            Some(ms) => {
                match ax_task::future::block_on(ax_task::future::timeout(
                    Some(Duration::from_millis(ms)),
                    fut,
                )) {
                    Ok(()) => Ok(()),
                    Err(_) => Err(TimedOut),
                }
            }
            None => {
                ax_task::future::block_on(fut);
                Ok(())
            }
        }
    }
}

static ARCEOS_RUNTIME: ArceosWifiRuntime = ArceosWifiRuntime;

/// Installs the ArceOS runtime into the aic8800 driver core *and* the
/// sdhci-cv1800 delay glue. Call once during init, before any Wi-Fi operation.
pub fn install_runtime() {
    sdhci_cv1800_arceos::install_delay();
    aic8800::set_runtime(&ARCEOS_RUNTIME);
}
