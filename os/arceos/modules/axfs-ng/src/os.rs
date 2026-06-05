use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use spin::Once;

pub trait BlockTimeProvider: Send + Sync {
    fn wall_time(&self) -> Duration;
}

pub trait AddressTranslator: Send + Sync {
    fn virt_to_phys(&self, vaddr: usize) -> usize;
}

static TIME_PROVIDER: Once<&'static dyn BlockTimeProvider> = Once::new();
static ADDRESS_TRANSLATOR: Once<&'static dyn AddressTranslator> = Once::new();
static INIT_FLAGS: AtomicUsize = AtomicUsize::new(0);

const TIME_READY: usize = 1 << 0;
const ADDR_READY: usize = 1 << 1;

pub fn set_time_provider(provider: &'static dyn BlockTimeProvider) {
    TIME_PROVIDER.call_once(|| provider);
    INIT_FLAGS.fetch_or(TIME_READY, Ordering::AcqRel);
}

pub fn wall_time() -> Duration {
    TIME_PROVIDER
        .get()
        .map(|provider| provider.wall_time())
        .unwrap_or_else(|| Duration::new(0, 0))
}

pub fn set_address_translator(translator: &'static dyn AddressTranslator) {
    ADDRESS_TRANSLATOR.call_once(|| translator);
    INIT_FLAGS.fetch_or(ADDR_READY, Ordering::AcqRel);
}

pub fn virt_to_phys(vaddr: usize) -> Option<usize> {
    ADDRESS_TRANSLATOR
        .get()
        .map(|translator| translator.virt_to_phys(vaddr))
}

pub fn has_time_provider() -> bool {
    INIT_FLAGS.load(Ordering::Acquire) & TIME_READY != 0
}

pub fn has_address_translator() -> bool {
    INIT_FLAGS.load(Ordering::Acquire) & ADDR_READY != 0
}
