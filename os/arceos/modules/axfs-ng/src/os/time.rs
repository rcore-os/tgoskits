use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use spin::Once;

pub trait BlockTimeProvider: Send + Sync {
    fn wall_time(&self) -> Duration;
}

static TIME_PROVIDER: Once<&'static dyn BlockTimeProvider> = Once::new();
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

pub fn has_time_provider() -> bool {
    TIME_READY.load(Ordering::Acquire)
}
