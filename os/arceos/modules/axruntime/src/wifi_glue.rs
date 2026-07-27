//! ArceOS runtime glue for the OS-independent Wi-Fi driver cores.
//!
//! The `aic8800` and `sdhci-cv1800` driver cores declare no ArceOS dependency;
//! they reach timing / delay / yield / task-spawning through injected provider
//! traits ([`aic8800::WifiRuntime`], [`sdhci_cv1800::SdhciDelay`]). This module
//! implements those over `ax-task` / `ax-hal` and installs them, so a single
//! [`install_runtime`] call wires up the whole SG2002 Wi-Fi stack.
//!
//! It lives in `axruntime` (rather than a standalone glue crate) because that is
//! where the OS already owns the `ax-task` / `ax-hal` runtime; keeping it here
//! avoids an extra adapter crate per driver.

use alloc::boxed::Box;
use core::{future::poll_fn, time::Duration};

use aic8800::{PollFn, SendPollFn, TimedOut, WifiRuntime};
use sdhci_cv1800::SdhciDelay;

/// ArceOS-backed implementation of the Wi-Fi driver's runtime capabilities.
struct ArceosWifiRuntime;

impl WifiRuntime for ArceosWifiRuntime {
    fn now_nanos(&self) -> u64 {
        ax_hal::time::monotonic_time_nanos()
    }

    fn sleep_ms(&self, ms: u64) {
        crate::task::sleep(Duration::from_millis(ms));
    }

    fn yield_now(&self) {
        crate::task::yield_current_cpu()
            .unwrap_or_else(|error| panic!("failed to yield Wi-Fi runtime thread: {error}"));
    }

    fn spawn_poll_task(&self, name: &str, mut poll: Box<SendPollFn>) {
        crate::task::spawn_raw(
            move || {
                crate::task::block_on(poll_fn(move |cx| poll(cx)));
            },
            name.into(),
            crate::task::default_task_stack_size(),
        )
        .unwrap_or_else(|error| panic!("failed to spawn Wi-Fi polling task: {error}"));
    }

    fn block_until(&self, timeout_ms: Option<u64>, poll: &mut PollFn<'_>) -> Result<(), TimedOut> {
        let fut = poll_fn(|cx| poll(cx));
        match timeout_ms {
            Some(ms) => {
                crate::task::block_on_timeout(Duration::from_millis(ms), fut).map_err(|_| TimedOut)
            }
            None => {
                crate::task::block_on(fut);
                Ok(())
            }
        }
    }
}

/// ArceOS-backed SDHCI delay/yield provider.
struct ArceosDelay;

impl SdhciDelay for ArceosDelay {
    fn delay_ms(&self, ms: u64) {
        crate::task::sleep(Duration::from_millis(ms));
    }

    fn yield_now(&self) {
        crate::task::yield_current_cpu()
            .unwrap_or_else(|error| panic!("failed to yield SDHCI runtime thread: {error}"));
    }
}

static ARCEOS_RUNTIME: ArceosWifiRuntime = ArceosWifiRuntime;
static ARCEOS_DELAY: ArceosDelay = ArceosDelay;

/// Installs the ArceOS runtime into the aic8800 driver core *and* the
/// sdhci-cv1800 delay glue. Call once during init, before any Wi-Fi operation.
pub(crate) fn install_runtime() {
    sdhci_cv1800::set_delay(&ARCEOS_DELAY);
    aic8800::set_runtime(&ARCEOS_RUNTIME);
}
