use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_task::{
    CpuId, FairMode, Nice, RtPriority, SchedulePolicy, SwitchReason, TaskSystem, TaskSystemConfig,
    ThreadExtension, ThreadExtensionOps, ThreadId, ThreadResources, ThreadSpec,
    runtime::{AddressSpaceHandle, ExecutionContextHandle, RuntimeStatus, StackHandle, TlsHandle},
};

mod support;

static RESOURCE_RELEASE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn runtime_snapshot_includes_the_running_residual_before_switch_commit() {
    support::clear_handles();
    support::set_online_cpu_count(1);
    support::set_hard_irq(false);
    assert_eq!(support::ipi_count(0), 0);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    system.charge_current(cpu.as_mut(), 4, 4, 0).unwrap();
    let snapshot = system.thread_runtime(running.id(), 7).unwrap();
    assert_eq!(snapshot.charged_runtime_ns(), 7);
    assert!(snapshot.is_running());

    let fifo = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(1).unwrap(),
        )))
        .unwrap();
    system.make_ready(fifo.id()).unwrap();
    system.enqueue(cpu.as_mut(), fifo.id(), 7).unwrap();

    let decision = system.yield_current(cpu.as_mut(), 10).unwrap();
    assert_eq!(decision.next(), fifo.id());
    assert_eq!(decision.switch_reason(), SwitchReason::Yield);
    let snapshot = system.thread_runtime(running.id(), 100).unwrap();
    assert_eq!(snapshot.charged_runtime_ns(), 10);
    assert!(!snapshot.is_running());
    support::clear_handles();
}

#[test]
fn current_address_space_replacement_updates_only_the_running_owner_record() {
    support::clear_handles();
    let resources = unsafe {
        ThreadResources::new(
            ExecutionContextHandle::from_raw(1),
            StackHandle::from_raw(2),
            TlsHandle::from_raw(3),
            AddressSpaceHandle::from_raw(4),
        )
    };
    let spec = unsafe { ThreadSpec::new(SchedulePolicy::default()).with_resources(resources) };
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let bootstrap = system.install_bootstrap_thread(cpu.as_mut(), spec).unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    assert_eq!(
        system.replace_current_address_space(cpu.as_mut(), AddressSpaceHandle::NONE),
        Err(ax_task::TaskError::InvalidConfiguration)
    );
    assert_eq!(
        system
            .replace_current_address_space(cpu.as_mut(), test_address_space(5))
            .unwrap(),
        test_address_space(4)
    );
    assert_eq!(
        system
            .replace_current_address_space(cpu.as_mut(), test_address_space(6))
            .unwrap(),
        test_address_space(5)
    );

    let next_resources = unsafe {
        ThreadResources::new(
            ExecutionContextHandle::from_raw(10),
            StackHandle::from_raw(11),
            TlsHandle::from_raw(12),
            AddressSpaceHandle::from_raw(13),
        )
    };
    let next = system
        .create_thread(unsafe {
            ThreadSpec::new(SchedulePolicy::fifo(RtPriority::new(1).unwrap()))
                .with_resources(next_resources)
        })
        .unwrap();
    system.make_ready(next.id()).unwrap();
    system.enqueue(cpu.as_mut(), next.id(), 0).unwrap();

    assert_eq!(system.schedule(cpu.as_mut(), 0).unwrap().next(), next.id());
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert_eq!(
        system.exit_current(cpu.as_mut()).unwrap().next(),
        bootstrap.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert_eq!(
        system
            .replace_current_address_space(cpu.as_mut(), test_address_space(7))
            .unwrap(),
        test_address_space(6),
        "switching away and back must retain the exec-time address-space token"
    );
}

#[test]
fn rejected_and_reaped_specs_drop_each_extension_exactly_once() {
    DROP_COUNTS[0].store(0, Ordering::Release);
    DROP_COUNTS[1].store(0, Ordering::Release);
    let rejected = unsafe { ThreadExtension::new(0, &DROP_COUNT_EXTENSION_OPS) };
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let invalid = ThreadSpec::new(SchedulePolicy::default())
        .with_affinity(ax_task::CpuSet::all(2))
        .with_extension(rejected);
    assert!(system.create_thread(invalid).is_err());
    assert_eq!(DROP_COUNTS[0].load(Ordering::Acquire), 1);

    let reaped = unsafe { ThreadExtension::new(1, &DROP_COUNT_EXTENSION_OPS) };
    let handle = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_extension(reaped))
        .unwrap();
    let id = handle.id();
    system.mark_exited(id).unwrap();
    drop(handle);
    assert!(
        system
            .service_deferred_task_work(64)
            .unwrap()
            .made_progress()
    );
    assert_eq!(
        system.thread_state(id),
        Err(ax_task::TaskError::StaleThreadId)
    );
    assert_eq!(DROP_COUNTS[1].load(Ordering::Acquire), 1);
}

#[test]
fn mark_exited_pins_extension_until_exit_callback_returns() {
    MARK_EXIT_CALLBACK_ENTERED.store(false, Ordering::Release);
    MARK_EXIT_CALLBACK_RELEASE.store(false, Ordering::Release);
    MARK_EXIT_DROPS.store(0, Ordering::Release);
    let system = std::sync::Arc::new(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let extension = unsafe { ThreadExtension::new(0, &MARK_EXIT_EXTENSION_OPS) };
    let handle = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_extension(extension))
        .unwrap();
    let id = handle.id();
    drop(handle);

    system.mark_exited(id).unwrap();
    let service_system = std::sync::Arc::clone(&system);
    let servicing = std::thread::spawn(move || service_system.service_deferred_task_work(1));
    while !MARK_EXIT_CALLBACK_ENTERED.load(Ordering::Acquire) {
        std::thread::yield_now();
    }

    let reap_during_callback = system.reap_thread(id);
    let drops_during_callback = MARK_EXIT_DROPS.load(Ordering::Acquire);
    MARK_EXIT_CALLBACK_RELEASE.store(true, Ordering::Release);
    assert!(servicing.join().unwrap().unwrap().made_progress());

    assert_eq!(
        reap_during_callback,
        Err(ax_task::TaskError::ThreadBusy),
        "the callback's borrowed extension view must pin its registry record"
    );
    assert_eq!(drops_during_callback, 0);
    assert_eq!(MARK_EXIT_DROPS.load(Ordering::Acquire), 0);
    assert_eq!(system.reap_unreferenced_exited(1), Ok(1));
    assert_eq!(MARK_EXIT_DROPS.load(Ordering::Acquire), 1);
}

#[test]
fn extension_lease_pins_the_record_until_the_borrow_is_released() {
    DROP_COUNTS[2].store(0, Ordering::Release);
    let extension = unsafe { ThreadExtension::new(2, &DROP_COUNT_EXTENSION_OPS) };
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let handle = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_extension(extension))
        .unwrap();
    let id = handle.id();
    let lease = system
        .thread_extension_lease(handle.clone())
        .unwrap()
        .unwrap();
    assert_eq!(lease.thread_id(), id);
    assert_eq!(lease.data(), 2);

    system.mark_exited(id).unwrap();
    drop(handle);
    assert_eq!(system.reap_unreferenced_exited(1).unwrap(), 0);
    assert!(
        system
            .service_deferred_task_work(1)
            .unwrap()
            .made_progress()
    );
    assert_eq!(DROP_COUNTS[2].load(Ordering::Acquire), 0);

    drop(lease);
    assert!(
        system
            .service_deferred_task_work(1)
            .unwrap()
            .made_progress()
    );
    assert_eq!(DROP_COUNTS[2].load(Ordering::Acquire), 1);
}

#[test]
fn thread_resources_release_each_unique_runtime_handle_once() {
    let _serial = RESOURCE_RELEASE_TEST_LOCK.lock().unwrap();
    support::reset_resource_release_counts();
    let resources = unsafe {
        ThreadResources::new(
            ExecutionContextHandle::from_raw(1),
            StackHandle::from_raw(2),
            TlsHandle::from_raw(3),
            AddressSpaceHandle::from_raw(4),
        )
    };
    let spec = unsafe { ThreadSpec::new(SchedulePolicy::default()).with_resources(resources) };
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let handle = system.create_thread(spec).unwrap();
    let id = handle.id();
    system.mark_exited(id).unwrap();
    system.reap_thread_handle(handle).unwrap();

    assert_eq!(support::resource_release_counts(), (1, 1, 1));
}

#[test]
fn context_destroy_failure_keeps_context_dependent_resources_live() {
    let _serial = RESOURCE_RELEASE_TEST_LOCK.lock().unwrap();
    support::reset_resource_release_counts();
    let resources = unsafe {
        ThreadResources::new(
            ExecutionContextHandle::from_raw(usize::MAX),
            StackHandle::from_raw(2),
            TlsHandle::from_raw(3),
            AddressSpaceHandle::from_raw(4),
        )
    };
    let spec = unsafe { ThreadSpec::new(SchedulePolicy::default()).with_resources(resources) };
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let handle = system.create_thread(spec).unwrap();
    system.mark_exited(handle.id()).unwrap();

    let error = system.reap_thread_handle(handle).unwrap_err();
    assert_eq!(
        error.task_error(),
        ax_task::TaskError::RuntimeFailure(RuntimeStatus::Busy as u32)
    );
    assert_eq!(
        support::resource_release_counts(),
        (1, 0, 0),
        "a live context may still reference its stack and TLS"
    );
}

fn test_address_space(raw: usize) -> AddressSpaceHandle {
    // SAFETY: the integration runtime treats these non-zero scalar values as
    // inert address-space identities and never dereferences them.
    unsafe { AddressSpaceHandle::from_raw(raw) }
}

static DROP_COUNT_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: ignore_thread_event,
    on_switch_out: ignore_switch_out,
    on_exit: ignore_thread_event,
    on_deadline_overrun: ignore_thread_event,
    drop: count_drop,
};

static DROP_COUNTS: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];

unsafe extern "Rust" fn ignore_thread_event(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn ignore_switch_out(_data: usize, _thread: ThreadId, _reason: SwitchReason) {}

unsafe extern "Rust" fn count_drop(data: usize) {
    DROP_COUNTS[data].fetch_add(1, Ordering::Release);
}

static MARK_EXIT_CALLBACK_ENTERED: AtomicBool = AtomicBool::new(false);
static MARK_EXIT_CALLBACK_RELEASE: AtomicBool = AtomicBool::new(false);
static MARK_EXIT_DROPS: AtomicUsize = AtomicUsize::new(0);
static MARK_EXIT_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: ignore_thread_event,
    on_switch_out: ignore_switch_out,
    on_exit: block_mark_exit_callback,
    on_deadline_overrun: ignore_thread_event,
    drop: count_mark_exit_drop,
};

unsafe extern "Rust" fn block_mark_exit_callback(_data: usize, _thread: ThreadId) {
    MARK_EXIT_CALLBACK_ENTERED.store(true, Ordering::Release);
    while !MARK_EXIT_CALLBACK_RELEASE.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
}

unsafe extern "Rust" fn count_mark_exit_drop(_data: usize) {
    MARK_EXIT_DROPS.fetch_add(1, Ordering::Release);
}
