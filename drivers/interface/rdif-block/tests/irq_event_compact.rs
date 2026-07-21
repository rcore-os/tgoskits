use std::{
    alloc::{GlobalAlloc, Layout, System},
    mem::{needs_drop, size_of},
    sync::atomic::{AtomicUsize, Ordering},
};

use rdif_block::{Event, IrqEvidenceId};

struct CountingAllocator;

static ALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        // SAFETY: this allocator forwards the caller's valid allocation request.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: this allocator forwards the matching deallocation request.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

fn assert_copy<T: Copy>() {}

#[test]
fn irq_event_is_a_compact_copyable_queue_fact() {
    assert_copy::<Event>();
    assert!(!needs_drop::<Event>());
    assert!(
        size_of::<Event>() <= 16,
        "IRQ event must fit in at most two machine words, got {} bytes",
        size_of::<Event>()
    );
}

#[test]
fn evidence_identity_is_a_compact_irq_safe_value() {
    assert_copy::<IrqEvidenceId>();
    assert!(!needs_drop::<IrqEvidenceId>());
    assert_eq!(
        size_of::<IrqEvidenceId>(),
        16,
        "evidence IDs are stored directly in fixed IRQ mailboxes"
    );
}

#[test]
fn constructing_and_filtering_irq_queue_facts_does_not_allocate() {
    let before = ALLOCATION_COUNT.load(Ordering::Relaxed);

    let mut event = Event::from_queue_bits(1 << 2);
    event.push_queue(5);
    let queue = event.for_queue(5).expect("queue 5 must be affected");
    let copied = event;

    let after = ALLOCATION_COUNT.load(Ordering::Relaxed);
    assert_eq!(after, before, "IRQ event operations must not allocate");
    assert_eq!(queue.queue_id(), 5);
    assert!(copied.for_queue(2).is_some());
}
