use alloc::{boxed::Box, string::String};
use core::sync::atomic::{AtomicBool, Ordering};

use spin::RwLock;

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
    fn spawn(&self, _name: String, _f: Box<dyn FnOnce() + Send + 'static>) {}
}

static TASK_OPS: RwLock<Option<&'static dyn BlockTaskOps>> = RwLock::new(None);
static TASK_READY: AtomicBool = AtomicBool::new(false);

pub fn set_task_ops(ops: &'static dyn BlockTaskOps) {
    *TASK_OPS.write() = Some(ops);
    TASK_READY.store(true, Ordering::Release);
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

pub fn spawn_task(name: String, f: Box<dyn FnOnce() + Send + 'static>) {
    if let Some(ops) = TASK_OPS.read().as_ref() {
        ops.spawn(name, f);
    }
}

pub fn has_task_ops() -> bool {
    TASK_READY.load(Ordering::Acquire)
}
