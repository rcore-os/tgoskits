use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use ax_task::{
    CpuId, FairMode, Nice, SchedulePolicy, SchedulerTickGate, TaskError, TaskSystem,
    TaskSystemConfig, ThreadExtension, ThreadExtensionOps, ThreadId, ThreadSpec, ThreadState,
    WakeResult, current_cpu_needs_resched, current_thread_extension, current_thread_id,
    on_clock_event, on_clock_event_with_scheduler_tick, schedule_current_cpu,
    take_current_expired_task_deadlines,
    timer::{ExpiredTaskDeadline, TaskDeadlineKind, TaskDeadlineNode},
};

mod support;

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static SCHEDULER_TICK_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static BLOCKING_TICK_ENTERED: AtomicBool = AtomicBool::new(false);
static RELEASE_BLOCKING_TICK: AtomicBool = AtomicBool::new(false);

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
    cpu.request_reschedule();
    assert!(current_cpu_needs_resched().unwrap());
    assert!(schedule_current_cpu().unwrap().decision().is_some());
    assert!(!current_cpu_needs_resched().unwrap());

    let sleeper = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue(cpu.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    assert_eq!(
        system.block_current(cpu.as_mut()).unwrap().next(),
        bootstrap.id()
    );
    assert_eq!(sleeper.wake_handle().wake(), WakeResult::Notified);
    let drain = system.drain_remote_wakes(cpu.as_mut(), 2).unwrap();
    assert_eq!(drain.drained(), 1);
    assert_eq!(
        system.thread_state(sleeper.id()).unwrap(),
        ThreadState::Ready
    );

    support::clear_handles();
}

#[test]
fn timer_irq_facade_bounds_and_preserves_unconsumed_expirations() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    let system = Box::pin(
        TaskSystem::new(
            TaskSystemConfig::new(1)
                .with_timer_capacity(3)
                .with_batch_limit(2),
        )
        .unwrap(),
    );
    let timers = [timer(1), timer(2), timer(3)];
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _registrations = timers
        .iter()
        .enumerate()
        .map(|(generation, node)| {
            cpu.as_mut()
                .task_deadlines()
                .arm(
                    node.as_ref(),
                    0,
                    TaskDeadlineKind::park_timeout(generation as u64 + 1),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    let first = on_clock_event(1, 2).unwrap();
    assert_eq!(first.expired(), 2);
    assert!(first.pending());
    assert!(first.update().deferred_work());
    assert!(current_cpu_needs_resched().unwrap());
    assert!(
        first.next_deadline_ns().is_none_or(|deadline| deadline > 2),
        "the overdue bounded backlog must be advanced by sticky safe-point work, not by an \
         immediate follow-up timer interrupt"
    );
    let before_drain = on_clock_event(1, 2).unwrap();
    assert_eq!(before_drain.expired(), 0);
    assert!(before_drain.pending(), "{before_drain:?}");
    assert!(before_drain.update().deferred_work());

    let mut expired = [ExpiredTaskDeadline::EMPTY; 2];
    assert_eq!(
        take_current_expired_task_deadlines(&mut expired).unwrap(),
        2
    );
    let mut owners = [
        expired[0].thread().unwrap().slot(),
        expired[1].thread().unwrap().slot(),
    ];
    support::set_monotonic_ns(1);
    let decision = schedule_current_cpu()
        .unwrap()
        .decision()
        .expect("the timer IRQ's owner preemption request must reach one scheduler decision");
    assert!(
        !decision.requires_context_switch(),
        "draining timer work with no runnable peer must preserve the current execution context"
    );
    assert!(!current_cpu_needs_resched().unwrap());
    let (_, next_deadline_ns, deferred_work) = support::last_task_deadline_update();
    assert_ne!(next_deadline_ns, 2);
    assert!(!deferred_work);
    assert!(cpu.as_mut().task_deadlines().is_empty());
    owners.sort_unstable();
    assert_ne!(owners[0], owners[1]);
    assert!(owners.iter().all(|owner| (1..=3).contains(owner)));
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
    on_clock_event_with_scheduler_tick(1, 1, true).unwrap();
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

    on_clock_event_with_scheduler_tick(1, 1, true).unwrap();
    gate.set_enabled(false);
    gate.set_enabled(true);
    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(
        SCHEDULER_TICK_CALLBACKS.load(Ordering::Acquire),
        0,
        "a publication from an earlier enabled epoch must not cross a disable boundary"
    );

    on_clock_event_with_scheduler_tick(2, 1, true).unwrap();
    on_clock_event_with_scheduler_tick(3, 1, true).unwrap();
    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(
        SCHEDULER_TICK_CALLBACKS.load(Ordering::Acquire),
        1,
        "ticks in one enabled epoch should coalesce into one task-work callback"
    );

    support::clear_handles();
}

#[test]
fn scheduler_tick_delivery_pins_extension_across_thread_exit() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    BLOCKING_TICK_ENTERED.store(false, Ordering::Relaxed);
    RELEASE_BLOCKING_TICK.store(false, Ordering::Relaxed);

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

    on_clock_event_with_scheduler_tick(1, 1, true).unwrap();
    system.block_current(cpu.as_mut()).unwrap();
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

#[test]
fn partial_deadline_drain_preserves_buffered_events() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    let system = Box::pin(
        TaskSystem::new(
            TaskSystemConfig::new(1)
                .with_timer_capacity(2)
                .with_batch_limit(2),
        )
        .unwrap(),
    );
    let timers = [timer(21), timer(22)];
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _registrations = timers
        .iter()
        .enumerate()
        .map(|(generation, node)| {
            cpu.as_mut()
                .task_deadlines()
                .arm(
                    node.as_ref(),
                    0,
                    TaskDeadlineKind::park_timeout(generation as u64 + 1),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    assert_eq!(on_clock_event(1, 2).unwrap().expired(), 2);

    let mut first = [ExpiredTaskDeadline::EMPTY; 1];
    assert_eq!(take_current_expired_task_deadlines(&mut first).unwrap(), 1);
    let mut second = [ExpiredTaskDeadline::EMPTY; 1];
    assert_eq!(
        take_current_expired_task_deadlines(&mut second).unwrap(),
        1,
        "a short consumer buffer must not discard the remaining expiration"
    );
    assert_ne!(first[0].thread(), second[0].thread());
    assert_eq!(take_current_expired_task_deadlines(&mut second).unwrap(), 0);

    support::clear_handles();
}

#[test]
fn scheduler_safe_points_finish_an_exhausted_timer_batch_without_another_irq() {
    let _test_lock = TEST_LOCK.lock().expect("facade test lock poisoned");
    support::clear_handles();
    let system = Box::pin(
        TaskSystem::new(
            TaskSystemConfig::new(1)
                .with_timer_capacity(3)
                .with_batch_limit(1),
        )
        .unwrap(),
    );
    let timers = [timer(11), timer(12), timer(13)];
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _registrations = timers
        .iter()
        .enumerate()
        .map(|(generation, node)| {
            cpu.as_mut()
                .task_deadlines()
                .arm(
                    node.as_ref(),
                    0,
                    TaskDeadlineKind::park_timeout(generation as u64 + 1),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    support::install_handles(
        (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
        cpu.as_mut(),
    );

    let irq = on_clock_event(0, 1).unwrap();
    assert_eq!(irq.expired(), 1);
    assert!(irq.pending());

    assert!(schedule_current_cpu().is_ok());
    assert!(
        current_cpu_needs_resched().unwrap(),
        "the per-CPU deadline worker must retain a sticky retry"
    );
    assert!(schedule_current_cpu().is_ok());
    assert!(
        cpu.as_mut().task_deadlines().is_empty(),
        "ordinary-context safe points must drain every due node without another timer IRQ"
    );
    assert!(!current_cpu_needs_resched().unwrap());

    support::clear_handles();
}

fn timer(slot: u32) -> Box<TaskDeadlineNode> {
    Box::new(TaskDeadlineNode::for_thread(ThreadId::from_parts(slot, 1)))
}

static TEST_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: no_extension_hook,
    on_switch_out: no_extension_switch_out,
    on_exit: no_extension_hook,
    on_deadline_overrun: no_extension_hook,
    drop: no_extension_drop,
};

unsafe extern "Rust" fn no_extension_hook(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn no_extension_switch_out(
    _data: usize,
    _thread: ThreadId,
    _reason: ax_task::SwitchReason,
) {
}

unsafe extern "Rust" fn no_extension_drop(_data: usize) {}

unsafe extern "Rust" fn count_scheduler_tick(_data: usize, _thread: ThreadId) {
    SCHEDULER_TICK_CALLBACKS.fetch_add(1, Ordering::Release);
}

unsafe extern "Rust" fn block_scheduler_tick(_data: usize, _thread: ThreadId) {
    BLOCKING_TICK_ENTERED.store(true, Ordering::Release);
    while !RELEASE_BLOCKING_TICK.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
}
