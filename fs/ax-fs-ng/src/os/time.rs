use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ax_lazyinit::OnceLock;

pub trait BlockTimeProvider: Send + Sync {
    /// Returns realtime since the Unix epoch for persistent timestamps.
    fn wall_time(&self) -> Duration;

    /// Returns monotonic time since boot for deadlines and elapsed durations.
    fn monotonic_time(&self) -> Duration;
}

static TIME_PROVIDER: OnceLock<&'static dyn BlockTimeProvider> = OnceLock::new();
static TIME_READY: AtomicBool = AtomicBool::new(false);

pub fn set_time_provider(provider: &'static dyn BlockTimeProvider) {
    TIME_PROVIDER.call_once(|| provider);
    TIME_READY.store(true, Ordering::Release);
}

pub fn wall_time() -> Duration {
    TIME_PROVIDER
        .get()
        .map(|provider| provider.wall_time())
        .unwrap_or_else(|| Duration::new(0, 0))
}

pub fn monotonic_time() -> Duration {
    TIME_PROVIDER
        .get()
        .map(|provider| provider.monotonic_time())
        .unwrap_or_else(|| Duration::new(0, 0))
}

pub fn has_time_provider() -> bool {
    TIME_READY.load(Ordering::Acquire)
}
