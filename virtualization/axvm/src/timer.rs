//! AxVM-owned CPU-bucketed VM timer wheels.

extern crate alloc;

use alloc::{boxed::Box, collections::BTreeMap};
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use ax_kernel_guard::NoPreempt;
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_timer_list::{TimeValue, TimerEvent, TimerList};

use crate::host::{HostCpu, HostTime, default_host};

static TOKEN: AtomicUsize = AtomicUsize::new(0);

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
    let current_cpu = default_host().this_cpu_id();
    let canceled = with_timer_wheels(|timer_wheels| timer_wheels.cancel(token));
    if let Some((owner_cpu, next_deadline)) = canceled
        && owner_cpu == current_cpu
    {
        rearm_host_timer(next_deadline);
    }
}

pub(crate) fn check_events() {
    loop {
        let now = default_host().monotonic_time();
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

fn rearm_host_timer(next_deadline: Option<TimeValue>) {
    if let Some(deadline) = next_deadline {
        default_host().set_oneshot_timer(deadline.as_nanos() as u64);
    }
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
    let cpu_id = default_host().this_cpu_id();
    with_timer_wheels(|timer_wheels| operation(cpu_id, timer_wheels))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(token: usize) -> VmTimerEvent {
        VmTimerEvent::new(token, |_| {})
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
}
