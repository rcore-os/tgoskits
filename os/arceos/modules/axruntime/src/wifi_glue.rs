//! ArceOS runtime glue for the OS-independent Wi-Fi driver cores.
//!
//! The `aic8800` and `sdhci-cv1800` driver cores declare no ArceOS dependency;
//! they reach timing / delay / yield / task-spawning through injected provider
//! traits ([`aic8800::WifiRuntime`], [`sdhci_cv1800::SdhciDelay`]). This module
//! implements those over the ArceOS task facade / `ax-hal` and installs them, so a single
//! [`install_runtime`] call wires up the whole SG2002 Wi-Fi stack.
//!
//! It lives in `axruntime` (rather than a standalone glue crate) because that is
//! where the OS already owns the scheduler / `ax-hal` runtime; keeping it here
//! avoids an extra adapter crate per driver.

use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, task::Wake};
use core::{task::Waker, time::Duration};

use aic8800::{PollFn, SendPollFn, TimedOut, WifiRuntime};
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use sdhci_cv1800::SdhciDelay;

use crate::task::{ThreadId, ThreadWakeHandle, WaitQueue};

// One owner reference remains until shutdown, so Waker clone/drop/wake from a
// hard IRQ can never perform the final Arc release or free scheduler wake data.
static POLL_WAKERS: LazyInit<SpinNoIrq<BTreeMap<ThreadId, Arc<PollWake>>>> = LazyInit::new();
const WIFI_TASK_STACK_SIZE: usize = 256 * 1024;

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
        if let Err(error) = crate::task::yield_current_cpu() {
            warn!("Wi-Fi runtime could not yield the current task: {error}");
        }
    }

    fn spawn_poll_task(&self, name: &str, mut poll: Box<SendPollFn>) {
        let result = crate::task::spawn_raw(
            move || {
                block_on_poll(&mut *poll, None)
                    .expect("an unbounded Wi-Fi polling task cannot time out");
            },
            name.into(),
            WIFI_TASK_STACK_SIZE,
        );
        if let Err(error) = result {
            warn!("failed to spawn Wi-Fi polling task {name}: {error}");
        }
    }

    fn block_until(&self, timeout_ms: Option<u64>, poll: &mut PollFn<'_>) -> Result<(), TimedOut> {
        block_on_poll(poll, timeout_ms.map(Duration::from_millis))
    }
}

struct PollWake {
    thread: ThreadWakeHandle,
}

impl Wake for PollWake {
    fn wake(self: Arc<Self>) {
        let _ = self.thread.wake();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.thread.wake();
    }
}

fn block_on_poll<F>(poll: &mut F, timeout: Option<Duration>) -> Result<(), TimedOut>
where
    F: FnMut(&mut core::task::Context<'_>) -> core::task::Poll<()> + ?Sized,
{
    let owner_wake = poll_wake_for_current();
    let waker = Waker::from(owner_wake);
    let mut context = core::task::Context::from_waker(&waker);
    let park = WaitQueue::new();
    let deadline_ns = timeout.map(|timeout| {
        ax_hal::time::monotonic_time_nanos()
            .saturating_add(timeout.as_nanos().min(u64::MAX as u128) as u64)
    });

    loop {
        if poll(&mut context).is_ready() {
            return Ok(());
        }
        let Some(deadline_ns) = deadline_ns else {
            park.wait();
            continue;
        };
        let now_ns = ax_hal::time::monotonic_time_nanos();
        if now_ns >= deadline_ns {
            return Err(TimedOut);
        }
        if park.wait_timeout(Duration::from_nanos(deadline_ns - now_ns)) {
            return Err(TimedOut);
        }
    }
}

fn poll_wake_for_current() -> Arc<PollWake> {
    let current = crate::task::current_thread_handle()
        .unwrap_or_else(|error| panic!("Wi-Fi polling requires a scheduler thread: {error}"));
    let poll_wakers = POLL_WAKERS.get_or_init(|| SpinNoIrq::new(BTreeMap::new()));
    let mut poll_wakers = poll_wakers.lock();
    Arc::clone(poll_wakers.entry(current.id()).or_insert_with(|| {
        Arc::new(PollWake {
            thread: current.wake_handle(),
        })
    }))
}

/// ArceOS-backed SDHCI delay/yield provider.
struct ArceosDelay;

impl SdhciDelay for ArceosDelay {
    fn delay_ms(&self, ms: u64) {
        crate::task::sleep(Duration::from_millis(ms));
    }

    fn yield_now(&self) {
        if let Err(error) = crate::task::yield_current_cpu() {
            warn!("SDHCI runtime could not yield the current task: {error}");
        }
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
