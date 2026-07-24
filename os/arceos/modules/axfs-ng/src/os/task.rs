use alloc::{boxed::Box, string::String};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ax_kspin::SpinRwLock as RwLock;

pub trait BlockTaskOps: Send + Sync {
    fn current_task_id(&self) -> Option<u64>;
    fn can_block(&self) -> bool {
        self.current_task_id().is_some()
    }
    fn task_yield(&self);
    fn task_wait(&self);
    fn task_wait_timeout(&self, dur: Duration) -> bool {
        let _ = dur;
        self.task_yield();
        true
    }
    fn task_wait_until(&self, condition: &dyn Fn() -> bool) {
        while !condition() {
            self.task_wait();
        }
    }
    fn wake_task(&self, task_id: u64);
    fn notify_waiters(&self) {}
    fn notify_drain(&self) {
        self.notify_waiters();
    }
    fn notify_drain_from_irq(&self) {
        self.notify_drain();
    }
    fn wait_for_drain_notification(&self) {
        self.task_wait();
    }
    fn wait_for_drain_notification_timeout(&self, dur: Duration) -> bool {
        let _ = dur;
        self.wait_for_drain_notification();
        true
    }
    fn spawn(&self, _name: String, _f: Box<dyn FnOnce() + Send + 'static>) {}
}

static TASK_OPS: RwLock<Option<&'static dyn BlockTaskOps>> = RwLock::new(None);
static TASK_READY: AtomicBool = AtomicBool::new(false);

pub fn set_task_ops(ops: &'static dyn BlockTaskOps) {
    *TASK_OPS.write() = Some(ops);
    TASK_READY.store(true, Ordering::Release);
}

fn task_ops() -> Option<&'static dyn BlockTaskOps> {
    TASK_OPS.read().as_ref().copied()
}

pub fn current_task_id() -> Option<u64> {
    task_ops().and_then(|ops| ops.current_task_id())
}

pub fn task_can_block() -> bool {
    task_ops().is_some_and(|ops| ops.can_block())
}

pub fn task_yield() {
    if let Some(ops) = task_ops() {
        ops.task_yield();
    }
}

pub fn task_wait() {
    if let Some(ops) = task_ops() {
        ops.task_wait();
    }
}

pub fn task_wait_timeout(dur: Duration) -> bool {
    task_ops().is_some_and(|ops| ops.task_wait_timeout(dur))
}

pub fn task_wait_until(condition: impl Fn() -> bool) {
    if let Some(ops) = task_ops() {
        ops.task_wait_until(&condition);
    } else {
        while !condition() {
            core::hint::spin_loop();
        }
    }
}

pub fn wake_task(task_id: u64) {
    if let Some(ops) = task_ops() {
        ops.wake_task(task_id);
    }
}

pub fn notify_waiters() {
    if let Some(ops) = task_ops() {
        ops.notify_waiters();
    }
}

pub fn notify_drain() {
    if let Some(ops) = task_ops() {
        ops.notify_drain();
    }
}

pub fn notify_drain_from_irq() {
    if let Some(ops) = task_ops() {
        ops.notify_drain_from_irq();
    }
}

pub fn wait_for_drain_notification() {
    if let Some(ops) = task_ops() {
        ops.wait_for_drain_notification();
    } else {
        core::hint::spin_loop();
    }
}

pub fn wait_for_drain_notification_timeout(dur: Duration) -> bool {
    task_ops().is_some_and(|ops| ops.wait_for_drain_notification_timeout(dur))
}

pub fn spawn_task(name: String, f: Box<dyn FnOnce() + Send + 'static>) {
    if let Some(ops) = task_ops() {
        ops.spawn(name, f);
    }
}

pub fn has_task_ops() -> bool {
    TASK_READY.load(Ordering::Acquire)
}
