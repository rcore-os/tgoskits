use core::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

use ax_task::{
    CpuId, DeadlineFlags, DeadlinePolicy, FairMode, Nice, RtPriority, SchedulePolicy, TaskError,
    TaskSystem, TaskSystemConfig, ThreadExtension, ThreadExtensionOps, ThreadId,
    ThreadPolicyApplied, ThreadPolicyClass, ThreadSpec,
};

mod support;

static POLICY_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
static POLICY_CALLBACK_THREAD: AtomicU64 = AtomicU64::new(0);
static POLICY_CALLBACK_GENERATION: AtomicU64 = AtomicU64::new(0);
static POLICY_CALLBACK_NOW_NS: AtomicU64 = AtomicU64::new(0);
static POLICY_CALLBACK_PREVIOUS: AtomicU8 = AtomicU8::new(0);
static POLICY_CALLBACK_CURRENT: AtomicU8 = AtomicU8::new(0);

static POLICY_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: ignore_switch_in,
    on_switch_out: ignore_switch_out,
    on_policy_applied: record_policy_applied,
    on_exit: ignore_thread,
    on_deadline_overrun: ignore_thread,
    drop: ignore_drop,
};

unsafe extern "Rust" fn ignore_switch_in(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn ignore_switch_out(
    _data: usize,
    _thread: ThreadId,
    _reason: ax_task::SwitchReason,
) {
}

unsafe extern "Rust" fn record_policy_applied(
    data: usize,
    thread: ThreadId,
    event: ThreadPolicyApplied,
) {
    assert_eq!(data, 0x504F_4C49);
    POLICY_CALLBACK_THREAD.store(thread.as_u64(), Ordering::Relaxed);
    POLICY_CALLBACK_GENERATION.store(event.generation(), Ordering::Relaxed);
    POLICY_CALLBACK_NOW_NS.store(event.now_ns(), Ordering::Relaxed);
    POLICY_CALLBACK_PREVIOUS.store(event.previous_class() as u8, Ordering::Relaxed);
    POLICY_CALLBACK_CURRENT.store(event.current_class() as u8, Ordering::Relaxed);
    POLICY_CALLBACK_COUNT.fetch_add(1, Ordering::Release);
}

unsafe extern "Rust" fn ignore_thread(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn ignore_drop(_data: usize) {}

fn reset_policy_callback_record() {
    POLICY_CALLBACK_COUNT.store(0, Ordering::Relaxed);
    POLICY_CALLBACK_THREAD.store(0, Ordering::Relaxed);
    POLICY_CALLBACK_GENERATION.store(0, Ordering::Relaxed);
    POLICY_CALLBACK_NOW_NS.store(0, Ordering::Relaxed);
    POLICY_CALLBACK_PREVIOUS.store(0, Ordering::Relaxed);
    POLICY_CALLBACK_CURRENT.store(0, Ordering::Relaxed);
}

fn assert_policy_callback(
    expected_count: usize,
    thread: ThreadId,
    generation: u64,
    now_ns: u64,
    previous: ThreadPolicyClass,
    current: ThreadPolicyClass,
) {
    assert_eq!(
        POLICY_CALLBACK_COUNT.load(Ordering::Acquire),
        expected_count
    );
    assert_eq!(
        POLICY_CALLBACK_THREAD.load(Ordering::Relaxed),
        thread.as_u64()
    );
    assert_eq!(
        POLICY_CALLBACK_GENERATION.load(Ordering::Relaxed),
        generation
    );
    assert_eq!(POLICY_CALLBACK_NOW_NS.load(Ordering::Relaxed), now_ns);
    assert_eq!(
        POLICY_CALLBACK_PREVIOUS.load(Ordering::Relaxed),
        previous as u8
    );
    assert_eq!(
        POLICY_CALLBACK_CURRENT.load(Ordering::Relaxed),
        current as u8
    );
}

#[test]
fn queued_and_running_owner_policy_commits_notify_once_with_commit_epoch() {
    reset_policy_callback_record();
    let (system, mut cpu) = online_system(1);
    // SAFETY: the integer marker has no referent and every callback treats it
    // only as the fixed identity value documented by this test table.
    let extension = unsafe { ThreadExtension::new(0x504F_4C49, &POLICY_EXTENSION_OPS) };
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_extension(extension))
        .unwrap();
    system.make_ready(thread.id()).unwrap();
    system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();

    let fifo = SchedulePolicy::fifo(RtPriority::new(80).unwrap());
    system.set_thread_policy(thread.id(), fifo).unwrap();
    assert_eq!(POLICY_CALLBACK_COUNT.load(Ordering::Acquire), 0);
    system.drain_policy_updates(cpu.as_mut(), 37).unwrap();
    assert_policy_callback(
        1,
        thread.id(),
        2,
        37,
        ThreadPolicyClass::Fair,
        ThreadPolicyClass::Realtime,
    );

    assert_eq!(
        system.schedule(cpu.as_mut(), 38).unwrap().next(),
        thread.id()
    );
    let fair = SchedulePolicy::fair(Nice::new(5).unwrap(), FairMode::Batch);
    system.set_thread_policy(thread.id(), fair).unwrap();
    assert_eq!(POLICY_CALLBACK_COUNT.load(Ordering::Acquire), 1);
    system.drain_policy_updates(cpu.as_mut(), 61).unwrap();
    assert_policy_callback(
        2,
        thread.id(),
        3,
        61,
        ThreadPolicyClass::Realtime,
        ThreadPolicyClass::Fair,
    );
    assert_eq!(thread.effective_policy(), fair);
}

#[test]
fn queued_policy_update_is_applied_by_the_runqueue_owner() {
    let (system, mut cpu) = online_system(1);
    let fair = ready_thread(&system, SchedulePolicy::default());
    let promoted = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), fair.id(), 0).unwrap();
    system.enqueue(cpu.as_mut(), promoted.id(), 0).unwrap();

    let fifo = SchedulePolicy::fifo(RtPriority::new(80).unwrap());
    system.set_thread_policy(promoted.id(), fifo).unwrap();
    assert_eq!(promoted.policy(), fifo);
    assert_eq!(promoted.effective_policy(), SchedulePolicy::default());

    assert_eq!(
        system
            .drain_policy_updates(cpu.as_mut(), 1)
            .unwrap()
            .drained(),
        1
    );
    assert_eq!(promoted.effective_policy(), fifo);
    assert_eq!(
        system.schedule(cpu.as_mut(), 1).unwrap().next(),
        promoted.id()
    );
}

#[test]
fn less_urgent_queued_policy_update_does_not_force_preemption() {
    let (system, mut cpu) = online_system(1);
    let running = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(80).unwrap()));
    let queued = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(70).unwrap()));
    system.enqueue(cpu.as_mut(), running.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        running.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    system.enqueue(cpu.as_mut(), queued.id(), 0).unwrap();

    let lower = SchedulePolicy::fifo(RtPriority::new(60).unwrap());
    system.set_thread_policy(queued.id(), lower).unwrap();

    assert!(
        system
            .schedule_if_requested(cpu.as_mut(), 1)
            .unwrap()
            .is_quiescent()
    );
    assert_eq!(cpu.current(), Some(running.id()));
    assert_eq!(queued.effective_policy(), lower);
}

#[test]
fn running_policy_update_survives_old_dispatch_commit() {
    let (system, mut cpu) = online_system(1);
    let running = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), running.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 0).unwrap().next(),
        running.id()
    );
    system.charge_current(cpu.as_mut(), 7, 7, 0).unwrap();

    let fifo = SchedulePolicy::fifo(RtPriority::new(80).unwrap());
    system.set_thread_policy(running.id(), fifo).unwrap();
    system.drain_policy_updates(cpu.as_mut(), 7).unwrap();
    assert_eq!(running.effective_policy(), fifo);

    let lower = ready_thread(&system, SchedulePolicy::fifo(RtPriority::new(70).unwrap()));
    system.enqueue(cpu.as_mut(), lower.id(), 7).unwrap();
    assert_eq!(
        system.yield_current(cpu.as_mut(), 8).unwrap().next(),
        running.id()
    );
}

#[test]
fn remote_running_policy_update_is_delivered_to_its_owner_cpu() {
    support::clear_handles();
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    support::install_handles(
        (&system as *const TaskSystem).expose_provenance(),
        cpu0.as_mut(),
    );
    support::install_cpu(1, cpu1.as_mut());
    support::set_online_cpu_count(2);

    let running = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu1.as_mut(), running.id(), 0).unwrap();
    assert_eq!(
        system.schedule(cpu1.as_mut(), 0).unwrap().next(),
        running.id()
    );

    let fifo = SchedulePolicy::fifo(RtPriority::new(60).unwrap());
    system.set_thread_policy(running.id(), fifo).unwrap();
    assert!(cpu1.has_remote_work());
    assert_eq!(support::ipi_count(1), 1);
    assert_eq!(
        system
            .drain_policy_updates(cpu1.as_mut(), 1)
            .unwrap()
            .drained(),
        1
    );
    assert_eq!(running.effective_policy(), fifo);
    support::clear_handles();
}

#[test]
fn owner_applies_deadline_to_fair_and_fair_to_deadline_transitions() {
    let (system, mut cpu) = online_system(1);
    let thread = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();

    let deadline =
        SchedulePolicy::deadline(DeadlinePolicy::new(2, 5, 10, DeadlineFlags::NONE).unwrap());
    system.set_thread_policy(thread.id(), deadline).unwrap();
    system.drain_policy_updates(cpu.as_mut(), 3).unwrap();
    assert_eq!(
        system.schedule(cpu.as_mut(), 3).unwrap().next(),
        thread.id()
    );
    assert_eq!(thread.effective_policy(), deadline);
    assert_eq!(
        system
            .deadline_runtime(thread.id())
            .unwrap()
            .remaining_runtime_ns(),
        2
    );

    let fair = SchedulePolicy::fair(Nice::new(5).unwrap(), FairMode::Normal);
    system.set_thread_policy(thread.id(), fair).unwrap();
    system.drain_policy_updates(cpu.as_mut(), 4).unwrap();
    assert_eq!(thread.effective_policy(), fair);
    assert_eq!(
        system.deadline_runtime(thread.id()),
        Err(TaskError::InvalidConfiguration)
    );
}

#[test]
fn coalesced_stale_message_applies_only_the_latest_policy_generation() {
    let (system, mut cpu) = online_system(1);
    let thread = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();

    let stale = SchedulePolicy::fifo(RtPriority::new(90).unwrap());
    let latest = SchedulePolicy::fair(Nice::new(10).unwrap(), FairMode::Batch);
    system.set_thread_policy(thread.id(), stale).unwrap();
    system.set_thread_policy(thread.id(), latest).unwrap();
    assert_eq!(thread.policy(), latest);
    assert_eq!(thread.effective_policy(), SchedulePolicy::default());

    assert_eq!(
        system
            .drain_policy_updates(cpu.as_mut(), 1)
            .unwrap()
            .drained(),
        1
    );
    assert_eq!(thread.effective_policy(), latest);
}

#[test]
fn exited_thread_waits_for_in_flight_policy_delivery() {
    let (system, mut cpu) = online_system(1);
    let thread = ready_thread(&system, SchedulePolicy::default());
    let thread_id = thread.id();
    system.enqueue(cpu.as_mut(), thread_id, 0).unwrap();

    system
        .set_thread_policy(
            thread_id,
            SchedulePolicy::fifo(RtPriority::new(80).unwrap()),
        )
        .unwrap();
    system.dequeue(cpu.as_mut(), thread_id).unwrap();
    system.mark_exited(thread_id).unwrap();
    drop(thread);

    assert_eq!(
        system
            .service_deferred_task_work(ax_task::DEFAULT_BATCH_LIMIT)
            .unwrap()
            .processed(),
        0,
        "an inbox-held policy delivery must pin registry-owned resources"
    );
    system.drain_policy_updates(cpu.as_mut(), 1).unwrap();
    assert_eq!(
        system
            .service_deferred_task_work(ax_task::DEFAULT_BATCH_LIMIT)
            .unwrap()
            .processed(),
        1
    );
    assert_eq!(
        system.thread_state(thread_id),
        Err(TaskError::StaleThreadId)
    );
}

#[test]
fn exited_thread_rejects_policy_and_affinity_mutation() {
    let (system, _cpu) = online_system(1);
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.mark_exited(thread.id()).unwrap();

    assert_eq!(
        system.set_thread_policy(
            thread.id(),
            SchedulePolicy::fifo(RtPriority::new(80).unwrap()),
        ),
        Err(TaskError::NotReady)
    );
    assert_eq!(
        system.set_affinity(thread.id(), ax_task::CpuSet::all(1)),
        Err(TaskError::NotReady)
    );
}

#[test]
fn pending_deadline_to_fair_update_keeps_active_admission_reserved() {
    let (system, mut cpu) = online_system(1);
    let active = ready_thread(&system, deadline(90, 100));
    system.enqueue(cpu.as_mut(), active.id(), 0).unwrap();

    system
        .set_thread_policy(active.id(), SchedulePolicy::default())
        .unwrap();
    assert!(matches!(
        system.create_thread(ThreadSpec::new(deadline(10, 100))),
        Err(TaskError::DeadlineAdmission)
    ));

    system.drain_policy_updates(cpu.as_mut(), 1).unwrap();
    system
        .create_thread(ThreadSpec::new(deadline(10, 100)))
        .unwrap();
}

#[test]
fn pending_deadline_reduction_releases_admission_only_after_owner_apply() {
    let (system, mut cpu) = online_system(1);
    let active = ready_thread(&system, deadline(90, 100));
    system.enqueue(cpu.as_mut(), active.id(), 0).unwrap();

    system
        .set_thread_policy(active.id(), deadline(50, 100))
        .unwrap();
    assert!(matches!(
        system.create_thread(ThreadSpec::new(deadline(45, 100))),
        Err(TaskError::DeadlineAdmission)
    ));

    system.drain_policy_updates(cpu.as_mut(), 1).unwrap();
    system
        .create_thread(ThreadSpec::new(deadline(45, 100)))
        .unwrap();
}

#[test]
fn pending_fair_to_deadline_update_reserves_before_owner_apply() {
    let (system, mut cpu) = online_system(1);
    let active = ready_thread(&system, SchedulePolicy::default());
    system.enqueue(cpu.as_mut(), active.id(), 0).unwrap();

    system
        .set_thread_policy(active.id(), deadline(90, 100))
        .unwrap();
    assert!(matches!(
        system.create_thread(ThreadSpec::new(deadline(10, 100))),
        Err(TaskError::DeadlineAdmission)
    ));

    // The policy inbox carries an intrusive Arc publication that is normally
    // consumed by the owner CPU at its next scheduler safe point. Complete
    // that ownership transfer before the isolated fixture is torn down so the
    // test does not strand the publication outside TaskSystem's registry.
    assert_eq!(
        system
            .drain_policy_updates(cpu.as_mut(), 1)
            .unwrap()
            .drained(),
        1
    );
}

fn online_system(cpu_count: usize) -> (TaskSystem, core::pin::Pin<Box<ax_task::CpuLocal>>) {
    support::clear_handles();
    let system = TaskSystem::new(TaskSystemConfig::new(cpu_count)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    (system, cpu)
}

fn ready_thread(system: &TaskSystem, policy: SchedulePolicy) -> ax_task::ThreadHandle {
    let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
    system.make_ready(thread.id()).unwrap();
    thread
}

fn deadline(runtime_ns: u64, period_ns: u64) -> SchedulePolicy {
    SchedulePolicy::deadline(
        DeadlinePolicy::new(runtime_ns, period_ns, period_ns, DeadlineFlags::NONE).unwrap(),
    )
}
