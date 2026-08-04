use alloc::{format, sync::Arc, task::Wake, vec::Vec};
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Waker},
};

use axpoll::{IoEvents, PollSet, Pollable};
use axtest::prelude::*;

use crate as axpoll;

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

#[axtest]
fn axpoll_event_masks_and_empty_wake_rules_hold() {
    let events = IoEvents::IN | IoEvents::OUT | IoEvents::ALWAYS_POLL;
    ax_assert!(events.contains(IoEvents::IN));
    ax_assert!(events.contains(IoEvents::OUT));
    ax_assert!(events.contains(IoEvents::ERR));
    ax_assert!(events.contains(IoEvents::HUP));
    ax_assert!(!events.contains(IoEvents::NVAL));
    ax_assert!(format!("{:?}", IoEvents::RDHUP).contains("RDHUP"));

    let poll_set = PollSet::default();
    ax_assert_eq!(unsafe { poll_set.wake(IoEvents::IN) }, 0);
    ax_assert_eq!(poll_set.wake_from_irq(IoEvents::IN), 0);
}

#[axtest]
fn axpoll_wakes_only_matching_interests() {
    let poll_set = PollSet::new();
    let read_counter = WakeCounter::new();
    let write_counter = WakeCounter::new();
    let read_waker = counter_waker(&read_counter);
    let write_waker = counter_waker(&write_counter);

    unsafe {
        poll_set.register(&read_waker, IoEvents::IN);
        poll_set.register(&write_waker, IoEvents::OUT);
    }

    ax_assert_eq!(unsafe { poll_set.wake(IoEvents::IN) }, 1);
    ax_assert_eq!(read_counter.count(), 1);
    ax_assert_eq!(write_counter.count(), 0);

    ax_assert_eq!(poll_set.wake_from_irq(IoEvents::OUT), 1);
    ax_assert_eq!(read_counter.count(), 1);
    ax_assert_eq!(write_counter.count(), 1);
    ax_assert_eq!(unsafe { poll_set.wake(IoEvents::IN | IoEvents::OUT) }, 0);
}

#[axtest]
fn axpoll_capacity_overwrite_and_drop_rules_hold() {
    let poll_set = PollSet::new();
    let counters = (0..65).map(|_| WakeCounter::new()).collect::<Vec<_>>();

    for counter in &counters {
        let waker = counter_waker(counter);
        unsafe { poll_set.register(&waker, IoEvents::IN) };
    }

    ax_assert_eq!(unsafe { poll_set.wake(IoEvents::IN) }, 64);
    ax_assert_eq!(
        counters
            .iter()
            .map(|counter| counter.count())
            .sum::<usize>(),
        65
    );

    let poll_set = PollSet::new();
    let drop_counter = WakeCounter::new();
    for _ in 0..4 {
        let waker = counter_waker(&drop_counter);
        unsafe { poll_set.register(&waker, IoEvents::OUT) };
    }
    drop(poll_set);
    ax_assert_eq!(drop_counter.count(), 4);
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

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        unsafe { self.poll_set.register(context.waker(), events) };
    }
}

#[axtest]
fn axpoll_pollable_context_registration_rules_hold() {
    let pollable = FixedPollable::new(IoEvents::IN | IoEvents::HUP);
    let counter = WakeCounter::new();
    let waker = counter_waker(&counter);
    let mut context = Context::from_waker(&waker);

    ax_assert!(pollable.poll().contains(IoEvents::IN));
    ax_assert!(pollable.poll().contains(IoEvents::HUP));
    pollable.register(&mut context, IoEvents::IN | IoEvents::ERR);

    ax_assert_eq!(unsafe { pollable.poll_set.wake(IoEvents::OUT) }, 0);
    ax_assert_eq!(counter.count(), 0);
    ax_assert_eq!(pollable.poll_set.wake_from_irq(IoEvents::ERR), 1);
    ax_assert_eq!(counter.count(), 1);

    let all_readable = IoEvents::all() & !IoEvents::NVAL;
    ax_assert!(all_readable.contains(IoEvents::IN));
    ax_assert!(all_readable.contains(IoEvents::RDHUP));
    ax_assert!(!all_readable.contains(IoEvents::NVAL));
}
