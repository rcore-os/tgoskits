extern crate alloc;

// Host tests must link the external lock/task provider.
use alloc::{format, sync::Arc, task::Wake, vec::Vec};
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    task::Waker,
};

use ax_runtime as _;
use axpoll::{
    ExclusiveConsumer, IoEvents, PollRegistrar, Pollable, SharedObserver, SharedRegistrationSink,
};
use axpoll_set::PollSet;

struct WakeCounter(AtomicUsize);

impl WakeCounter {
    fn new() -> Arc<Self> {
        Arc::new(Self(AtomicUsize::new(0)))
    }

    fn count(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }

    fn bump(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

impl Wake for WakeCounter {
    fn wake(self: Arc<Self>) {
        self.bump();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.bump();
    }
}

fn counter_waker(counter: &Arc<WakeCounter>) -> Waker {
    Waker::from(counter.clone())
}

#[test]
fn axpoll_event_masks_and_empty_wake_rules_hold() {
    let events = IoEvents::IN | IoEvents::OUT | IoEvents::ALWAYS_POLL;
    assert!(events.contains(IoEvents::IN));
    assert!(events.contains(IoEvents::OUT));
    assert!(events.contains(IoEvents::ERR));
    assert!(events.contains(IoEvents::HUP));
    assert!(!events.contains(IoEvents::NVAL));
    assert!(format!("{:?}", IoEvents::RDHUP).contains("RDHUP"));

    let poll_set = PollSet::default();
    assert_eq!(unsafe { poll_set.wake(IoEvents::IN) }, 0);
    assert_eq!(unsafe { poll_set.wake(IoEvents::IN) }, 0);
}

#[test]
fn axpoll_wakes_only_matching_interests() {
    let poll_set = PollSet::new();
    let read_counter = WakeCounter::new();
    let write_counter = WakeCounter::new();
    let read_waker = counter_waker(&read_counter);
    let write_waker = counter_waker(&write_counter);
    let mut read = PollRegistrar::<SharedObserver>::new(&read_waker);
    let mut write = PollRegistrar::<SharedObserver>::new(&write_waker);

    unsafe {
        read.register(&poll_set, IoEvents::IN);
        write.register(&poll_set, IoEvents::OUT);
    }

    assert_eq!(unsafe { poll_set.wake(IoEvents::IN) }, 1);
    assert_eq!(read_counter.count(), 1);
    assert_eq!(write_counter.count(), 0);

    assert_eq!(unsafe { poll_set.wake(IoEvents::OUT) }, 1);
    assert_eq!(read_counter.count(), 1);
    assert_eq!(write_counter.count(), 1);
    assert_eq!(unsafe { poll_set.wake(IoEvents::IN | IoEvents::OUT) }, 0);
}

#[test]
fn axpoll_exclusive_wake_keeps_other_matching_waiters() {
    let poll_set = PollSet::new();
    let first_counter = WakeCounter::new();
    let second_counter = WakeCounter::new();
    let first_waker = counter_waker(&first_counter);
    let second_waker = counter_waker(&second_counter);
    let mut first = PollRegistrar::<ExclusiveConsumer>::new(&first_waker);
    let mut second = PollRegistrar::<ExclusiveConsumer>::new(&second_waker);

    unsafe {
        first.register_exclusive(&poll_set, IoEvents::IN);
        second.register_exclusive(&poll_set, IoEvents::IN);
    }

    assert_eq!(unsafe { poll_set.wake(IoEvents::IN) }, 1);
    assert_eq!(first_counter.count() + second_counter.count(), 1);
    assert_eq!(unsafe { poll_set.wake(IoEvents::IN) }, 1);
    assert_eq!(first_counter.count(), 1);
    assert_eq!(second_counter.count(), 1);
    assert_eq!(unsafe { poll_set.wake(IoEvents::IN) }, 0);
}

#[test]
fn axpoll_unbounded_registration_and_drop_rules_hold() {
    let poll_set = PollSet::new();
    let counters = (0..65).map(|_| WakeCounter::new()).collect::<Vec<_>>();
    let mut registrars = Vec::new();

    for counter in &counters {
        let waker = counter_waker(counter);
        let mut registrar = PollRegistrar::<SharedObserver>::new(&waker);
        unsafe { registrar.register(&poll_set, IoEvents::IN) };
        registrars.push(registrar);
    }

    assert!(counters.iter().all(|counter| counter.count() == 0));
    assert_eq!(unsafe { poll_set.wake(IoEvents::IN) }, 65);
    assert!(counters.iter().all(|counter| counter.count() == 1));

    let poll_set = PollSet::new();
    let drop_counter = WakeCounter::new();
    let drop_waker = counter_waker(&drop_counter);
    let mut drop_registrars = Vec::new();
    for _ in 0..4 {
        let mut registrar = PollRegistrar::<SharedObserver>::new(&drop_waker);
        unsafe { registrar.register(&poll_set, IoEvents::OUT) };
        drop_registrars.push(registrar);
    }
    drop(poll_set);
    assert_eq!(drop_counter.count(), 4);
}

struct FixedPollable {
    poll_set: PollSet,
    ready: IoEvents,
}

impl FixedPollable {
    fn new(ready: IoEvents) -> Self {
        Self {
            poll_set: PollSet::new(),
            ready,
        }
    }
}

impl Pollable for FixedPollable {
    fn poll(&self) -> IoEvents {
        self.ready
    }

    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
        unsafe { sink.register_shared(&self.poll_set, events) };
    }
}

#[test]
fn axpoll_pollable_owned_registration_rules_hold() {
    let pollable = FixedPollable::new(IoEvents::IN | IoEvents::HUP);
    let counter = WakeCounter::new();
    let waker = counter_waker(&counter);
    let mut registrar = PollRegistrar::<SharedObserver>::new(&waker);

    assert!(pollable.poll().contains(IoEvents::IN));
    assert!(pollable.poll().contains(IoEvents::HUP));
    unsafe { pollable.register_shared(&mut registrar, IoEvents::IN | IoEvents::ERR) };

    assert_eq!(unsafe { pollable.poll_set.wake(IoEvents::OUT) }, 0);
    assert_eq!(counter.count(), 0);
    assert_eq!(unsafe { pollable.poll_set.wake(IoEvents::ERR) }, 1);
    assert_eq!(counter.count(), 1);

    let all_readable = IoEvents::all() & !IoEvents::NVAL;
    assert!(all_readable.contains(IoEvents::IN));
    assert!(all_readable.contains(IoEvents::RDHUP));
    assert!(!all_readable.contains(IoEvents::NVAL));
}
