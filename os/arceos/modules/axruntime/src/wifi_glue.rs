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
use ax_task::WaitQueue;
use sdhci_cv1800::SdhciDelay;

/// ArceOS-backed implementation of the Wi-Fi driver's runtime capabilities.
struct ArceosWifiRuntime;

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

/// WaitQueue for PIO interrupt-driven wakeup.
/// The SDHCI ISR calls `sdhci_pio_wake_callback` which notifies this queue,
/// waking tasks blocked in `ArceosDelay::block_timeout`.
///
/// # Single-waiter invariant
///
/// At most one task is ever blocked on this queue at a time — the SDIO bus
/// lock (`SdioTransport`) serialises all transfers, so only the TX or RX
/// thread (never both) can be inside `block_timeout`.  This invariant is part
/// of the `SdhciDelay::block_timeout` contract.
static SDHCI_PIO_WQ: WaitQueue = WaitQueue::new();

fn sdhci_pio_wake_callback() {
    SDHCI_PIO_WQ.notify_one_from_irq();
}

/// ArceOS-backed SDHCI delay/wake provider.
struct ArceosDelay;

impl SdhciDelay for ArceosDelay {
    fn delay_ms(&self, ms: u64) {
        ax_task::sleep(Duration::from_millis(ms));
    }

    fn block_timeout(&self, timeout_ms: u64) -> bool {
        SDHCI_PIO_WQ.wait_timeout(Duration::from_millis(timeout_ms))
    }
}

static ARCEOS_RUNTIME: ArceosWifiRuntime = ArceosWifiRuntime;
static ARCEOS_DELAY: ArceosDelay = ArceosDelay;

/// Installs the ArceOS runtime into the aic8800 driver core *and* the
/// sdhci-cv1800 delay glue. Call once during init, before any Wi-Fi operation.
pub(crate) fn install_runtime() {
    // The ISR/task SIG_EN RMW protocol relies on single-core serialization;
    // wake correctness under SMP requires additional fencing (see irq.rs).
    debug_assert!(
        ax_hal::cpu_num() == 1,
        "sdhci-cv1800 ISR design assumes single-core; SMP not yet supported"
    );
    sdhci_cv1800::set_delay(&ARCEOS_DELAY);
    aic8800::set_runtime(&ARCEOS_RUNTIME);
    sdhci_cv1800::irq::register_pio_wake_callback(sdhci_pio_wake_callback);
}
