//! AxVM-owned per-CPU VM timer queues.

#[cfg(test)]
use std::sync::{MutexGuard, atomic::AtomicU64};
use std::{
    boxed::Box,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
    vec::Vec,
};

use ax_std::os::arceos::sync::IrqSafeMutex;

#[cfg(not(test))]
use crate::host::{HostCpu, HostTime, default_host};
use crate::{
    ThreadHandle,
    host::task::{IrqNotification, MonotonicDeadline},
    sync::MutexExt,
};

type TimeValue = Duration;

static TOKEN: AtomicUsize = AtomicUsize::new(0);
const TIMER_WORKER_STACK_SIZE: usize = 0x20_000;
#[cfg(test)]
const TEST_TIMER_CPU_COUNT: usize = 128;

/// Owner-aware handle for one AxVM timer entry.
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
    fn new(token: usize, callback: Box<dyn FnOnce(TimeValue) + Send + 'static>) -> Self {
        Self { token, callback }
    }

    fn callback(self, now: TimeValue) {
        trace!("handle VM timer event token {}", self.token);
        (self.callback)(now);
    }
}

/// One preallocated timer entry linked into its owner CPU's queue.
///
/// The box is allocated before taking the IRQ-safe queue lock. Queue mutation
/// therefore only relinks existing nodes, matching Linux hrtimer's rule that
/// timer-base locking never performs a sleepable allocation.
struct TimerEntry {
    deadline: TimeValue,
    event: VmTimerEvent,
    next: Option<Box<TimerEntry>>,
}

struct TimerQueue {
    head: Option<Box<TimerEntry>>,
}

impl TimerQueue {
    const fn new() -> Self {
        Self { head: None }
    }

    fn insert(&mut self, entry: Box<TimerEntry>) {
        Self::insert_at(&mut self.head, entry);
    }

    fn insert_at(link: &mut Option<Box<TimerEntry>>, mut entry: Box<TimerEntry>) {
        if link
            .as_ref()
            .is_some_and(|current| current.deadline <= entry.deadline)
        {
            Self::insert_at(
                &mut link
                    .as_mut()
                    .expect("checked timer entry must remain present")
                    .next,
                entry,
            );
            return;
        }
        entry.next = link.take();
        *link = Some(entry);
    }

    fn cancel(&mut self, token: usize) -> Option<Box<TimerEntry>> {
        Self::cancel_at(&mut self.head, token)
    }

    fn cancel_at(link: &mut Option<Box<TimerEntry>>, token: usize) -> Option<Box<TimerEntry>> {
        let current = link.as_ref()?;
        if current.event.token == token {
            let mut removed = link
                .take()
                .expect("checked timer entry must remain present");
            *link = removed.next.take();
            return Some(removed);
        }
        Self::cancel_at(
            &mut link
                .as_mut()
                .expect("checked timer entry must remain present")
                .next,
            token,
        )
    }

    fn expire_one(&mut self, now: TimeValue) -> Option<Box<TimerEntry>> {
        if self.head.as_ref()?.deadline > now {
            return None;
        }
        let mut expired = self
            .head
            .take()
            .expect("checked timer entry must remain present");
        self.head = expired.next.take();
        Some(expired)
    }

    fn next_deadline(&self) -> Option<TimeValue> {
        self.head.as_ref().map(|entry| entry.deadline)
    }
}

struct TimerCpu {
    queue: IrqSafeMutex<TimerQueue>,
    notification: IrqNotification,
    worker: OnceLock<ThreadHandle>,
}

impl TimerCpu {
    const fn new() -> Self {
        Self {
            queue: IrqSafeMutex::new(TimerQueue::new()),
            notification: IrqNotification::new(),
            worker: OnceLock::new(),
        }
    }

    fn register(&self, deadline: TimeValue, event: VmTimerEvent) -> Option<TimeValue> {
        let entry = Box::new(TimerEntry {
            deadline,
            event,
            next: None,
        });
        let mut queue = self.queue.lock();
        queue.insert(entry);
        queue.next_deadline()
    }

    fn cancel(&self, token: usize) -> bool {
        let removed = {
            let mut queue = self.queue.lock();
            queue.cancel(token)
        };
        let was_removed = removed.is_some();
        drop(removed);
        was_removed
    }

    fn expire_one(&self, now: TimeValue) -> Option<Box<TimerEntry>> {
        self.queue.lock().expire_one(now)
    }

    fn next_deadline(&self) -> Option<TimeValue> {
        self.queue.lock().next_deadline()
    }
}

struct TimerCpuSlot {
    init: Mutex<()>,
    cpu: OnceLock<Arc<TimerCpu>>,
}

impl TimerCpuSlot {
    const fn new() -> Self {
        Self {
            init: Mutex::new(()),
            cpu: OnceLock::new(),
        }
    }
}

static TIMER_CPUS: OnceLock<Box<[TimerCpuSlot]>> = OnceLock::new();

fn allocate_timer_cpu_slots(cpu_count: usize) -> Box<[TimerCpuSlot]> {
    let mut slots = Vec::with_capacity(cpu_count);
    slots.resize_with(cpu_count, TimerCpuSlot::new);
    slots.into_boxed_slice()
}

#[cfg(not(test))]
fn timer_cpu_slots() -> &'static [TimerCpuSlot] {
    TIMER_CPUS.get_or_init(|| allocate_timer_cpu_slots(default_host().cpu_count()))
}

#[cfg(test)]
fn timer_cpu_slots() -> &'static [TimerCpuSlot] {
    TIMER_CPUS.get_or_init(|| allocate_timer_cpu_slots(TEST_TIMER_CPU_COUNT))
}

fn timer_cpu_slot(cpu_id: usize) -> Option<&'static TimerCpuSlot> {
    timer_cpu_slots().get(cpu_id)
}

#[cfg(not(test))]
fn timer_cpu(cpu_id: usize) -> Arc<TimerCpu> {
    timer_cpu_slot(cpu_id)
        .and_then(|slot| slot.cpu.get())
        .cloned()
        .unwrap_or_else(|| panic!("AxVM timer CPU {cpu_id} was not initialized"))
}

#[cfg(test)]
fn timer_cpu(cpu_id: usize) -> Arc<TimerCpu> {
    timer_cpu_slot(cpu_id)
        .unwrap_or_else(|| panic!("test timer CPU {cpu_id} exceeds supported CPU IDs"))
        .cpu
        .get_or_init(|| Arc::new(TimerCpu::new()))
        .clone()
}

fn next_timer_token(owner_cpu: usize) -> usize {
    let owner_stride = timer_cpu_slots().len();
    assert!(
        owner_cpu < owner_stride,
        "AxVM timer CPU ID exceeds the encoded owner range"
    );
    let base = TOKEN
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |token| {
            token.checked_add(owner_stride)
        })
        .expect("AxVM timer identity space exhausted");
    base.checked_add(owner_cpu)
        .expect("AxVM timer identity owner encoding overflowed")
}

fn timer_token_owner(token: usize) -> usize {
    token % timer_cpu_slots().len()
}

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
    let owner_cpu = current_cpu_id();
    let token = next_timer_token(owner_cpu);
    let cpu = timer_cpu(owner_cpu);
    cpu.register(
        TimeValue::from_nanos(deadline_ns),
        VmTimerEvent::new(token, callback),
    );
    notify_timer_worker(&cpu.notification);
    VmTimerHandle { token, owner_cpu }
}

pub(crate) fn cancel_timer_handle(handle: VmTimerHandle) {
    if timer_token_owner(handle.token) != handle.owner_cpu {
        return;
    }
    let cpu = timer_cpu(handle.owner_cpu);
    if cpu.cancel(handle.token) {
        notify_timer_worker(&cpu.notification);
    }
}

pub(crate) fn cancel_timer(token: usize) {
    cancel_timer_handle(VmTimerHandle {
        token,
        owner_cpu: timer_token_owner(token),
    });
}

#[cfg(test)]
fn check_events() -> Option<TimeValue> {
    let cpu = timer_cpu(current_cpu_id());
    check_events_for(&cpu)
}

fn check_events_for(cpu: &TimerCpu) -> Option<TimeValue> {
    loop {
        let now = current_host_time();
        if let Some(expired) = cpu.expire_one(now) {
            trace!("handle VM timer event scheduled at {:#?}", expired.deadline);
            expired.event.callback(now);
        } else {
            return cpu.next_deadline();
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

fn timer_worker(cpu: Arc<TimerCpu>) -> ! {
    loop {
        let next_deadline = check_events_for(&cpu).map(MonotonicDeadline::from_duration);
        cpu.notification.wait_until(next_deadline);
    }
}

#[cfg(not(test))]
fn notify_timer_worker(notification: &IrqNotification) {
    // Guest-exit timer programming is non-preemptible but still belongs to a
    // scheduler task, never to hard IRQ context. Use the task wake edge so a
    // same-CPU worker becomes runnable without waiting for an unrelated IRQ
    // return; the worker performs callbacks after the queue lock is released.
    notification.notify_from_task();
}

#[cfg(test)]
fn notify_timer_worker(_notification: &IrqNotification) {}

pub(crate) fn init_percpu() -> crate::AxVmResult {
    info!("Initializing AxVM timer queue...");
    let cpu_id = current_cpu_id();
    let cpu_count = timer_cpu_slots().len();
    let slot = timer_cpu_slot(cpu_id).ok_or_else(|| {
        crate::AxVmError::invalid_input(
            "initialize AxVM timer CPU",
            std::format!("host CPU ID {cpu_id} exceeds {cpu_count} timer queues"),
        )
    })?;
    let _init = slot.init.lock_unpoisoned();
    if slot.cpu.get().is_some() {
        return Ok(());
    }

    let cpu = Arc::new(TimerCpu::new());
    let worker_cpu = Arc::clone(&cpu);
    let affinity = crate::host::task::cpu_set_one(cpu_id);
    let worker = unsafe {
        // SAFETY: the per-CPU worker carries no OS extension. Its single-CPU
        // affinity is installed before scheduler publication, and its owned
        // timer CPU reference is moved exactly once into a permanent entry.
        crate::host::task::spawn_thread_with_extension_and_affinity(
            move || timer_worker(worker_cpu),
            std::format!("axvm-timer-{cpu_id}"),
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
    assert!(
        cpu.worker.set(worker).is_ok(),
        "new AxVM timer CPU unexpectedly already owns a worker"
    );
    assert!(
        slot.cpu.set(cpu).is_ok(),
        "serialized AxVM timer CPU initialization raced"
    );
    Ok(())
}

#[cfg(test)]
static TEST_CURRENT_CPU: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static TEST_NOW_NS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
fn current_cpu_id() -> usize {
    TEST_CURRENT_CPU.load(Ordering::Acquire)
}

#[cfg(not(test))]
fn current_cpu_id() -> usize {
    use crate::host::HostCpu;

    default_host().this_cpu_id()
}

#[cfg(test)]
fn lock_test_mutex<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("AxVM timer test mutex poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());
    static TEST_CALLBACK_NOW_NS: AtomicU64 = AtomicU64::new(0);

    fn reset_timer_state() {
        for slot in timer_cpu_slots() {
            if let Some(cpu) = slot.cpu.get() {
                *cpu.queue.lock() = TimerQueue::new();
            }
        }
        TOKEN.store(0, Ordering::Release);
        TEST_CURRENT_CPU.store(0, Ordering::Release);
        TEST_NOW_NS.store(0, Ordering::Release);
    }

    fn set_current_cpu_for_test(cpu_id: usize) {
        TEST_CURRENT_CPU.store(cpu_id, Ordering::Release);
    }

    fn event(token: usize) -> VmTimerEvent {
        VmTimerEvent::new(token, Box::new(|_| {}))
    }

    fn entry(token: usize, deadline: TimeValue) -> Box<TimerEntry> {
        Box::new(TimerEntry {
            deadline,
            event: event(token),
            next: None,
        })
    }

    #[test]
    fn queue_orders_preallocated_entries_without_global_state() {
        let mut queue = TimerQueue::new();
        let early = Duration::from_millis(5);
        let late = Duration::from_millis(10);

        queue.insert(entry(2, late));
        queue.insert(entry(1, early));

        assert_eq!(queue.next_deadline(), Some(early));
        assert_eq!(queue.expire_one(early).unwrap().event.token, 1);
        assert_eq!(queue.next_deadline(), Some(late));
    }

    #[test]
    fn worker_dispatches_registered_event_once_at_its_deadline() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_timer_state();
        TEST_CALLBACK_NOW_NS.store(0, Ordering::Release);

        set_current_cpu_for_test(0);
        TEST_NOW_NS.store(1_000_000, Ordering::Release);
        register_timer(
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
    }

    #[test]
    fn remote_cancel_updates_only_the_owner_cpu_queue() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_timer_state();

        set_current_cpu_for_test(0);
        let early_token = register_timer(10_000_000, Box::new(|_| {}));
        let late_token = register_timer(20_000_000, Box::new(|_| {}));

        set_current_cpu_for_test(1);
        cancel_timer(early_token);

        assert_eq!(
            timer_cpu(0).next_deadline(),
            Some(Duration::from_nanos(20_000_000))
        );
        assert_eq!(timer_cpu(1).next_deadline(), None);

        cancel_timer(late_token);
        assert_eq!(timer_cpu(0).next_deadline(), None);
    }

    #[test]
    fn owner_aware_handle_rejects_a_stale_cpu_identity() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_timer_state();

        set_current_cpu_for_test(2);
        let handle = register_timer_handle(20_000_000, Box::new(|_| {}));
        let stale = VmTimerHandle {
            token: handle.token,
            owner_cpu: 1,
        };

        cancel_timer_handle(stale);
        assert!(timer_cpu(2).next_deadline().is_some());

        cancel_timer_handle(handle);
        assert_eq!(timer_cpu(2).next_deadline(), None);
    }

    #[test]
    fn timer_tokens_encode_their_owner_cpu() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_timer_state();

        set_current_cpu_for_test(3);
        let token = register_timer(10_000_000, Box::new(|_| {}));

        assert_eq!(timer_token_owner(token), 3);
    }

    #[test]
    fn timer_owner_encoding_is_not_limited_by_machine_word_bits() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_timer_state();

        let owner_cpu = usize::BITS as usize + 1;
        set_current_cpu_for_test(owner_cpu);
        let handle = register_timer_handle(10_000_000, Box::new(|_| {}));

        assert_eq!(handle.owner_cpu, owner_cpu);
        assert_eq!(timer_token_owner(handle.token), owner_cpu);
        cancel_timer_handle(handle);
        assert_eq!(timer_cpu(owner_cpu).next_deadline(), None);
    }

    #[test]
    fn timer_token_exhaustion_is_not_reused() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        TOKEN.store(usize::MAX, Ordering::Release);

        let exhausted = std::panic::catch_unwind(|| next_timer_token(0));

        TOKEN.store(0, Ordering::Release);
        assert!(exhausted.is_err(), "an exhausted timer identity was reused");
    }
}
