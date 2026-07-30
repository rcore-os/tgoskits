//! Allocation contract for the owner runqueue scheduling hot path.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

use ax_task::{CpuId, SchedulePolicy, TaskSystem, TaskSystemConfig, ThreadSpec};

mod support;

struct AuditAllocator;

std::thread_local! {
    static ALLOCATION_AUDIT: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}

// SAFETY: every operation is forwarded unchanged to the system allocator. The
// thread-local counters only observe the owner scheduler operation under test.
unsafe impl GlobalAlloc for AuditAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: this implementation forwards the caller's allocator contract.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record_allocation();
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record_deallocation();
        // SAFETY: this implementation forwards the caller's allocator contract.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: AuditAllocator = AuditAllocator;

#[test]
fn fair_schedule_rotation_reuses_owner_runqueue_storage() {
    retain_fake_runtime_helpers();
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let threads = (0..2)
        .map(|_| {
            let thread = system
                .create_thread(ThreadSpec::new(SchedulePolicy::default()))
                .unwrap();
            system.make_ready(thread.id()).unwrap();
            system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
            thread
        })
        .collect::<Vec<_>>();
    system.schedule(cpu.as_mut(), 0).unwrap();
    system.yield_current(cpu.as_mut(), 1).unwrap();

    assert_no_alloc_or_free(|| {
        system.yield_current(cpu.as_mut(), 2).unwrap();
    });
    assert_eq!(threads.len(), 2);
}

fn record_allocation() {
    let _ = ALLOCATION_AUDIT.try_with(|audit| {
        if let Some((allocations, deallocations)) = audit.get() {
            audit.set(Some((allocations.saturating_add(1), deallocations)));
        }
    });
}

fn record_deallocation() {
    let _ = ALLOCATION_AUDIT.try_with(|audit| {
        if let Some((allocations, deallocations)) = audit.get() {
            audit.set(Some((allocations, deallocations.saturating_add(1))));
        }
    });
}

fn assert_no_alloc_or_free<T>(operation: impl FnOnce() -> T) -> T {
    ALLOCATION_AUDIT.with(|audit| {
        assert_eq!(
            audit.replace(Some((0, 0))),
            None,
            "scheduler allocation audits must not nest"
        );
        let value = operation();
        let observed = audit
            .replace(None)
            .expect("scheduler allocation audit must remain active");
        assert_eq!(
            observed,
            (0, 0),
            "owner fair scheduling must not allocate or free runqueue storage"
        );
        value
    })
}

fn retain_fake_runtime_helpers() {
    let _ = (
        support::install_handles as fn(usize, core::pin::Pin<&mut ax_task::CpuLocal>),
        support::install_cpu as fn(u32, core::pin::Pin<&mut ax_task::CpuLocal>),
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
