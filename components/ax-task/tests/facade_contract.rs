use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use ax_task::{
    CpuId, FairMode, Nice, SchedulePolicy, SchedulerTickGate, SchedulerTickWorkDisposition,
    TaskError, TaskSystem, TaskSystemConfig, ThreadExtension, ThreadExtensionOps, ThreadId,
    ThreadSpec, ThreadState, WakeResult, current_thread_extension, current_thread_id,
    on_clock_event, publish_scheduler_tick as publish_scheduler_tick_work,
    runtime::MonotonicInstant,
};

pub mod support;
use support::TaskSystemClockTestExt;

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static SCHEDULER_TICK_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static SCHEDULER_TICK_OBSERVED_NS: AtomicU64 = AtomicU64::new(0);
static SCHEDULER_TICK_RETRIES: AtomicUsize = AtomicUsize::new(0);
static BLOCKING_TICK_ENTERED: AtomicBool = AtomicBool::new(false);
static RELEASE_BLOCKING_TICK: AtomicBool = AtomicBool::new(false);

fn instant(nanos: u64) -> MonotonicInstant {
    MonotonicInstant::from_nanos(nanos).unwrap()
}

fn publish_scheduler_tick(now_ns: u64) {
    publish_scheduler_tick_at(now_ns, now_ns);
}

fn publish_scheduler_tick_at(scheduler_now_ns: u64, monotonic_now_ns: u64) {
    support::set_scheduler_ns(scheduler_now_ns);
    support::set_monotonic_ns(monotonic_now_ns);
    let outcome = on_clock_event(instant(monotonic_now_ns), 1).unwrap();
    publish_scheduler_tick_work(outcome.scheduler_tick_stamp()).unwrap();
}

#[test]
fn facade_reports_uninitialized_then_uses_runtime_owned_objects() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    assert_eq!(current_thread_id(), Err(TaskError::NotInitialized));

    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let extension = unsafe { ThreadExtension::new(0x1234, &TEST_EXTENSION_OPS) };
    let bootstrap = system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
        )
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    assert_eq!(current_thread_id().unwrap(), bootstrap.id());
    assert_eq!(
        std::thread::spawn(current_thread_id).join().unwrap(),
        Err(TaskError::NotInitialized),
        "a host test thread must not inherit another fixture's borrowed handles"
    );
    assert_eq!(current_thread_id().unwrap(), bootstrap.id());
    assert_eq!(current_thread_extension().unwrap().unwrap().data(), 0x1234);
    let sleeper = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    assert_eq!(
        system.block_current_at(cpu.as_mut(), 1).unwrap().next(),
        bootstrap.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert_eq!(
        system.thread_state(sleeper.id()).unwrap(),
        ThreadState::Blocked
    );
    assert_eq!(sleeper.wake_handle().wake(), WakeResult::Notified);
    assert_eq!(
        system.thread_state(sleeper.id()).unwrap(),
        ThreadState::Ready
    );

    support::clear_handles();
}

#[test]
fn scheduler_tick_extension_work_is_deferred_out_of_hard_irq() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    SCHEDULER_TICK_CALLBACKS.store(0, Ordering::Relaxed);

    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let gate = Arc::new(SchedulerTickGate::new());
    gate.set_enabled(true);
    let extension = unsafe {
        ThreadExtension::new(0, &TEST_EXTENSION_OPS)
            .with_scheduler_tick_work(gate, count_scheduler_tick)
    };
    system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    support::set_hard_irq(true);
    publish_scheduler_tick(1);
    support::set_hard_irq(false);
    assert_eq!(
        SCHEDULER_TICK_CALLBACKS.load(Ordering::Acquire),
        0,
        "scheduler tick work must not run in hard IRQ context"
    );
    assert!(
        system.deferred_task_work_pending(),
        "an enabled scheduler tick must publish task work"
    );
    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(SCHEDULER_TICK_CALLBACKS.load(Ordering::Acquire), 1);

    support::clear_handles();
}

#[test]
fn scheduler_tick_os_work_uses_the_latest_runqueue_clock_timestamp() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    SCHEDULER_TICK_CALLBACKS.store(0, Ordering::Relaxed);
    SCHEDULER_TICK_OBSERVED_NS.store(0, Ordering::Relaxed);

    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let gate = Arc::new(SchedulerTickGate::new());
    gate.set_enabled(true);
    let extension = unsafe {
        ThreadExtension::new(0, &TEST_EXTENSION_OPS)
            .with_scheduler_tick_work(gate, count_scheduler_tick)
    };
    system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    for monotonic_now_ns in 1..=3 {
        support::set_hard_irq(true);
        publish_scheduler_tick_at(100 + monotonic_now_ns, monotonic_now_ns);
        support::set_hard_irq(false);
    }
    assert_eq!(SCHEDULER_TICK_CALLBACKS.load(Ordering::Acquire), 0);
    assert_eq!(
        SCHEDULER_TICK_OBSERVED_NS.load(Ordering::Acquire),
        0,
        "hard IRQ must not invoke OS scheduler-tick work"
    );
    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(SCHEDULER_TICK_CALLBACKS.load(Ordering::Acquire), 1);
    assert_eq!(
        SCHEDULER_TICK_OBSERVED_NS.load(Ordering::Acquire),
        103,
        "coalesced work must receive the latest runqueue-clock boundary, not the physical IRQ \
         timestamp"
    );

    support::clear_handles();
}

#[test]
fn scheduler_tick_gate_does_not_replay_work_from_an_old_enable_epoch() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    SCHEDULER_TICK_CALLBACKS.store(0, Ordering::Relaxed);

    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let gate = Arc::new(SchedulerTickGate::new());
    gate.set_enabled(true);
    let extension = unsafe {
        ThreadExtension::new(0, &TEST_EXTENSION_OPS)
            .with_scheduler_tick_work(Arc::clone(&gate), count_scheduler_tick)
    };
    system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    publish_scheduler_tick(1);
    gate.set_enabled(false);
    gate.set_enabled(true);
    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(
        SCHEDULER_TICK_CALLBACKS.load(Ordering::Acquire),
        0,
        "a publication from an earlier enabled epoch must not cross a disable boundary"
    );

    publish_scheduler_tick(2);
    publish_scheduler_tick(3);
    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(
        SCHEDULER_TICK_CALLBACKS.load(Ordering::Acquire),
        1,
        "ticks in one enabled epoch should coalesce into one task-work callback"
    );

    support::clear_handles();
}

#[test]
fn scheduler_tick_retry_republishes_one_bounded_task_work_attempt() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    SCHEDULER_TICK_CALLBACKS.store(0, Ordering::Relaxed);
    SCHEDULER_TICK_OBSERVED_NS.store(0, Ordering::Relaxed);
    SCHEDULER_TICK_RETRIES.store(1, Ordering::Relaxed);

    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let gate = Arc::new(SchedulerTickGate::new());
    gate.set_enabled(true);
    let extension = unsafe {
        ThreadExtension::new(0, &TEST_EXTENSION_OPS)
            .with_scheduler_tick_work(gate, retry_scheduler_tick_once)
    };
    system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    publish_scheduler_tick(7);
    let first = system.service_deferred_task_work(1).unwrap();
    assert_eq!(first.processed(), 1);
    assert_eq!(first.scheduler_tick_callbacks(), 1);
    assert!(
        system.deferred_task_work_pending(),
        "a transient callback conflict must republish task work"
    );

    let second = system.service_deferred_task_work(1).unwrap();
    assert_eq!(second.processed(), 1);
    assert_eq!(second.scheduler_tick_callbacks(), 1);
    assert_eq!(SCHEDULER_TICK_CALLBACKS.load(Ordering::Acquire), 2);
    assert_eq!(SCHEDULER_TICK_OBSERVED_NS.load(Ordering::Acquire), 7);
    assert_eq!(
        system.service_deferred_task_work(1).unwrap().processed(),
        0,
        "a completed retry must leave no duplicate intrusive publication"
    );

    support::clear_handles();
}

#[test]
fn scheduler_tick_retry_defers_until_a_later_service_pass() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    SCHEDULER_TICK_CALLBACKS.store(0, Ordering::Relaxed);
    SCHEDULER_TICK_OBSERVED_NS.store(0, Ordering::Relaxed);

    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let gate = Arc::new(SchedulerTickGate::new());
    gate.set_enabled(true);
    let extension = unsafe {
        ThreadExtension::new(0, &TEST_EXTENSION_OPS)
            .with_scheduler_tick_work(Arc::clone(&gate), always_retry_scheduler_tick)
    };
    system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    publish_scheduler_tick(17);
    let first = system
        .service_deferred_task_work(ax_task::DEFAULT_BATCH_LIMIT)
        .unwrap();
    assert_eq!(
        first.scheduler_tick_callbacks(),
        1,
        "one transient conflict must not busy-retry in the same service pass"
    );
    assert_eq!(first.processed(), 1);
    assert!(system.deferred_task_work_pending());

    let second = system
        .service_deferred_task_work(ax_task::DEFAULT_BATCH_LIMIT)
        .unwrap();
    assert_eq!(second.scheduler_tick_callbacks(), 1);
    assert_eq!(second.processed(), 1);
    assert_eq!(SCHEDULER_TICK_CALLBACKS.load(Ordering::Acquire), 2);

    gate.set_enabled(false);
    let stale = system
        .service_deferred_task_work(ax_task::DEFAULT_BATCH_LIMIT)
        .unwrap();
    assert_eq!(stale.scheduler_tick_callbacks(), 0);
    assert_eq!(stale.processed(), 1);

    support::clear_handles();
}

#[test]
fn newer_tick_owns_delivery_when_it_races_a_callback_retry() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    SCHEDULER_TICK_CALLBACKS.store(0, Ordering::Relaxed);
    SCHEDULER_TICK_OBSERVED_NS.store(0, Ordering::Relaxed);
    SCHEDULER_TICK_RETRIES.store(1, Ordering::Relaxed);
    BLOCKING_TICK_ENTERED.store(false, Ordering::Release);
    RELEASE_BLOCKING_TICK.store(false, Ordering::Release);

    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let gate = Arc::new(SchedulerTickGate::new());
    gate.set_enabled(true);
    let extension = unsafe {
        ThreadExtension::new(0, &TEST_EXTENSION_OPS)
            .with_scheduler_tick_work(gate, block_then_retry_scheduler_tick_once)
    };
    system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    publish_scheduler_tick(5);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| system.service_deferred_task_work(1).unwrap());
        while !BLOCKING_TICK_ENTERED.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        publish_scheduler_tick(11);
        RELEASE_BLOCKING_TICK.store(true, Ordering::Release);
        assert_eq!(worker.join().unwrap().processed(), 1);
    });

    let second = system.service_deferred_task_work(1).unwrap();
    assert_eq!(second.processed(), 1);
    assert_eq!(second.scheduler_tick_callbacks(), 1);
    assert_eq!(SCHEDULER_TICK_CALLBACKS.load(Ordering::Acquire), 2);
    assert_eq!(
        SCHEDULER_TICK_OBSERVED_NS.load(Ordering::Acquire),
        11,
        "the newer IRQ publication must retain the timestamp watermark"
    );
    assert_eq!(
        system.service_deferred_task_work(1).unwrap().processed(),
        0,
        "the losing retry must not publish a duplicate delivery"
    );

    support::clear_handles();
}

#[test]
fn scheduler_tick_retry_cannot_cross_a_gate_disable_epoch() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    SCHEDULER_TICK_CALLBACKS.store(0, Ordering::Relaxed);
    SCHEDULER_TICK_OBSERVED_NS.store(0, Ordering::Relaxed);
    SCHEDULER_TICK_RETRIES.store(1, Ordering::Relaxed);
    BLOCKING_TICK_ENTERED.store(false, Ordering::Release);
    RELEASE_BLOCKING_TICK.store(false, Ordering::Release);

    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let gate = Arc::new(SchedulerTickGate::new());
    gate.set_enabled(true);
    let extension = unsafe {
        ThreadExtension::new(0, &TEST_EXTENSION_OPS)
            .with_scheduler_tick_work(Arc::clone(&gate), block_then_retry_scheduler_tick_once)
    };
    system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    publish_scheduler_tick(5);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| system.service_deferred_task_work(1).unwrap());
        while !BLOCKING_TICK_ENTERED.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        gate.set_enabled(false);
        gate.set_enabled(true);
        RELEASE_BLOCKING_TICK.store(true, Ordering::Release);
        assert_eq!(worker.join().unwrap().processed(), 1);
    });
    assert_eq!(
        system.service_deferred_task_work(1).unwrap().processed(),
        0,
        "Retry must not replay work from the disabled generation"
    );

    publish_scheduler_tick(13);
    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(SCHEDULER_TICK_CALLBACKS.load(Ordering::Acquire), 2);
    assert_eq!(SCHEDULER_TICK_OBSERVED_NS.load(Ordering::Acquire), 13);

    support::clear_handles();
}

#[test]
fn scheduler_tick_delivery_pins_extension_across_thread_exit() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    BLOCKING_TICK_ENTERED.store(false, Ordering::Release);
    RELEASE_BLOCKING_TICK.store(false, Ordering::Release);

    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let gate = Arc::new(SchedulerTickGate::new());
    gate.set_enabled(true);
    let extension = unsafe {
        ThreadExtension::new(0, &TEST_EXTENSION_OPS)
            .with_scheduler_tick_work(gate, block_scheduler_tick)
    };
    let bootstrap = system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
        )
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    publish_scheduler_tick(1);
    system.block_current_at(cpu.as_mut(), 1).unwrap();
    system.complete_context_switch(cpu.as_mut()).unwrap();

    let exit_result = std::thread::scope(|scope| {
        let worker = scope.spawn(|| system.service_deferred_task_work(1).unwrap());
        while !BLOCKING_TICK_ENTERED.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        let result = system.mark_exited(bootstrap.id());
        RELEASE_BLOCKING_TICK.store(true, Ordering::Release);
        assert_eq!(worker.join().unwrap().processed(), 1);
        result
    });
    assert_eq!(
        exit_result,
        Ok(()),
        "a sleepable task-work callback must not turn normal thread exit into a fatal busy error"
    );

    support::clear_handles();
}

static TEST_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: no_extension_switch_in,
    on_switch_out: no_extension_switch_out,
    on_exit: no_extension_hook,
    on_deadline_overrun: no_extension_hook,
    drop: no_extension_drop,
};

unsafe extern "Rust" fn no_extension_hook(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn no_extension_switch_in(
    _data: usize,
    _thread: ThreadId,
    _policy: SchedulePolicy,
) {
}

unsafe extern "Rust" fn no_extension_switch_out(
    _data: usize,
    _thread: ThreadId,
    _reason: ax_task::SwitchReason,
) {
}

unsafe extern "Rust" fn no_extension_drop(_data: usize) {}

unsafe extern "Rust" fn count_scheduler_tick(
    _data: usize,
    _thread: ThreadId,
    observed_ns: u64,
) -> SchedulerTickWorkDisposition {
    SCHEDULER_TICK_OBSERVED_NS.store(observed_ns, Ordering::Release);
    SCHEDULER_TICK_CALLBACKS.fetch_add(1, Ordering::Release);
    SchedulerTickWorkDisposition::Complete
}

unsafe extern "Rust" fn retry_scheduler_tick_once(
    _data: usize,
    _thread: ThreadId,
    observed_ns: u64,
) -> SchedulerTickWorkDisposition {
    SCHEDULER_TICK_OBSERVED_NS.store(observed_ns, Ordering::Release);
    SCHEDULER_TICK_CALLBACKS.fetch_add(1, Ordering::Release);
    if SCHEDULER_TICK_RETRIES
        .try_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
    {
        SchedulerTickWorkDisposition::Retry
    } else {
        SchedulerTickWorkDisposition::Complete
    }
}

unsafe extern "Rust" fn always_retry_scheduler_tick(
    _data: usize,
    _thread: ThreadId,
    observed_ns: u64,
) -> SchedulerTickWorkDisposition {
    SCHEDULER_TICK_OBSERVED_NS.store(observed_ns, Ordering::Release);
    SCHEDULER_TICK_CALLBACKS.fetch_add(1, Ordering::Release);
    SchedulerTickWorkDisposition::Retry
}

unsafe extern "Rust" fn block_then_retry_scheduler_tick_once(
    _data: usize,
    _thread: ThreadId,
    observed_ns: u64,
) -> SchedulerTickWorkDisposition {
    SCHEDULER_TICK_OBSERVED_NS.store(observed_ns, Ordering::Release);
    SCHEDULER_TICK_CALLBACKS.fetch_add(1, Ordering::Release);
    if SCHEDULER_TICK_RETRIES
        .try_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
    {
        BLOCKING_TICK_ENTERED.store(true, Ordering::Release);
        while !RELEASE_BLOCKING_TICK.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        SchedulerTickWorkDisposition::Retry
    } else {
        SchedulerTickWorkDisposition::Complete
    }
}

unsafe extern "Rust" fn block_scheduler_tick(
    _data: usize,
    _thread: ThreadId,
    _observed_ns: u64,
) -> SchedulerTickWorkDisposition {
    BLOCKING_TICK_ENTERED.store(true, Ordering::Release);
    while !RELEASE_BLOCKING_TICK.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    SchedulerTickWorkDisposition::Complete
}
