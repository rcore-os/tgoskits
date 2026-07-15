use core::sync::atomic::{AtomicUsize, Ordering};

use ax_task::{
    CpuId, DeadlineFlags, DeadlinePolicy, FairMode, Nice, SchedulePolicy, TaskError, TaskSystem,
    TaskSystemConfig, ThreadExtension, ThreadExtensionOps, ThreadId, ThreadSpec, ThreadState,
};

mod support;

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static LAST_EXIT_MARKER: AtomicUsize = AtomicUsize::new(0);

#[test]
fn service_batch_limit_is_shared_by_exit_and_reap() {
    let _serial = serial_test();
    prepare_test_runtime();
    LAST_EXIT_MARKER.store(0, Ordering::Release);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let exited = create_detached_extended_thread(&system, 1);

    let callback_pass = system.service_deferred_task_work(1).unwrap();
    assert_eq!(callback_pass.processed(), 1);
    assert_eq!(LAST_EXIT_MARKER.load(Ordering::Acquire), 1);
    assert_eq!(system.thread_state(exited), Ok(ThreadState::Exited));

    let reap_pass = system.service_deferred_task_work(1).unwrap();
    assert_eq!(reap_pass.processed(), 1);
    assert_eq!(system.thread_state(exited), Err(TaskError::StaleThreadId));
}

#[test]
fn exit_callback_cursor_skips_a_reused_low_slot() {
    let _serial = serial_test();
    prepare_test_runtime();
    LAST_EXIT_MARKER.store(0, Ordering::Release);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let low = create_detached_extended_thread(&system, 1);
    let high = create_detached_extended_thread(&system, 2);
    assert!(low.slot() < high.slot());

    assert_eq!(system.dispatch_exit_callbacks(1), Ok(1));
    assert_eq!(LAST_EXIT_MARKER.load(Ordering::Acquire), 1);
    assert_eq!(system.reap_unreferenced_exited(1), Ok(1));
    assert_eq!(system.thread_state(low), Err(TaskError::StaleThreadId));

    let replacement = create_detached_extended_thread(&system, 3);
    assert_eq!(replacement.slot(), low.slot());
    LAST_EXIT_MARKER.store(0, Ordering::Release);

    assert_eq!(system.dispatch_exit_callbacks(1), Ok(1));
    assert_eq!(
        LAST_EXIT_MARKER.load(Ordering::Acquire),
        2,
        "a reused low slot must not overtake the older high-slot callback"
    );
    assert_eq!(system.thread_state(high), Ok(ThreadState::Exited));

    assert_eq!(system.dispatch_exit_callbacks(1), Ok(1));
    assert_eq!(system.reap_unreferenced_exited(2), Ok(2));
}

#[test]
fn reap_cursor_skips_a_reused_low_slot() {
    let _serial = serial_test();
    prepare_test_runtime();
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let low = create_detached_thread(&system);
    let high = create_detached_thread(&system);
    assert!(low.slot() < high.slot());

    assert_eq!(system.reap_unreferenced_exited(1), Ok(1));
    assert_eq!(system.thread_state(low), Err(TaskError::StaleThreadId));

    let replacement = create_detached_thread(&system);
    assert_eq!(replacement.slot(), low.slot());
    assert_eq!(system.reap_unreferenced_exited(1), Ok(1));
    assert_eq!(
        system.thread_state(high),
        Err(TaskError::StaleThreadId),
        "continuous low-slot reuse must not starve an older high-slot record"
    );
    assert_eq!(system.thread_state(replacement), Ok(ThreadState::Exited));
    assert_eq!(system.reap_unreferenced_exited(1), Ok(1));
}

#[test]
fn deadline_backlog_cannot_starve_exit_or_reap_classes() {
    let _serial = serial_test();
    prepare_test_runtime();
    LAST_EXIT_MARKER.store(0, Ordering::Release);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let _idle = system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let policy = SchedulePolicy::deadline(
        DeadlinePolicy::new(1, 10, 20, DeadlineFlags::DL_OVERRUN).unwrap(),
    );
    let first = system.create_thread(ThreadSpec::new(policy)).unwrap();
    let second = system.create_thread(ThreadSpec::new(policy)).unwrap();
    for thread in [&first, &second] {
        system.make_ready(thread.id()).unwrap();
        system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
    }
    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), first.id());
    system.charge_current(cpu.as_mut(), 1, 1, 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 1).unwrap().next(),
        second.id()
    );
    system.charge_current(cpu.as_mut(), 1, 2, 0).unwrap();
    system.schedule(cpu.as_mut(), 2).unwrap();

    let exited = create_detached_extended_thread(&system, 7);
    let deadline_pass = system.service_deferred_task_work(1).unwrap();
    assert_eq!(deadline_pass.processed(), 1);
    assert_eq!(LAST_EXIT_MARKER.load(Ordering::Acquire), 0);
    assert_eq!(system.thread_state(exited), Ok(ThreadState::Exited));

    let exit_pass = system.service_deferred_task_work(1).unwrap();
    assert_eq!(exit_pass.processed(), 1);
    assert_eq!(LAST_EXIT_MARKER.load(Ordering::Acquire), 7);
    assert_eq!(system.thread_state(exited), Ok(ThreadState::Exited));

    let reap_pass = system.service_deferred_task_work(1).unwrap();
    assert_eq!(reap_pass.processed(), 1);
    assert_eq!(system.thread_state(exited), Err(TaskError::StaleThreadId));
}

fn create_detached_thread(system: &TaskSystem) -> ThreadId {
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let thread_id = thread.id();
    system.mark_exited(thread_id).unwrap();
    drop(thread);
    thread_id
}

fn create_detached_extended_thread(system: &TaskSystem, marker: usize) -> ThreadId {
    let extension = unsafe {
        // SAFETY: the static callback table accepts a scalar marker and retains
        // no reference to the scheduler record or callback argument.
        ThreadExtension::new(marker, &EXIT_EXTENSION_OPS)
    };
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_extension(extension))
        .unwrap();
    let thread_id = thread.id();
    system.mark_exited(thread_id).unwrap();
    drop(thread);
    thread_id
}

static EXIT_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: ignore_thread_event,
    on_switch_out: ignore_switch_out,
    on_exit: record_exit_marker,
    on_deadline_overrun: ignore_thread_event,
    drop: ignore_drop,
};

unsafe extern "Rust" fn ignore_thread_event(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn ignore_switch_out(
    _data: usize,
    _thread: ThreadId,
    _reason: ax_task::SwitchReason,
) {
}

unsafe extern "Rust" fn record_exit_marker(data: usize, _thread: ThreadId) {
    LAST_EXIT_MARKER.store(data, Ordering::Release);
}

unsafe extern "Rust" fn ignore_drop(_data: usize) {}

fn serial_test() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn prepare_test_runtime() {
    support::set_hard_irq(false);
    let _ = (
        support::install_handles as fn(usize, core::pin::Pin<&mut ax_task::CpuLocal>),
        support::install_cpu as fn(u32, core::pin::Pin<&mut ax_task::CpuLocal>),
        support::set_online_cpu_count as fn(usize),
        support::ipi_count as fn(u32) -> usize,
        support::resource_release_counts as fn() -> (usize, usize, usize),
        support::last_oneshot_ns as fn() -> u64,
        support::set_timer_resolution_ns as fn(u64),
        support::set_monotonic_ns as fn(u64),
        support::reset_resource_release_counts as fn(),
        support::clear_handles as fn(),
    );
}
