//! Allocation audit for scheduler-owned deferred notification collection.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_task::{
    CpuId, DeadlineFlags, DeadlinePolicy, FairMode, Nice, SchedulePolicy, TaskSystem,
    TaskSystemConfig, ThreadExtension, ThreadExtensionOps, ThreadId, ThreadSpec,
};

mod support;

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static DEADLINE_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every operation is forwarded unchanged to the system allocator; the
// counter only observes allocations made by the operation under test.
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

static DEADLINE_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: ignore_thread_event,
    on_switch_out: ignore_switch_out,
    on_exit: ignore_thread_event,
    on_deadline_overrun: ignore_thread_event,
    drop: ignore_drop,
};

static HARD_IRQ_DEADLINE_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: ignore_thread_event,
    on_switch_out: ignore_switch_out,
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

fn assert_no_alloc(operation: impl FnOnce()) {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    operation();
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(after, before, "deferred notification collection allocated");
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
