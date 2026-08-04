//! AxVM-owned CPU-bucketed VM timer wheels.

extern crate alloc;

use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
#[cfg(test)]
use core::sync::atomic::AtomicU64;
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
#[cfg(test)]
use std::sync::{Mutex, MutexGuard};

use ax_kernel_guard::NoPreempt;
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_timer_list::{TimeValue, TimerEvent, TimerList};

use crate::host::task::IrqNotification;
#[cfg(not(test))]
use crate::host::{HostTime, default_host};

static TOKEN: AtomicUsize = AtomicUsize::new(0);
const TIMER_WORKER_STACK_SIZE: usize = 0x20_000;

/// Owner-aware handle for one AxVM timer-wheel entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VmTimerHandle {
    token: usize,
    owner_cpu: usize,
}

struct VmTimerEvent {
    token: usize,
    callback: Box<dyn FnOnce(TimeValue) + Send + 'static>,
}

impl VmTimerEvent {
    fn new<F>(token: usize, callback: F) -> Self
    where
        F: FnOnce(TimeValue) + Send + 'static,
    {
        Self {
            token,
            callback: Box::new(callback),
        }
    }
}

impl TimerEvent for VmTimerEvent {
    fn callback(self, now: TimeValue) {
        trace!("handle VM timer event token {}", self.token);
        (self.callback)(now);
    }
}

struct TimerWheels {
    wheels: BTreeMap<usize, TimerList<VmTimerEvent>>,
    owners: BTreeMap<usize, usize>,
    notifications: BTreeMap<usize, Arc<IrqNotification>>,
    workers_started: BTreeSet<usize>,
}

impl TimerWheels {
    fn new() -> Self {
        Self {
            wheels: BTreeMap::new(),
            owners: BTreeMap::new(),
            notifications: BTreeMap::new(),
            workers_started: BTreeSet::new(),
        }
    }

    fn ensure_cpu(&mut self, cpu_id: usize) -> &mut TimerList<VmTimerEvent> {
        self.notifications
            .entry(cpu_id)
            .or_insert_with(|| Arc::new(IrqNotification::new()));
        self.wheels.entry(cpu_id).or_default()
    }

    fn notification(&mut self, cpu_id: usize) -> Arc<IrqNotification> {
        self.ensure_cpu(cpu_id);
        self.notifications
            .get(&cpu_id)
            .expect("ensured AxVM timer CPU must have a notification")
            .clone()
    }

    fn worker_started(&self, cpu_id: usize) -> bool {
        self.workers_started.contains(&cpu_id)
    }

    fn mark_worker_started(&mut self, cpu_id: usize) {
        self.workers_started.insert(cpu_id);
    }

    fn register(
        &mut self,
        owner_cpu: usize,
        token: usize,
        deadline: TimeValue,
        event: VmTimerEvent,
    ) -> Option<TimeValue> {
        self.owners.insert(token, owner_cpu);
        self.ensure_cpu(owner_cpu).set(deadline, event);
        self.next_deadline(owner_cpu)
    }

    fn handle(&self, token: usize) -> Option<VmTimerHandle> {
        self.owners
            .get(&token)
            .copied()
            .map(|owner_cpu| VmTimerHandle { token, owner_cpu })
    }

    fn cancel_handle(&mut self, handle: VmTimerHandle) -> Option<Option<TimeValue>> {
        if self.owners.get(&handle.token).copied() != Some(handle.owner_cpu) {
            return None;
        }
        self.owners.remove(&handle.token);
        let wheel = self.wheels.get_mut(&handle.owner_cpu)?;
        wheel.cancel(|event| event.token == handle.token);
        Some(wheel.next_deadline())
    }

    fn expire_one(
        &mut self,
        owner_cpu: usize,
        now: TimeValue,
    ) -> Option<(TimeValue, VmTimerEvent)> {
        let expired = self
            .wheels
            .get_mut(&owner_cpu)
            .and_then(|wheel| wheel.expire_one(now));
        if let Some((_, event)) = &expired {
            self.owners.remove(&event.token);
        }
        expired
    }

    fn next_deadline(&self, owner_cpu: usize) -> Option<TimeValue> {
        self.wheels
            .get(&owner_cpu)
            .and_then(TimerList::next_deadline)
    }
}

static TIMER_WHEELS: LazyInit<SpinNoIrq<TimerWheels>> = LazyInit::new();

pub(crate) fn register_timer(
    deadline_ns: u64,
    callback: Box<dyn FnOnce(Duration) + Send + 'static>,
) -> usize {
    register_timer_handle(deadline_ns, callback).token
}

pub(crate) fn register_timer_handle(
    deadline_ns: u64,
    callback: Box<dyn FnOnce(Duration) + Send + 'static>,
) -> VmTimerHandle {
    let token = TOKEN.fetch_add(1, Ordering::Relaxed);
    let (owner_cpu, notification) = with_current_timer_wheels(|cpu_id, timer_wheels| {
        timer_wheels.register(
            cpu_id,
            token,
            TimeValue::from_nanos(deadline_ns),
            VmTimerEvent::new(token, callback),
        );
        (cpu_id, timer_wheels.notification(cpu_id))
    });
    notify_timer_worker(&notification);
    VmTimerHandle { token, owner_cpu }
}

pub(crate) fn cancel_timer_handle(handle: VmTimerHandle) {
    let _guard = NoPreempt::new();
    let notification = with_timer_wheels(|timer_wheels| {
        timer_wheels
            .cancel_handle(handle)
            .map(|_| timer_wheels.notification(handle.owner_cpu))
    });
    if let Some(notification) = notification {
        notify_timer_worker(&notification);
    }
}

pub(crate) fn cancel_timer(token: usize) {
    let handle = {
        let _guard = NoPreempt::new();
        with_timer_wheels(|timer_wheels| timer_wheels.handle(token))
    };
    if let Some(handle) = handle {
        cancel_timer_handle(handle);
    }
}

fn check_events() -> Option<TimeValue> {
    loop {
        let now = current_host_time();
        let (expired, next_deadline) = with_current_timer_wheels(|cpu_id, timer_wheels| {
            let expired = timer_wheels.expire_one(cpu_id, now);
            let next_deadline = if expired.is_none() {
                timer_wheels.next_deadline(cpu_id)
            } else {
                None
            };
            (expired, next_deadline)
        });
        if let Some((deadline, event)) = expired {
            trace!("handle VM timer event scheduled at {deadline:#?}");
            event.callback(now);
        } else {
            return next_deadline;
        }
    }
}

#[cfg(not(test))]
fn current_host_time() -> TimeValue {
    default_host().monotonic_time()
}

#[cfg(test)]
fn current_host_time() -> TimeValue {
    TimeValue::from_nanos(TEST_NOW_NS.load(Ordering::Acquire))
}

fn timer_worker(notification: Arc<IrqNotification>) -> ! {
    loop {
        let next_deadline = check_events();
        notification.wait_until(next_deadline);
    }
}

#[cfg(not(test))]
fn notify_timer_worker(notification: &IrqNotification) {
    notification.notify_from_task();
}

#[cfg(test)]
fn notify_timer_worker(_notification: &IrqNotification) {}

pub(crate) fn init_percpu() -> crate::AxVmResult {
    info!("Initializing AxVM timer wheel...");
    let cpu_id = current_cpu_id();
    let (notification, already_started) = with_current_timer_wheels(|cpu_id, timer_wheels| {
        (
            timer_wheels.notification(cpu_id),
            timer_wheels.worker_started(cpu_id),
        )
    });
    if already_started {
        return Ok(());
    }

    let worker_notification = Arc::clone(&notification);
    let affinity = crate::host::task::cpu_set_one(cpu_id);
    let worker = unsafe {
        // SAFETY: the per-CPU worker carries no OS extension. Its single-CPU
        // affinity is installed before scheduler publication, and its owned
        // notification is moved exactly once into a permanent entry point.
        crate::host::task::spawn_thread_with_extension_and_affinity(
            move || timer_worker(worker_notification),
            alloc::format!("axvm-timer-{cpu_id}"),
            TIMER_WORKER_STACK_SIZE,
            None,
            Some(affinity),
        )
    }
    .map_err(|error| crate::AxVmError::host("start per-CPU AxVM timer worker", error))?;
    debug!(
        "AxVM timer worker {} started on CPU {cpu_id}",
        worker.id().as_u64()
    );
    with_current_timer_wheels(|cpu_id, timer_wheels| timer_wheels.mark_worker_started(cpu_id));
    drop(worker);
    Ok(())
}

fn with_timer_wheels<R>(operation: impl FnOnce(&mut TimerWheels) -> R) -> R {
    let timer_wheels = TIMER_WHEELS.get_or_init(|| SpinNoIrq::new(TimerWheels::new()));
    operation(&mut timer_wheels.lock())
}

fn with_current_timer_wheels<R>(operation: impl FnOnce(usize, &mut TimerWheels) -> R) -> R {
    let _guard = NoPreempt::new();
    let cpu_id = current_cpu_id();
    with_timer_wheels(|timer_wheels| operation(cpu_id, timer_wheels))
}

#[cfg(not(test))]
fn current_cpu_id() -> usize {
    use crate::host::HostCpu;

    default_host().this_cpu_id()
}

#[cfg(test)]
static TEST_CURRENT_CPU: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static TEST_NOW_NS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
fn current_cpu_id() -> usize {
    TEST_CURRENT_CPU.load(Ordering::Acquire)
}

#[cfg(test)]
fn lock_test_mutex<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("AxVM timer test mutex poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset_global_timer_state() {
        with_timer_wheels(|timer_wheels| *timer_wheels = TimerWheels::new());
        TEST_CURRENT_CPU.store(0, Ordering::Release);
        TEST_NOW_NS.store(0, Ordering::Release);
    }

    fn set_current_cpu_for_test(cpu_id: usize) {
        TEST_CURRENT_CPU.store(cpu_id, Ordering::Release);
    }

    static TEST_CALLBACK_NOW_NS: AtomicU64 = AtomicU64::new(0);

    fn event(token: usize) -> VmTimerEvent {
        VmTimerEvent::new(token, |_| {})
    }

    #[test]
    fn worker_dispatches_registered_event_once_at_its_deadline() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_global_timer_state();
        TEST_CALLBACK_NOW_NS.store(0, Ordering::Release);

        set_current_cpu_for_test(0);
        TEST_NOW_NS.store(1_000_000, Ordering::Release);
        let token = register_timer(
            10_000_000,
            Box::new(|now| {
                TEST_CALLBACK_NOW_NS.store(now.as_nanos() as u64, Ordering::Release);
            }),
        );

        assert_eq!(check_events(), Some(Duration::from_nanos(10_000_000)));
        assert_eq!(TEST_CALLBACK_NOW_NS.load(Ordering::Acquire), 0);

        TEST_NOW_NS.store(10_000_000, Ordering::Release);
        assert_eq!(check_events(), None);
        assert_eq!(TEST_CALLBACK_NOW_NS.load(Ordering::Acquire), 10_000_000);
        assert_eq!(
            with_timer_wheels(|timer_wheels| timer_wheels.handle(token)),
            None
        );
    }

    #[test]
    fn cancel_removes_event_from_original_cpu_wheel() {
        let mut timer_wheels = TimerWheels::new();
        let deadline = Duration::from_secs(60);

        assert_eq!(
            timer_wheels.register(0, 7, deadline, event(7)),
            Some(deadline)
        );
        assert_eq!(timer_wheels.next_deadline(0), Some(deadline));
        assert_eq!(timer_wheels.next_deadline(1), None);

        assert_eq!(
            timer_wheels.cancel_handle(VmTimerHandle {
                token: 7,
                owner_cpu: 0,
            }),
            Some(None)
        );
        assert_eq!(timer_wheels.next_deadline(0), None);
        assert_eq!(timer_wheels.handle(7), None);
    }

    #[test]
    fn cancel_exposes_remaining_owner_deadline() {
        let mut timer_wheels = TimerWheels::new();
        let early = Duration::from_secs(10);
        let late = Duration::from_secs(20);

        timer_wheels.register(1, 11, early, event(11));
        timer_wheels.register(1, 12, late, event(12));

        assert_eq!(
            timer_wheels.cancel_handle(VmTimerHandle {
                token: 11,
                owner_cpu: 1,
            }),
            Some(Some(late))
        );
        assert_eq!(timer_wheels.next_deadline(1), Some(late));
    }

    #[test]
    fn migration_deletes_stale_original_cpu_deadline() {
        let mut timer_wheels = TimerWheels::new();
        let stale_deadline = Duration::from_secs(60);
        let migrated_deadline = Duration::from_millis(10);

        assert_eq!(
            timer_wheels.register(0, 31, stale_deadline, event(31)),
            Some(stale_deadline)
        );
        assert_eq!(
            timer_wheels.cancel_handle(VmTimerHandle {
                token: 31,
                owner_cpu: 0,
            }),
            Some(None)
        );
        assert_eq!(
            timer_wheels.register(1, 32, migrated_deadline, event(32)),
            Some(migrated_deadline)
        );

        assert!(timer_wheels.expire_one(0, stale_deadline).is_none());
        let (deadline, migrated_event) = timer_wheels
            .expire_one(1, migrated_deadline)
            .expect("migrated timer event should expire on the new owner CPU");
        assert_eq!(deadline, migrated_deadline);
        assert_eq!(migrated_event.token, 32);
        assert_eq!(timer_wheels.handle(32), None);
    }

    #[test]
    fn expiring_event_forgets_owner_token() {
        let mut timer_wheels = TimerWheels::new();
        let deadline = Duration::from_millis(5);

        timer_wheels.register(2, 21, deadline, event(21));
        let expired = timer_wheels.expire_one(2, deadline);

        assert!(expired.is_some());
        assert_eq!(timer_wheels.handle(21), None);
    }

    #[test]
    fn worker_deadline_snapshot_tracks_registration_cancellation_and_expiry() {
        let mut timer_wheels = TimerWheels::new();
        let early = Duration::from_millis(5);
        let late = Duration::from_millis(10);

        timer_wheels.register(0, 51, early, event(51));
        timer_wheels.register(0, 52, late, event(52));
        assert_eq!(timer_wheels.next_deadline(0), Some(early));

        timer_wheels.cancel_handle(VmTimerHandle {
            token: 51,
            owner_cpu: 0,
        });
        assert_eq!(timer_wheels.next_deadline(0), Some(late));

        timer_wheels.expire_one(0, late);
        assert_eq!(timer_wheels.next_deadline(0), None);
    }

    #[test]
    fn remote_cancel_updates_the_owner_cpu_wheel() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_global_timer_state();

        set_current_cpu_for_test(0);
        let early_token = register_timer(10_000_000, Box::new(|_| {}));
        let late_token = register_timer(20_000_000, Box::new(|_| {}));

        set_current_cpu_for_test(1);
        cancel_timer(early_token);

        assert_eq!(
            with_timer_wheels(|timer_wheels| timer_wheels.next_deadline(0)),
            Some(Duration::from_nanos(20_000_000))
        );

        cancel_timer(late_token);
        assert_eq!(
            with_timer_wheels(|timer_wheels| timer_wheels.next_deadline(0)),
            None
        );
    }

    #[test]
    fn owner_aware_handle_rejects_a_stale_cpu_identity() {
        let mut timer_wheels = TimerWheels::new();
        let deadline = Duration::from_secs(1);
        timer_wheels.register(2, 41, deadline, event(41));

        assert_eq!(
            timer_wheels.cancel_handle(VmTimerHandle {
                token: 41,
                owner_cpu: 1,
            }),
            None
        );
        assert_eq!(timer_wheels.next_deadline(2), Some(deadline));
        assert_eq!(
            timer_wheels.cancel_handle(VmTimerHandle {
                token: 41,
                owner_cpu: 2,
            }),
            Some(None)
        );
    }

    #[test]
    fn remote_handle_cancel_uses_the_recorded_owner_cpu() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_global_timer_state();

        set_current_cpu_for_test(2);
        let handle = register_timer_handle(20_000_000, Box::new(|_| {}));

        set_current_cpu_for_test(0);
        cancel_timer_handle(handle);

        assert_eq!(
            with_timer_wheels(|timer_wheels| timer_wheels.next_deadline(2)),
            None
        );
    }
}
