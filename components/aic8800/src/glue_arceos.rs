//! ArceOS runtime glue (feature `arceos`).
//!
//! This is the OS adapter layer: it implements [`wifi_host::WifiRuntime`] on top
//! of ArceOS's `ax-task` / `ax-hal` and installs it into the driver core. The
//! driver core itself (everything outside this module) contains no reference to
//! any ArceOS crate.

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

/// Installs the ArceOS runtime into the driver core. Call once during init,
/// before [`crate::fdrv::init`] or any Wi-Fi operation.
pub fn install_runtime() {
    crate::set_runtime(&ARCEOS_RUNTIME);
}
