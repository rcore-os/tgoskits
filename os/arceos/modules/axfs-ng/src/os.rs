use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use spin::{Once, RwLock};

pub trait BlockTimeProvider: Send + Sync {
    fn wall_time(&self) -> Duration;
}

pub trait AddressTranslator: Send + Sync {
    fn virt_to_phys(&self, vaddr: usize) -> usize;
}

pub trait BlockTaskOps: Send + Sync {
    fn current_task_id(&self) -> Option<u64>;
    fn task_yield(&self);
    fn task_wait(&self) {
        self.task_yield();
    }
    fn task_wait_until(&self, condition: &dyn Fn() -> bool) {
        while !condition() {
            self.task_wait();
        }
    }
    fn wake_task(&self, task_id: u64);
    fn notify_waiters(&self) {}
}

static TIME_PROVIDER: Once<&'static dyn BlockTimeProvider> = Once::new();
static ADDRESS_TRANSLATOR: Once<&'static dyn AddressTranslator> = Once::new();
static TASK_OPS: RwLock<Option<&'static dyn BlockTaskOps>> = RwLock::new(None);
static INIT_FLAGS: AtomicUsize = AtomicUsize::new(0);

const TIME_READY: usize = 1 << 0;
const ADDR_READY: usize = 1 << 1;
const TASK_READY: usize = 1 << 2;

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

pub fn set_task_ops(ops: &'static dyn BlockTaskOps) {
    *TASK_OPS.write() = Some(ops);
    INIT_FLAGS.fetch_or(TASK_READY, Ordering::AcqRel);
}

pub fn current_task_id() -> Option<u64> {
    TASK_OPS
        .read()
        .as_ref()
        .and_then(|ops| ops.current_task_id())
}

pub fn task_yield() {
    if let Some(ops) = TASK_OPS.read().as_ref() {
        ops.task_yield();
    }
}

pub fn task_wait() {
    if let Some(ops) = TASK_OPS.read().as_ref() {
        ops.task_wait();
    }
}

pub fn task_wait_until(condition: impl Fn() -> bool) {
    if let Some(ops) = TASK_OPS.read().as_ref() {
        ops.task_wait_until(&condition);
    } else {
        while !condition() {
            core::hint::spin_loop();
        }
    }
}

pub fn wake_task(task_id: u64) {
    if let Some(ops) = TASK_OPS.read().as_ref() {
        ops.wake_task(task_id);
    }
}

pub fn notify_waiters() {
    if let Some(ops) = TASK_OPS.read().as_ref() {
        ops.notify_waiters();
    }
}

pub fn has_time_provider() -> bool {
    INIT_FLAGS.load(Ordering::Acquire) & TIME_READY != 0
}

pub fn has_address_translator() -> bool {
    INIT_FLAGS.load(Ordering::Acquire) & ADDR_READY != 0
}

pub fn has_task_ops() -> bool {
    INIT_FLAGS.load(Ordering::Acquire) & TASK_READY != 0
}
