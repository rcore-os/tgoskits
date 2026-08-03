//! AxVM-owned CPU-bucketed VM timer wheels.

extern crate alloc;

#[cfg(test)]
use alloc::vec::Vec;
use alloc::{boxed::Box, collections::BTreeMap};
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

#[cfg(not(test))]
use crate::host::{HostTime, default_host, task};

static TOKEN: AtomicUsize = AtomicUsize::new(0);
const HOST_TIMER_PARK_DELAY: Duration = Duration::from_secs(1);

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
        (self.callback)(now);
    }
}

struct TimerWheels {
    wheels: BTreeMap<usize, TimerList<VmTimerEvent>>,
    owners: BTreeMap<usize, usize>,
}

impl TimerWheels {
    fn new() -> Self {
        Self {
            wheels: BTreeMap::new(),
            owners: BTreeMap::new(),
        }
    }

    fn ensure_cpu(&mut self, cpu_id: usize) -> &mut TimerList<VmTimerEvent> {
        self.wheels.entry(cpu_id).or_default()
    }

    fn register(
        &mut self,
        owner_cpu: usize,
        token: usize,
        deadline: TimeValue,
        event: VmTimerEvent,
    ) -> Option<TimeValue> {
        self.owners.insert(token, owner_cpu);
        let wheel = self.ensure_cpu(owner_cpu);
        wheel.set(deadline, event);
        wheel.next_deadline()
    }

    fn cancel(&mut self, token: usize) -> Option<(usize, Option<TimeValue>)> {
        let owner_cpu = self.owners.remove(&token)?;
        let next_deadline = self.wheels.get_mut(&owner_cpu).map(|wheel| {
            wheel.cancel(|event| event.token == token);
            wheel.next_deadline()
        });
        Some((owner_cpu, next_deadline.flatten()))
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
    let token = TOKEN.fetch_add(1, Ordering::Relaxed);
    let next_deadline = with_current_timer_wheels(|cpu_id, timer_wheels| {
        timer_wheels.register(
            cpu_id,
            token,
            TimeValue::from_nanos(deadline_ns),
            VmTimerEvent::new(token, callback),
        )
    });
    rearm_host_timer(next_deadline);
    token
}

pub(crate) fn cancel_timer(token: usize) {
    let _guard = NoPreempt::new();
    let current_cpu = current_cpu_id();
    let canceled = with_timer_wheels(|timer_wheels| timer_wheels.cancel(token));
    if let Some((owner_cpu, next_deadline)) = canceled {
        rearm_owner_host_timer(owner_cpu, current_cpu, next_deadline);
    }
}

pub(crate) fn check_events() {
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
            rearm_host_timer(next_deadline);
            break;
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

fn rearm_owner_host_timer(owner_cpu: usize, current_cpu: usize, next_deadline: Option<TimeValue>) {
    if owner_cpu == current_cpu {
        rearm_host_timer(next_deadline);
    } else {
        rearm_remote_owner_host_timer(owner_cpu);
    }
}

fn rearm_current_host_timer_from_wheel() {
    let next_deadline =
        with_current_timer_wheels(|cpu_id, timer_wheels| timer_wheels.next_deadline(cpu_id));
    rearm_host_timer(next_deadline);
}

#[cfg(not(test))]
unsafe fn rearm_current_host_timer_from_wheel_thunk(_arg: *mut ()) {
    rearm_current_host_timer_from_wheel();
}

#[cfg(not(test))]
fn rearm_remote_owner_host_timer(owner_cpu: usize) {
    let result = task::run_on_cpu_sync(
        owner_cpu,
        rearm_current_host_timer_from_wheel_thunk,
        core::ptr::null_mut(),
    );
    if let Err(error) = result {
        warn!("failed to rearm AxVM timer on owner CPU {owner_cpu}: {error:?}; sending IPI");
        task::send_ipi(owner_cpu);
    }
}

#[cfg(not(test))]
fn rearm_host_timer(next_deadline: Option<TimeValue>) {
    let deadline = next_deadline.unwrap_or_else(|| {
        // The host timer API has no cancel hook. Park the comparator in the
        // future so an empty wheel overwrites any stale canceled deadline.
        parked_host_timer_deadline(default_host().monotonic_time())
    });
    default_host().set_oneshot_timer(deadline.as_nanos() as u64);
}

fn parked_host_timer_deadline(now: TimeValue) -> TimeValue {
    now.saturating_add(HOST_TIMER_PARK_DELAY)
}

pub(crate) fn init_percpu() {
    info!("Initializing AxVM timer wheel...");
    with_current_timer_wheels(|cpu_id, timer_wheels| {
        timer_wheels.ensure_cpu(cpu_id);
    });
    crate::arch::register_timer_callback();
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
static TEST_REARMS: Mutex<Vec<(usize, Option<TimeValue>)>> = Mutex::new(Vec::new());
#[cfg(test)]
static TEST_REMOTE_REARMS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
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
fn rearm_host_timer(next_deadline: Option<TimeValue>) {
    let deadline = next_deadline.or_else(|| {
        Some(parked_host_timer_deadline(TimeValue::from_nanos(
            TEST_NOW_NS.load(Ordering::Acquire),
        )))
    });
    lock_test_mutex(&TEST_REARMS).push((current_cpu_id(), deadline));
}

#[cfg(test)]
fn rearm_remote_owner_host_timer(owner_cpu: usize) {
    lock_test_mutex(&TEST_REMOTE_REARMS).push(owner_cpu);
    let previous_cpu = TEST_CURRENT_CPU.swap(owner_cpu, Ordering::AcqRel);
    rearm_current_host_timer_from_wheel();
    TEST_CURRENT_CPU.store(previous_cpu, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset_global_timer_state() {
        with_timer_wheels(|timer_wheels| *timer_wheels = TimerWheels::new());
        lock_test_mutex(&TEST_REARMS).clear();
        lock_test_mutex(&TEST_REMOTE_REARMS).clear();
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
    fn host_timer_callback_path_dispatches_registered_event_once() {
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

        check_events();
        assert_eq!(TEST_CALLBACK_NOW_NS.load(Ordering::Acquire), 0);
        assert_eq!(
            lock_test_mutex(&TEST_REARMS).last().copied(),
            Some((0, Some(Duration::from_nanos(10_000_000))))
        );

        TEST_NOW_NS.store(10_000_000, Ordering::Release);
        check_events();
        assert_eq!(TEST_CALLBACK_NOW_NS.load(Ordering::Acquire), 10_000_000);
        assert_eq!(
            with_timer_wheels(|timer_wheels| timer_wheels.cancel(token)),
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

        assert_eq!(timer_wheels.cancel(7), Some((0, None)));
        assert_eq!(timer_wheels.next_deadline(0), None);
        assert_eq!(timer_wheels.cancel(7), None);
    }

    #[test]
    fn cancel_rearms_to_remaining_owner_deadline() {
        let mut timer_wheels = TimerWheels::new();
        let early = Duration::from_secs(10);
        let late = Duration::from_secs(20);

        timer_wheels.register(1, 11, early, event(11));
        timer_wheels.register(1, 12, late, event(12));

        assert_eq!(timer_wheels.cancel(11), Some((1, Some(late))));
        assert_eq!(timer_wheels.next_deadline(1), Some(late));
    }

    #[test]
    fn migration_reprogramming_deletes_stale_original_cpu_deadline() {
        let mut timer_wheels = TimerWheels::new();
        let stale_deadline = Duration::from_secs(60);
        let migrated_deadline = Duration::from_millis(10);

        assert_eq!(
            timer_wheels.register(0, 31, stale_deadline, event(31)),
            Some(stale_deadline)
        );
        assert_eq!(timer_wheels.cancel(31), Some((0, None)));
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
        assert_eq!(timer_wheels.cancel(32), None);
    }

    #[test]
    fn expiring_event_forgets_owner_token() {
        let mut timer_wheels = TimerWheels::new();
        let deadline = Duration::from_millis(5);

        timer_wheels.register(2, 21, deadline, event(21));
        let expired = timer_wheels.expire_one(2, deadline);

        assert!(expired.is_some());
        assert_eq!(timer_wheels.cancel(21), None);
    }

    #[test]
    fn remote_cancel_reprograms_owner_cpu_timer() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_global_timer_state();

        set_current_cpu_for_test(0);
        let early_token = register_timer(10_000_000, Box::new(|_| {}));
        let late_token = register_timer(20_000_000, Box::new(|_| {}));
        assert_eq!(lock_test_mutex(&TEST_REARMS).len(), 2);

        lock_test_mutex(&TEST_REARMS).clear();
        set_current_cpu_for_test(1);
        cancel_timer(early_token);

        assert_eq!(lock_test_mutex(&TEST_REMOTE_REARMS).as_slice(), &[0]);
        assert_eq!(
            lock_test_mutex(&TEST_REARMS).as_slice(),
            &[(0, Some(Duration::from_nanos(20_000_000)))]
        );

        lock_test_mutex(&TEST_REARMS).clear();
        cancel_timer(late_token);

        assert_eq!(lock_test_mutex(&TEST_REMOTE_REARMS).as_slice(), &[0, 0]);
        assert_eq!(
            lock_test_mutex(&TEST_REARMS).as_slice(),
            &[(0, Some(Duration::from_nanos(1_000_000_000)))]
        );
    }
}
