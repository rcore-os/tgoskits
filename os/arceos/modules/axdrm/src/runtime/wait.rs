//! Task-context sleep/wakeup for DRM's per-task Linux wait predicates.

use alloc::{boxed::Box, string::String};
use core::{
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};

use ax_task::{
    runtime::task_runtime,
    sync::{
        WaitQueue,
        irq::{IrqWaitCell, IrqWorkerWaiter},
    },
    thread::current::current_thread_handle,
};
use drm_kapi::wait::WaitOps;

static WAITERS: WaitQueue = WaitQueue::new();
static IRQ_WAKE: IrqWaitCell = IrqWaitCell::new();
static WAKE_WORKER: AtomicU8 = AtomicU8::new(0);

pub(super) static OPS: WaitOps = WaitOps {
    wait_timeout_until,
    wake_all,
};

fn wait_timeout_until(timeout_ns: Option<u64>, predicate: &dyn Fn() -> bool) -> bool {
    // The native queue checks its notification generation around parking.
    // KAPI publishes `ready` before notification, so a wake between the
    // predicate check and task parking cannot disappear.
    if let Some(timeout_ns) = timeout_ns {
        WAITERS.wait_timeout_until(Duration::from_nanos(timeout_ns), predicate)
    } else {
        WAITERS.wait_until(predicate);
        false
    }
}

fn wake_all() {
    if task_runtime::in_hard_irq() {
        // A hard IRQ must not iterate or allocate ordinary task wait nodes.
        // The cell publishes one pending bit and wakes its fixed owner only.
        let _ = IRQ_WAKE.notify();
    } else {
        WAITERS.notify_all();
    }
}

fn wake_worker() {
    let owner = current_thread_handle().expect("DRM wake worker has a scheduler owner");
    let waiter = IrqWorkerWaiter::new(owner.into_wake_handle());
    loop {
        waiter
            .wait(&IRQ_WAKE)
            .expect("DRM IRQ wake owner remains live");
        WAITERS.notify_all();
    }
}

pub(super) fn init() -> Result<(), i32> {
    loop {
        match WAKE_WORKER.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                let started = super::task::spawn(String::from("drm-wake"), Box::new(wake_worker));
                WAKE_WORKER.store(if started.is_ok() { 2 } else { 0 }, Ordering::Release);
                return started;
            }
            Err(2) => return Ok(()),
            Err(1) => super::relax(),
            Err(_) => unreachable!("invalid DRM wake-worker state"),
        }
    }
}
