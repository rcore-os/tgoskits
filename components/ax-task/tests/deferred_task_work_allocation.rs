//! Allocation audit for scheduler-owned deferred notification collection.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    boxed::Box,
    cell::Cell,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use ax_task::{
    CpuId, DeadlineFlags, DeadlinePolicy, FairMode, Nice, SchedulePolicy, TaskSystem,
    TaskSystemConfig, ThreadExtension, ThreadExtensionOps, ThreadId, ThreadPolicyApplied,
    ThreadSpec,
};

mod support;

struct CountingAllocator;

static DEADLINE_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

std::thread_local! {
    static ALLOCATION_AUDIT: Cell<Option<usize>> = const { Cell::new(None) };
}

// SAFETY: every operation is forwarded unchanged to the system allocator; the
// thread-local counter only observes allocations made by the operation under
// test, excluding the test harness and unrelated test threads.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: this implementation forwards the caller's allocator contract.
        let pointer = unsafe { System.alloc(layout) };
        record_allocation(pointer);
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: this implementation forwards the caller's allocator contract.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        record_allocation(pointer);
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: this implementation forwards the caller's allocator contract.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: this implementation forwards the caller's allocator contract.
        let replacement = unsafe { System.realloc(pointer, layout, new_size) };
        record_allocation(replacement);
        replacement
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

static DEADLINE_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: ignore_thread_event,
    on_switch_out: ignore_switch_out,
    on_policy_applied: ignore_policy_applied,
    on_exit: ignore_thread_event,
    on_deadline_overrun: ignore_thread_event,
    drop: ignore_drop,
};

static HARD_IRQ_DEADLINE_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: ignore_thread_event,
    on_switch_out: ignore_switch_out,
    on_policy_applied: ignore_policy_applied,
    on_exit: ignore_thread_event,
    on_deadline_overrun: record_deadline_overrun,
    drop: ignore_drop,
};

#[test]
fn hard_irq_cannot_consume_or_dispatch_deadline_task_work() {
    DEADLINE_CALLBACKS.store(0, Ordering::Release);
    let (system, _cpu) = deadline_overrun_fixture(&HARD_IRQ_DEADLINE_EXTENSION_OPS);

    support::set_hard_irq(true);
    let dispatched = system.dispatch_deadline_overruns(1);
    support::set_hard_irq(false);

    assert_eq!(
        dispatched,
        Err(ax_task::TaskError::UnsafeContext),
        "hard IRQ must not dispatch OS callbacks"
    );
    assert_eq!(DEADLINE_CALLBACKS.load(Ordering::Acquire), 0);
    assert_eq!(system.dispatch_deadline_overruns(1), Ok(1));
    assert_eq!(DEADLINE_CALLBACKS.load(Ordering::Acquire), 1);
}

#[test]
fn deadline_overrun_collection_uses_only_registry_owned_storage() {
    retain_fake_runtime_helpers();
    let (system, _cpu) = deadline_overrun_fixture(&DEADLINE_EXTENSION_OPS);

    assert_no_alloc(|| assert_eq!(system.dispatch_deadline_overruns(1), Ok(1)));
    assert_no_alloc(|| assert_eq!(system.dispatch_deadline_overruns(1), Ok(0)));
}

#[test]
fn allocation_audit_ignores_allocations_from_other_threads() {
    let phase = Arc::new(AtomicUsize::new(0));
    let helper_phase = Arc::clone(&phase);
    let helper = std::thread::spawn(move || {
        helper_phase.store(1, Ordering::Release);
        while helper_phase.load(Ordering::Acquire) != 2 {
            core::hint::spin_loop();
        }
        let unrelated = Box::new([0_u8; 64]);
        std::hint::black_box(&unrelated);
        helper_phase.store(3, Ordering::Release);
        drop(unrelated);
    });
    while phase.load(Ordering::Acquire) != 1 {
        core::hint::spin_loop();
    }

    assert_no_alloc(|| {
        phase.store(2, Ordering::Release);
        while phase.load(Ordering::Acquire) != 3 {
            core::hint::spin_loop();
        }
    });
    helper.join().expect("allocation helper must finish");
}

fn deadline_overrun_fixture(
    extension_ops: &'static ThreadExtensionOps,
) -> (TaskSystem, Pin<Box<ax_task::CpuLocal>>) {
    retain_fake_runtime_helpers();
    let system = TaskSystem::new(TaskSystemConfig::new(1)).expect("task system must initialize");
    let mut cpu = system
        .create_cpu_local(CpuId::new(0))
        .expect("CPU local must initialize");
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .expect("idle thread must initialize");
    system
        .bring_cpu_online(cpu.as_mut())
        .expect("CPU must come online");
    let extension = unsafe {
        // SAFETY: callbacks ignore the scalar payload and retain no references.
        ThreadExtension::new(0, extension_ops)
    };
    let policy = DeadlinePolicy::new(5, 10, 20, DeadlineFlags::DL_OVERRUN)
        .expect("deadline policy must be valid");
    let deadline = system
        .create_thread(ThreadSpec::new(SchedulePolicy::deadline(policy)).with_extension(extension))
        .expect("deadline thread must initialize");
    system.make_ready(deadline.id()).unwrap();
    system.enqueue(cpu.as_mut(), deadline.id(), 0).unwrap();
    system.schedule(cpu.as_mut(), 0).unwrap();
    system.charge_current(cpu.as_mut(), 5, 5, 0).unwrap();
    system.schedule(cpu.as_mut(), 5).unwrap();
    (system, cpu)
}

fn record_allocation(pointer: *mut u8) {
    if pointer.is_null() {
        return;
    }
    let _ = ALLOCATION_AUDIT.try_with(|audit| {
        if let Some(allocations) = audit.get() {
            audit.set(Some(allocations.saturating_add(1)));
        }
    });
}

fn assert_no_alloc<T>(operation: impl FnOnce() -> T) -> T {
    ALLOCATION_AUDIT.with(|audit| {
        assert_eq!(
            audit.replace(Some(0)),
            None,
            "allocation audits must not nest"
        );
        let value = operation();
        let allocations = audit
            .replace(None)
            .expect("allocation audit must remain active");
        assert_eq!(allocations, 0, "deferred notification collection allocated");
        value
    })
}

unsafe extern "Rust" fn ignore_thread_event(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn record_deadline_overrun(_data: usize, _thread: ThreadId) {
    DEADLINE_CALLBACKS.fetch_add(1, Ordering::AcqRel);
}

unsafe extern "Rust" fn ignore_switch_out(
    _data: usize,
    _thread: ThreadId,
    _reason: ax_task::SwitchReason,
) {
}

unsafe extern "Rust" fn ignore_policy_applied(
    _data: usize,
    _thread: ThreadId,
    _event: ThreadPolicyApplied,
) {
}

unsafe extern "Rust" fn ignore_drop(_data: usize) {}

fn retain_fake_runtime_helpers() {
    let _ = (
        support::install_handles as fn(usize, Pin<&mut ax_task::CpuLocal>),
        support::install_cpu as fn(u32, Pin<&mut ax_task::CpuLocal>),
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
