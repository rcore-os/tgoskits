use std::{
    alloc::{GlobalAlloc, Layout, System},
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_task::{CpuLocal, PiLockId, SchedulePolicy, TaskSystem, TaskSystemConfig, ThreadSpec};

mod support;

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every operation is forwarded unchanged to the process system
// allocator; the counter is observational and does not affect allocation.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: this implementation forwards the caller's allocator contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: this implementation forwards the caller's allocator contract.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn pi_registration_handoff_and_cancel_do_not_allocate() {
    retain_fake_runtime_helpers();
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let selected = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let cancelled = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let lock = PiLockId::new(0x5049);

    let selected_wait = assert_no_alloc(|| {
        system
            .pi_wait_start(lock, selected.id(), owner.id())
            .unwrap()
    });
    let cancelled_wait = assert_no_alloc(|| {
        system
            .pi_wait_start(lock, cancelled.id(), owner.id())
            .unwrap()
    });
    assert_no_alloc(|| system.pi_wait_cancel(cancelled_wait).unwrap());
    assert_no_alloc(|| {
        system
            .pi_mutex_handoff(lock, owner.id(), Some(selected.id()))
            .unwrap()
    });
    assert!(selected_wait.is_granted());
}

fn retain_fake_runtime_helpers() {
    let _ = (
        support::install_handles as fn(usize, Pin<&mut CpuLocal>),
        support::install_cpu as fn(u32, Pin<&mut CpuLocal>),
        support::set_online_cpu_count as fn(usize),
        support::set_hard_irq as fn(bool),
        support::ipi_count as fn(u32) -> usize,
        support::resource_release_counts as fn() -> (usize, usize, usize),
        support::last_oneshot_ns as fn() -> u64,
        support::set_timer_resolution_ns as fn(u64),
        support::set_monotonic_ns as fn(u64),
        support::reset_resource_release_counts as fn(),
        support::clear_handles as fn(),
    );
}

fn assert_no_alloc<T>(operation: impl FnOnce() -> T) -> T {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let result = operation();
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(after, before, "PI scheduler operation allocated");
    result
}
