use core::{
    pin::pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    boxed::Box,
    cell::Cell,
    pin::Pin,
    sync::Arc,
    thread,
    vec::Vec,
};

use crate::hard_call::{HardCall, HardCallQueue};

struct TrackingAllocator;

thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static DEALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every operation delegates to the process System allocator. The
// thread-local counters observe calls without changing allocation semantics.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|tracking| {
            if tracking.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        TRACK_ALLOCATIONS.with(|tracking| {
            if tracking.get() {
                DEALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static TEST_ALLOCATOR: TrackingAllocator = TrackingAllocator;

unsafe fn increment(argument: *mut ()) {
    let counter = unsafe { &*(argument as *const AtomicUsize) };
    counter.fetch_add(1, Ordering::Relaxed);
}

unsafe fn record_sequence(argument: *mut ()) {
    let slot = unsafe { &*(argument as *const (&AtomicUsize, usize)) };
    let previous = slot.0.fetch_add(1, Ordering::Relaxed);
    assert_eq!(previous + 1, slot.1);
}

#[test]
fn hard_call_transport_completes_without_owning_the_argument() {
    let queue = HardCallQueue::new();
    let counter = AtomicUsize::new(0);
    let call = pin!(HardCall::new(
        increment,
        &counter as *const AtomicUsize as *mut (),
    ));

    assert!(unsafe { queue.publish(call.as_ref()) });
    let outcome = queue.drain(1);

    assert_eq!(outcome.completed, 1);
    assert!(!outcome.more_work);
    assert!(call.is_complete());
    assert_eq!(counter.load(Ordering::Relaxed), 1);
}

#[test]
fn hard_call_irq_drain_is_bounded_and_preserves_fifo_remainder() {
    let queue = HardCallQueue::new();
    let sequence = AtomicUsize::new(0);
    let first_data: (&AtomicUsize, usize) = (&sequence, 1);
    let second_data: (&AtomicUsize, usize) = (&sequence, 2);
    let first = pin!(HardCall::new(
        record_sequence,
        &first_data as *const (&AtomicUsize, usize) as *mut (),
    ));
    let second = pin!(HardCall::new(
        record_sequence,
        &second_data as *const (&AtomicUsize, usize) as *mut (),
    ));

    assert!(unsafe { queue.publish(first.as_ref()) });
    assert!(!unsafe { queue.publish(second.as_ref()) });

    let first_batch = queue.drain(1);
    assert_eq!(first_batch.completed, 1);
    assert!(first_batch.more_work);

    let second_batch = queue.drain(1);
    assert_eq!(second_batch.completed, 1);
    assert!(!second_batch.more_work);
    assert!(first.is_complete());
    assert!(second.is_complete());
    assert_eq!(sequence.load(Ordering::Relaxed), 2);
}

#[test]
fn hard_call_budget_of_64_preserves_the_65th_request() {
    let queue = HardCallQueue::new();
    let counter = AtomicUsize::new(0);
    let calls = (0..65)
        .map(|_| {
            Box::pin(HardCall::new(
                increment,
                &counter as *const AtomicUsize as *mut (),
            ))
        })
        .collect::<Vec<Pin<Box<HardCall>>>>();

    for call in &calls {
        unsafe { queue.publish(call.as_ref()) };
    }

    let first = queue.drain(64);
    assert_eq!(first.completed, 64);
    assert!(first.more_work);
    assert_eq!(counter.load(Ordering::Relaxed), 64);

    let second = queue.drain(64);
    assert_eq!(second.completed, 1);
    assert!(!second.more_work);
    assert_eq!(counter.load(Ordering::Relaxed), 65);
}

#[test]
fn hard_call_accepts_concurrent_producers() {
    const PRODUCERS: usize = 8;

    let queue = Arc::new(HardCallQueue::new());
    let counter = Arc::new(AtomicUsize::new(0));
    let published = Arc::new(AtomicUsize::new(0));

    thread::scope(|scope| {
        for _ in 0..PRODUCERS {
            let queue = Arc::clone(&queue);
            let counter = Arc::clone(&counter);
            let published = Arc::clone(&published);
            scope.spawn(move || {
                let call = pin!(HardCall::new(increment, Arc::as_ptr(&counter) as *mut (),));
                unsafe { queue.publish(call.as_ref()) };
                published.fetch_add(1, Ordering::Release);
                call.wait();
            });
        }

        while published.load(Ordering::Acquire) != PRODUCERS {
            thread::yield_now();
        }
        let outcome = queue.drain(PRODUCERS);
        assert_eq!(outcome.completed, PRODUCERS);
        assert!(!outcome.more_work);
    });

    assert_eq!(counter.load(Ordering::Relaxed), PRODUCERS);
}

struct DropProbe<'a>(&'a AtomicBool);

impl Drop for DropProbe<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

unsafe fn observe_drop_probe(argument: *mut ()) {
    let probe = unsafe { &*(argument as *const DropProbe<'_>) };
    assert!(!probe.0.load(Ordering::Acquire));
}

#[test]
fn irq_drain_does_not_take_ownership_or_drop_the_argument() {
    let queue = HardCallQueue::new();
    let dropped = AtomicBool::new(false);
    let probe = DropProbe(&dropped);
    let call = pin!(HardCall::new(
        observe_drop_probe,
        &probe as *const DropProbe<'_> as *mut (),
    ));

    unsafe { queue.publish(call.as_ref()) };
    assert_eq!(queue.drain(1).completed, 1);
    assert!(!dropped.load(Ordering::Acquire));

    drop(probe);
    assert!(dropped.load(Ordering::Acquire));
}

#[test]
fn irq_drain_allocates_and_deallocates_nothing() {
    let queue = HardCallQueue::new();
    let counter = AtomicUsize::new(0);
    let call = pin!(HardCall::new(
        increment,
        &counter as *const AtomicUsize as *mut (),
    ));
    unsafe { queue.publish(call.as_ref()) };

    ALLOCATIONS.with(|count| count.set(0));
    DEALLOCATIONS.with(|count| count.set(0));
    TRACK_ALLOCATIONS.with(|tracking| tracking.set(true));
    let outcome = queue.drain(1);
    TRACK_ALLOCATIONS.with(|tracking| tracking.set(false));

    assert_eq!(outcome.completed, 1);
    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
    assert_eq!(DEALLOCATIONS.with(Cell::get), 0);
}

#[test]
fn delivery_failure_cancels_a_published_stack_node_without_execution() {
    let queue = HardCallQueue::new();
    let counter = AtomicUsize::new(0);
    let call = pin!(HardCall::new(
        increment,
        &counter as *const AtomicUsize as *mut (),
    ));
    unsafe { queue.publish(call.as_ref()) };

    assert!(queue.cancel_after_delivery_error(call.as_ref()));
    assert!(call.is_complete());
    assert_eq!(counter.load(Ordering::Relaxed), 0);
    assert_eq!(queue.drain(1).completed, 0);
}
