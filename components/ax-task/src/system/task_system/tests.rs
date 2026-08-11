use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::*;
use crate::{FairEntity, PiMutexAcquire, PiMutexClaimOutcome, PiMutexCore, PiMutexLockResult};

trait TaskSystemClockTestExt {
    fn enqueue_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
        now_ns: u64,
    ) -> Result<(), TaskError>;

    fn place_ready_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
        now_ns: u64,
    ) -> Result<(), TaskError>;

    fn bring_cpu_online_at(&self, cpu: Pin<&mut CpuLocal>, now_ns: u64) -> Result<(), TaskError>;

    fn charge_current_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
        runtime_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<ChargeOutcome, TaskError>;

    fn charge_current_until_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<ChargeOutcome, TaskError>;

    fn schedule_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError>;

    fn schedule_if_requested_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<SchedulerOutcome, TaskError>;

    fn yield_current_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError>;

    fn block_current_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError>;

    fn exit_current_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError>;

    fn commit_park_at_for_test(
        &self,
        cpu: Pin<&mut CpuLocal>,
        token: &mut ParkTicket,
        now_ns: u64,
    ) -> Result<ParkCommit, TaskError>;

    fn drain_owner_control_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<OwnerControlDrain, TaskError>;
}

#[test]
fn cpu_online_transition_samples_the_owner_rq_clock_once() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();

    crate::test_runtime::reset_scheduler_reads();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    assert_eq!(
        crate::test_runtime::scheduler_reads(),
        1,
        "one rq state transition must use one clock sample"
    );
}

#[test]
fn rt_period_observes_rq_throttle_after_an_optimistic_empty_snapshot() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online_at(cpu.as_mut(), 0).unwrap();

    // Model the interleaving where the period owner first observes an empty
    // runtime ledger, then the rq owner publishes the throttle transition and
    // removes the last RT entity before the period owner acquires the rq lock.
    cpu.lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .set_rt_throttled(true);
    assert!(
        system
            .root_domain
            .activate_rt_period(CpuId::new(0), MonotonicInstant::from_nanos(0).unwrap())
    );

    assert!(!system.service_rt_period(
        cpu.as_ref().get_ref(),
        MonotonicInstant::from_nanos(1_000_000_000).unwrap(),
    ));
    assert!(
        !cpu.lock_run_queue(crate::RunQueueGuardSource::TestInspection)
            .rt_is_throttled(),
        "the rq-owned throttle fact must participate in the period fast-path decision"
    );
}

#[test]
fn offline_bootstrap_rq_setup_does_not_enter_runtime_irq_service() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();

    crate::test_runtime::reset_irq_guard_entries();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();

    assert_eq!(
        crate::test_runtime::irq_guard_entries(),
        0,
        "Linux sched_init uses the already IRQ-off boot owner, not IRQ-exit service hooks",
    );
}

#[test]
fn offline_bootstrap_accepts_only_the_linux_boot_fair_class() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();

    assert!(matches!(
        system.install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fifo(RtPriority::new(1).unwrap())),
        ),
        Err(TaskError::InvalidConfiguration)
    ));
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
}

#[test]
fn block_transition_samples_the_owner_rq_clock_once() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    crate::test_runtime::reset_scheduler_reads();
    let _decision = system.block_current_at(cpu.as_mut(), 1).unwrap();

    assert_eq!(
        crate::test_runtime::scheduler_reads(),
        1,
        "park preparation and rq commit are one scheduling transition"
    );
}

#[test]
fn ordinary_switch_tail_does_not_reopen_the_owner_runqueue() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    system.block_current_at(cpu.as_mut(), 1).unwrap();
    crate::test_runtime::reset_scheduler_reads();

    system.complete_context_switch(cpu.as_mut()).unwrap();

    assert_eq!(
        crate::test_runtime::scheduler_reads(),
        0,
        "Linux finish_task_switch releases the staged on_cpu claim without opening a new rq \
         transaction",
    );
}

impl TaskSystemClockTestExt for TaskSystem {
    fn enqueue_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        crate::test_runtime::set_scheduler_ns_for_cpu(cpu.owner().as_u32(), now_ns);
        self.enqueue(cpu, thread)
    }

    fn place_ready_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        crate::test_runtime::set_scheduler_ns_for_cpu(cpu.owner().as_u32(), now_ns);
        self.place_ready(cpu, thread)
    }

    fn bring_cpu_online_at(&self, cpu: Pin<&mut CpuLocal>, now_ns: u64) -> Result<(), TaskError> {
        crate::test_runtime::set_scheduler_ns_for_cpu(cpu.owner().as_u32(), now_ns);
        self.bring_cpu_online(cpu)
    }

    fn charge_current_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
        runtime_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<ChargeOutcome, TaskError> {
        crate::test_runtime::set_scheduler_ns_for_cpu(cpu.owner().as_u32(), now_ns);
        self.charge_current(cpu, runtime_ns, reclaimed_ns)
    }

    fn charge_current_until_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<ChargeOutcome, TaskError> {
        crate::test_runtime::set_scheduler_ns_for_cpu(cpu.owner().as_u32(), now_ns);
        self.charge_current_until(cpu, reclaimed_ns)
    }

    fn schedule_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError> {
        crate::test_runtime::set_scheduler_ns_for_cpu(cpu.owner().as_u32(), now_ns);
        self.schedule(cpu)
    }

    fn schedule_if_requested_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<SchedulerOutcome, TaskError> {
        crate::test_runtime::set_scheduler_ns_for_cpu(cpu.owner().as_u32(), now_ns);
        self.schedule_if_requested(cpu)
    }

    fn yield_current_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError> {
        crate::test_runtime::set_scheduler_ns_for_cpu(cpu.owner().as_u32(), now_ns);
        self.yield_current(cpu)
    }

    fn block_current_at(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError> {
        crate::test_runtime::set_scheduler_ns_for_cpu(cpu.owner().as_u32(), now_ns);
        let ParkPrepare::Prepared(mut ticket) = self.prepare_park(cpu.as_mut())? else {
            panic!("isolated block fixture unexpectedly consumed a preceding notification")
        };
        match self.commit_park(cpu, &mut ticket)? {
            ParkCommit::Blocked(decision) => Ok(decision),
            ParkCommit::Notified => {
                panic!("isolated block fixture unexpectedly raced with a notification")
            }
        }
    }

    fn exit_current_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError> {
        crate::test_runtime::set_scheduler_ns_for_cpu(cpu.owner().as_u32(), now_ns);
        self.exit_current(cpu)
    }

    fn commit_park_at_for_test(
        &self,
        cpu: Pin<&mut CpuLocal>,
        token: &mut ParkTicket,
        now_ns: u64,
    ) -> Result<ParkCommit, TaskError> {
        crate::test_runtime::set_scheduler_ns_for_cpu(cpu.owner().as_u32(), now_ns);
        self.commit_park(cpu, token)
    }

    fn drain_owner_control_at(
        &self,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<OwnerControlDrain, TaskError> {
        crate::test_runtime::set_scheduler_ns_for_cpu(cpu.owner().as_u32(), now_ns);
        self.drain_owner_control(cpu)
    }
}

fn commit_pi_wait(
    system: &TaskSystem,
    lock: &PiMutexCore,
    waiter: ThreadId,
    owner: ThreadId,
) -> Result<PiWaitToken, TaskError> {
    if !lock.is_owned_by(owner.into()) {
        if acquire_pi_for_thread(lock, owner)? != PiMutexAcquire::Acquired {
            return Err(TaskError::InvalidPiState);
        }
    }
    match system.pi_mutex_lock_slow(lock.mutex_ref()?, waiter, waiter.as_u64())? {
        PiMutexLockResult::Waiting(token) => Ok(token),
        PiMutexLockResult::Acquired => Err(TaskError::InvalidPiState),
    }
}

fn create_online_pi_cpu(system: &TaskSystem) -> Pin<Box<CpuLocal>> {
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    cpu
}

fn place_pi_owner(system: &TaskSystem, mut cpu: Pin<&mut CpuLocal>, owner: &ThreadHandle) {
    system.make_ready(owner.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), owner.id(), 0).unwrap();
}

fn acquire_pi_for_thread(
    lock: &PiMutexCore,
    thread: ThreadId,
) -> Result<PiMutexAcquire, TaskError> {
    // SAFETY: these unit tests own the complete modeled scheduler and raw PI
    // mutex state and serialize every explicit owner transition.
    unsafe { Ok(lock.try_acquire_for_thread(thread)?) }
}

fn release_pi_for_thread(lock: &PiMutexCore, thread: ThreadId) -> Result<bool, TaskError> {
    // SAFETY: these unit tests release only an explicit owner installed by the
    // same single-threaded scheduler fixture.
    unsafe { Ok(lock.try_release_for_thread(thread)?) }
}

fn publish_test_scheduler_work(
    remote: &CpuRemote,
    node: Pin<&'static crate::inbox::InboxNode>,
    slot: u32,
) {
    let message = InboxMessage::migration(
        ThreadId::from_parts(slot, 1),
        remote.owner(),
        remote.owner(),
        u64::from(slot),
        1_024,
    );
    let result = remote.publish_owner_control(node, message);
    assert_eq!(result, PublishResult::Published);
}

fn test_inbox_node(
    node: &Pin<Box<crate::inbox::InboxNode>>,
) -> Pin<&'static crate::inbox::InboxNode> {
    let node = node.as_ref().get_ref() as *const crate::inbox::InboxNode;
    unsafe {
        // Callers keep the pinned fixture alive until its inbox has drained
        // or the complete owning task system has been dropped.
        Pin::new_unchecked(&*node)
    }
}

#[test]
fn runqueue_clock_ignores_a_backward_scheduler_source_sample() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(thread.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), thread.id(), 100).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 100).unwrap().next(),
        thread.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();

    let charge = system.charge_current_until_at(cpu.as_mut(), 90, 0).unwrap();
    assert!(!charge.slice_expired());
    assert!(!charge.deadline_overrun());
}

#[test]
fn remote_deadline_wake_uses_the_target_runqueue_clock() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system
        .register_idle_thread(
            cpu0.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    let idle1 = system
        .register_idle_thread(
            cpu1.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online_at(cpu0.as_mut(), 0).unwrap();
    system.bring_cpu_online_at(cpu1.as_mut(), 0).unwrap();

    let deadline = system
        .create_thread(ThreadSpec::new(SchedulePolicy::deadline(
            DeadlinePolicy::new(2, 5, 10, DeadlineFlags::NONE).unwrap(),
        )))
        .unwrap();
    system.make_ready(deadline.id()).unwrap();
    system.enqueue_at(cpu1.as_mut(), deadline.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 0).unwrap().next(),
        deadline.id()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();
    assert_eq!(
        system.block_current_at(cpu1.as_mut(), 1).unwrap().next(),
        idle1.id()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();

    crate::test_runtime::set_scheduler_ns_for_cpu(0, 100);
    crate::test_runtime::set_scheduler_ns_for_cpu(1, 10);
    assert_eq!(
        system.wake_thread_direct(Arc::clone(&deadline.core), Some(CpuId::new(1))),
        crate::WakeResult::Notified
    );
    let absolute_deadline = cpu1
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .scheduling_entity(deadline.id())
        .expect("the woken Deadline entity must be owned by the target rq")
        .deadline()
        .and_then(DeadlineEntity::absolute_deadline_ns);
    assert_eq!(absolute_deadline, Some(15));
}

#[test]
fn scheduler_safe_point_does_not_expire_a_future_monotonic_deadline() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online_at(cpu.as_mut(), 0).unwrap();

    let timer_owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let registration = cpu
        .remote()
        .lock_deadline_activity(DeadlineBaseGuardSource::TestInspection)
        .queue
        .arm(
            timer_owner.sleep_timer(),
            MonotonicDeadline::from_nanos(50).unwrap(),
            TaskDeadlineKind::park_timeout(1),
        )
        .unwrap();

    crate::test_runtime::set_monotonic_ns(10);
    system.schedule_at(cpu.as_mut(), 1_000).unwrap();

    assert!(
        cpu.remote()
            .lock_deadline_activity(DeadlineBaseGuardSource::TestInspection)
            .queue
            .cancel(&registration),
        "rq clock 1000 must not be interpreted as monotonic time past deadline 50"
    );
}

#[test]
fn cpu_owners_schedule_while_cold_domains_are_locked() {
    use std::{
        sync::{Barrier, mpsc},
        thread,
        time::Duration,
    };

    let system = Arc::new(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let ready = Arc::new(Barrier::new(3));
    let start = Arc::new(Barrier::new(3));
    let (completed, progress) = mpsc::channel();
    let mut workers = Vec::new();

    for cpu_index in 0..2 {
        let system = Arc::clone(&system);
        let ready = Arc::clone(&ready);
        let start = Arc::clone(&start);
        let completed = completed.clone();
        workers.push(thread::spawn(move || {
            let mut cpu = system.create_cpu_local(CpuId::new(cpu_index)).unwrap();
            system
                .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
                .unwrap();
            system.bring_cpu_online(cpu.as_mut()).unwrap();
            ready.wait();
            start.wait();
            let result = system
                .drain_owner_control_at(cpu.as_mut(), 1)
                .and_then(|_| system.schedule_at(cpu.as_mut(), 1).map(|_| ()));
            completed.send((cpu_index, result)).unwrap();
        }));
    }
    drop(completed);

    ready.wait();
    let registry = system.state.lock();
    let root_domain = system.root_domain.lock();
    start.wait();
    let mut observed = Vec::new();
    let mut timed_out = false;
    for _ in 0..2 {
        match progress.recv_timeout(Duration::from_secs(2)) {
            Ok(result) => observed.push(result),
            Err(_) => {
                timed_out = true;
                break;
            }
        }
    }
    drop(root_domain);
    drop(registry);
    for worker in workers {
        worker.join().unwrap();
    }

    assert!(!timed_out, "owner scheduling waited for a cold lock domain");
    assert_eq!(observed.len(), 2);
    for (_, result) in observed {
        result.unwrap();
    }
}

#[test]
fn current_extension_lookup_progresses_while_registry_is_locked() {
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let extension = unsafe { ThreadExtension::new(0x55, &DEADLINE_TEST_EXTENSION_OPS) };
    system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

    let registry = system.state.lock();
    let lease = crate::current_thread_extension().unwrap().unwrap();
    assert_eq!(lease.data(), 0x55);
    assert!(core::ptr::eq(lease.ops(), &DEADLINE_TEST_EXTENSION_OPS));
    drop(registry);
}

#[test]
fn remote_publication_cannot_be_preempted_before_doorbell() {
    let node = Box::pin(crate::inbox::InboxNode::new(InboxKind::OwnerControl));
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let remote = &system.cpu_remotes[1];
    assert!(remote.mark_online());
    crate::test_runtime::reset_irq_state();
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);

    publish_test_scheduler_work(remote, test_inbox_node(&node), 1);

    assert_eq!(crate::test_runtime::scheduler_ipi_send_count(), 1);
    assert_eq!(
        crate::test_runtime::scheduler_ipi_irq_guards(),
        1,
        "a producer must remain non-preemptible until its published work has a doorbell"
    );
}

#[test]
fn same_cpu_hard_irq_publication_uses_irq_return_instead_of_a_self_ipi() {
    let node = Box::pin(crate::inbox::InboxNode::new(InboxKind::OwnerControl));
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    let remote = &system.cpu_remotes[0];
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    crate::test_runtime::reset_local_scheduler_work_publications();

    crate::test_runtime::set_hard_irq(true);
    publish_test_scheduler_work(remote, test_inbox_node(&node), 1);
    crate::test_runtime::set_hard_irq(false);

    let scheduler_ipi_send_count = crate::test_runtime::scheduler_ipi_send_count();
    let drained = system.drain_owner_control_at(cpu.as_mut(), 1).unwrap();
    assert_eq!(drained.drained(), 1);
    assert_eq!(
        scheduler_ipi_send_count, 0,
        "same-CPU hard-IRQ work must run from IRQ return without a self-IPI round trip"
    );
    assert_eq!(
        crate::test_runtime::local_scheduler_work_publications(),
        1,
        "suppressing the self-IPI must first publish need_resched to the IRQ-return owner"
    );
}

#[test]
fn same_cpu_hard_irq_wake_is_runnable_at_the_irq_return_safe_point() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let bootstrap = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let service = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(1).unwrap(),
        )))
        .unwrap();
    system.make_ready(service.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), service.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 1).unwrap().next(),
        service.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert_eq!(
        system.block_current_at(cpu.as_mut(), 2).unwrap().next(),
        bootstrap.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();

    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    crate::test_runtime::reset_local_scheduler_work_publications();
    let cell = crate::IrqWaitCell::new();
    let registration = crate::IrqWaitRegistration::new(service.wake_handle());
    let token = match cell.register(&registration) {
        crate::IrqRegisterResult::Registered(token) => token,
        other => panic!("fresh hard-IRQ registration failed: {other:?}"),
    };

    crate::test_runtime::set_hard_irq(true);
    assert_eq!(cell.notify(), crate::IrqNotifyResult::Notified);
    crate::test_runtime::set_hard_irq(false);

    assert_eq!(
        system.thread_state(service.id()).unwrap(),
        ThreadState::Ready
    );
    let outcome = system.schedule_if_requested_at(cpu.as_mut(), 3).unwrap();
    crate::quiesce_irq_wait(token).unwrap();

    assert_eq!(outcome.decision().unwrap().next(), service.id());
    assert_eq!(
        system.thread_state(service.id()).unwrap(),
        ThreadState::Running
    );
    assert_eq!(crate::test_runtime::scheduler_ipi_send_count(), 0);
    assert_eq!(
        crate::test_runtime::local_scheduler_work_publications(),
        1,
        "same-CPU IRQ completion must publish the IRQ-return reschedule edge"
    );
}

#[test]
fn fair_service_thread_woken_from_irq_preempts_without_waiting_for_timer() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let bootstrap = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    assert!(
        system
            .charge_current_at(cpu.as_mut(), 20_000_000, 20_000_000, 0)
            .unwrap()
            .slice_expired()
    );
    let service = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(service.id()).unwrap();
    system
        .enqueue_at(cpu.as_mut(), service.id(), 20_000_000)
        .unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 20_000_000).unwrap().next(),
        service.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    *cpu.lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .current_mut()
        .unwrap()
        .active_mut()
        .entity_mut() = SchedulingEntity::Fair(FairEntity::test_state(
        Nice::ZERO,
        FairMode::Normal,
        19_000_000,
        19_500_000,
    ));
    assert_eq!(
        system
            .block_current_at(cpu.as_mut(), 20_000_000)
            .unwrap()
            .next(),
        bootstrap.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert_eq!(
        service
            .core
            .sched()
            .lock()
            .policy
            .active()
            .entity()
            .fair()
            .unwrap()
            .saved_sleep_lag(),
        Some(500_000)
    );

    let current_entity =
        FairEntity::test_state(Nice::ZERO, FairMode::Normal, 20_000_000, 20_400_000);
    *cpu.lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .current_mut()
        .unwrap()
        .active_mut()
        .entity_mut() = SchedulingEntity::Fair(current_entity);
    cpu.lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .update_fair_virtual_time(Some(current_entity));
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    let cell = crate::IrqWaitCell::new();
    let registration = crate::IrqWaitRegistration::new(service.wake_handle());
    let token = match cell.register(&registration) {
        crate::IrqRegisterResult::Registered(token) => token,
        other => panic!("fresh fair service registration failed: {other:?}"),
    };

    crate::test_runtime::set_hard_irq(true);
    assert_eq!(cell.notify(), crate::IrqNotifyResult::Notified);
    crate::test_runtime::set_hard_irq(false);
    assert_eq!(
        system.thread_state(service.id()).unwrap(),
        ThreadState::Ready
    );
    let queued = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .scheduling_entity(service.id())
        .expect("the IRQ-woken service must be linked in its owner rq")
        .fair()
        .unwrap();
    assert_eq!(queued.vruntime(), 19_000_000);
    assert_eq!(queued.virtual_deadline(), 19_000_001);
    let outcome = system
        .schedule_if_requested_at(cpu.as_mut(), 20_000_001)
        .unwrap();
    crate::quiesce_irq_wait(token).unwrap();

    assert_eq!(
        outcome.decision().map(|decision| decision.next()),
        Some(service.id()),
        "an IRQ-woken positive-lag service thread must run at IRQ return, not at a later timer"
    );
    assert!(
        !cpu.remote().needs_reschedule(),
        "put_prev_task must not re-publish the outgoing fair current as a new wakeup: {:?}",
        cpu.remote().scheduler_request_state_for_test(),
    );
}

#[test]
fn eligible_fair_current_keeps_latest_eevdf_slice_protection_on_wake() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let current = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 900, 1_200);
    let woken = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 800, 1_000);
    let mut run_queue = cpu.lock_run_queue(crate::RunQueueGuardSource::TestInspection);
    let dispatch = run_queue.current_mut().unwrap();
    *dispatch.active_mut().entity_mut() = SchedulingEntity::Fair(current);

    assert!(current.is_eligible(1_000));
    assert!(woken.deadline_precedes(current));
    assert!(
        !dispatch.should_preempt(
            SchedulingEntity::Fair(current),
            SchedulePolicy::default(),
            SchedulingEntity::Fair(woken),
            1_000,
        ),
        "latest EEVDF keeps an eligible current request protected until its request boundary"
    );
}

#[test]
fn same_cpu_task_publication_uses_guard_exit_instead_of_a_self_ipi() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new_task_context(system.as_ref(), cpu.as_mut());
    let remote = &system.cpu_remotes[0];
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    crate::test_runtime::reset_scheduler_frame_state();
    crate::test_runtime::configure_irq_exit_schedule_reentry(1);
    assert_eq!(crate::test_runtime::active_irq_guards(), 0);

    assert!(remote.kick_scheduler_work());
    assert_eq!(crate::test_runtime::active_irq_guards(), 0);

    crate::test_runtime::configure_irq_exit_schedule_reentry(0);
    assert_eq!(
        crate::test_runtime::scheduler_ipi_send_count(),
        0,
        "same-CPU task work must enter the scheduler from the final publication guard exit"
    );
    assert_eq!(
        crate::test_runtime::scheduler_frame_state().1,
        1,
        "the final publication guard exit must enter exactly one local scheduler frame"
    );
}

#[test]
fn same_cpu_task_wake_activates_the_owner_runqueue_directly() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let bootstrap = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let sleeper = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert_eq!(
        system.block_current_at(cpu.as_mut(), 0).unwrap().next(),
        bootstrap.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();

    let _runtime_handles = InstalledTaskHandles::new_task_context(system.as_ref(), cpu.as_mut());
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    assert_eq!(
        sleeper.wake_handle().wake_from_task(),
        crate::WakeResult::Notified
    );

    assert_eq!(
        crate::test_runtime::scheduler_ipi_send_count(),
        0,
        "same-CPU task wake must not send a scheduler IPI"
    );
    assert_eq!(
        system.thread_state(sleeper.id()).unwrap(),
        ThreadState::Ready
    );
    let token = task_runtime::irq_guard_enter();
    assert_eq!(
        system.snapshot(cpu.as_ref()).unwrap().runnable(),
        2,
        "Linux rq->nr_running includes both the non-idle current and queued wakee"
    );
    // SAFETY: this consumes the task-context owner token on the same host
    // thread after the snapshot borrow has ended.
    unsafe { task_runtime::irq_guard_exit(token) };
}

#[test]
fn same_cpu_task_irq_cell_notification_activates_the_owner_runqueue_directly() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let bootstrap = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let sleeper = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert_eq!(
        system.block_current_at(cpu.as_mut(), 0).unwrap().next(),
        bootstrap.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();

    let _runtime_handles = InstalledTaskHandles::new_task_context(system.as_ref(), cpu.as_mut());
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    let cell = crate::IrqWaitCell::new();
    let registration = crate::IrqWaitRegistration::new(sleeper.wake_handle());
    let token = match cell.register(&registration) {
        crate::IrqRegisterResult::Registered(token) => token,
        other => panic!("fresh task notification registration failed: {other:?}"),
    };

    let notified = cell.notify_from_task();
    let sleeper_state = system.thread_state(sleeper.id()).unwrap();
    token.detach().try_finish().unwrap();

    assert_eq!(notified, crate::IrqNotifyResult::Notified);
    assert_eq!(sleeper_state, ThreadState::Ready);
}

#[test]
fn guarded_same_cpu_task_wake_uses_the_irq_safe_runqueue() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let bootstrap = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let sleeper = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert_eq!(
        system.block_current_at(cpu.as_mut(), 0).unwrap().next(),
        bootstrap.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();

    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    assert_eq!(
        sleeper.wake_handle().wake_from_task(),
        crate::WakeResult::Notified
    );

    assert_eq!(
        system.thread_state(sleeper.id()).unwrap(),
        ThreadState::Ready,
        "the irqsave thread/runqueue transaction must complete before wake returns"
    );
}

#[test]
fn direct_wake_activates_the_target_runqueue_before_its_owner_safe_point() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }
    let mut cpu1_only = CpuSet::empty(2);
    assert!(cpu1_only.insert(CpuId::new(1)));
    let sleeper = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_affinity(cpu1_only))
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu1.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();
    let _cpu1_bootstrap = system.block_current_at(cpu1.as_mut(), 2).unwrap().next();
    system.complete_context_switch(cpu1.as_mut()).unwrap();

    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu0.as_mut());
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    assert_eq!(sleeper.wake_handle().wake(), crate::WakeResult::Notified);

    assert_eq!(
        system.thread_state(sleeper.id()).unwrap(),
        ThreadState::Ready,
        "PREEMPT_RT wakeup must activate under the target runqueue lock before the owner safe \
         point",
    );
    assert_eq!(
        cpu1.lock_run_queue(crate::RunQueueGuardSource::TestInspection)
            .nr_queued(),
        1,
        "the target runqueue must expose the newly runnable thread before the wake returns",
    );
}

#[test]
fn task_context_fair_wake_keeps_the_sleep_cpu_when_the_waker_cpu_is_busier() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let sleeper = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu1.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();
    system.block_current_at(cpu1.as_mut(), 2).unwrap();
    system.complete_context_switch(cpu1.as_mut()).unwrap();

    let mut cpu0_only = CpuSet::empty(2);
    assert!(cpu0_only.insert(CpuId::new(0)));
    let waker_peer = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_affinity(cpu0_only))
        .unwrap();
    system.make_ready(waker_peer.id()).unwrap();
    system
        .enqueue_at(cpu0.as_mut(), waker_peer.id(), 3)
        .unwrap();

    let _runtime_handles = InstalledTaskHandles::new_task_context(system.as_ref(), cpu0.as_mut());
    assert_eq!(
        sleeper.wake_handle().wake_from_task(),
        crate::WakeResult::Notified
    );

    assert_eq!(
        sleeper.core.sched().lock().placement.queued_cpu(),
        Some(CpuId::new(1)),
        "wake-affine must not move a Fair wakee onto a busier waker CPU"
    );
}

#[test]
fn task_context_fair_wake_keeps_the_sleep_cpu_when_load_is_equal() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let sleeper = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu1.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();
    system.block_current_at(cpu1.as_mut(), 2).unwrap();
    system.complete_context_switch(cpu1.as_mut()).unwrap();

    let _runtime_handles = InstalledTaskHandles::new_task_context(system.as_ref(), cpu0.as_mut());
    assert_eq!(
        sleeper.wake_handle().wake_from_task(),
        crate::WakeResult::Notified
    );

    assert_eq!(
        sleeper.core.sched().lock().placement.queued_cpu(),
        Some(CpuId::new(1)),
        "equal demand must preserve the sleep CPU instead of causing cache migration"
    );
}

#[test]
fn fair_wake_does_not_republish_an_unchanged_deadline_index() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let sleeper = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    system.block_current_at(cpu.as_mut(), 2).unwrap();
    system.complete_context_switch(cpu.as_mut()).unwrap();

    let _runtime_handles = InstalledTaskHandles::new_task_context(system.as_ref(), cpu.as_mut());
    priority_index::reset_deadline_index_publications();
    assert_eq!(
        sleeper.wake_handle().wake_from_task(),
        crate::WakeResult::Notified
    );
    assert_eq!(
        priority_index::deadline_index_publications(),
        0,
        "a Fair-only rq transaction must not take the cpudl heap lock when the published Deadline \
         state is unchanged"
    );
}

#[test]
fn task_context_fair_wake_uses_the_less_loaded_waker_cpu() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let sleeper = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu1.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();
    system.block_current_at(cpu1.as_mut(), 2).unwrap();
    system.complete_context_switch(cpu1.as_mut()).unwrap();

    let mut cpu1_only = CpuSet::empty(2);
    assert!(cpu1_only.insert(CpuId::new(1)));
    let sleep_cpu_peer = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_affinity(cpu1_only))
        .unwrap();
    system.make_ready(sleep_cpu_peer.id()).unwrap();
    system
        .enqueue_at(cpu1.as_mut(), sleep_cpu_peer.id(), 3)
        .unwrap();

    let _runtime_handles = InstalledTaskHandles::new_task_context(system.as_ref(), cpu0.as_mut());
    assert_eq!(
        sleeper.wake_handle().wake_from_task(),
        crate::WakeResult::Notified
    );

    assert_eq!(
        sleeper.core.sched().lock().placement.queued_cpu(),
        Some(CpuId::new(0)),
        "wake-affine must use a strictly less loaded waker CPU"
    );
}

#[test]
fn task_context_wait_claim_uses_the_less_loaded_waker_cpu() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let sleeper = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu1.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();
    system.block_current_at(cpu1.as_mut(), 2).unwrap();
    system.complete_context_switch(cpu1.as_mut()).unwrap();

    let mut cpu1_only = CpuSet::empty(2);
    assert!(cpu1_only.insert(CpuId::new(1)));
    let sleep_cpu_peer = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_affinity(cpu1_only))
        .unwrap();
    system.make_ready(sleep_cpu_peer.id()).unwrap();
    system
        .enqueue_at(cpu1.as_mut(), sleep_cpu_peer.id(), 3)
        .unwrap();

    let claim = WaitWakeClaim::new(sleeper.id(), sleeper.core.park_generation());
    assert!(claim.select());
    let _runtime_handles = InstalledTaskHandles::new_task_context(system.as_ref(), cpu0.as_mut());
    assert_eq!(
        sleeper.wake_handle().deliver_wait_claim_from_task(&claim),
        WaitWakeDelivery::Delivered
    );

    assert_eq!(
        sleeper.core.sched().lock().placement.queued_cpu(),
        Some(CpuId::new(0)),
        "wait-claim delivery must share ordinary Fair wake-affine load comparison"
    );
}

#[test]
fn fair_wake_republishes_reschedule_for_a_dedicated_idle_owner() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let mut cpu1_only = CpuSet::empty(2);
    assert!(cpu1_only.insert(CpuId::new(1)));
    for cpu in [&mut cpu0, &mut cpu1] {
        let idle = system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        assert_eq!(
            system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
            idle.id()
        );
        system.complete_context_switch(cpu.as_mut()).unwrap();
    }

    let sleeper = system
        .create_thread(
            ThreadSpec::new(SchedulePolicy::fair(
                Nice::new(19).unwrap(),
                FairMode::Normal,
            ))
            .with_affinity(cpu1_only.clone()),
        )
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu1.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();
    assert_eq!(
        system.block_current_at(cpu1.as_mut(), 2).unwrap().next(),
        cpu1.remote().idle_thread().unwrap()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();

    let contender = system
        .create_thread(
            ThreadSpec::new(SchedulePolicy::fair(
                Nice::new(-20).unwrap(),
                FairMode::Normal,
            ))
            .with_affinity(cpu1_only),
        )
        .unwrap();
    system.make_ready(contender.id()).unwrap();
    system.enqueue_at(cpu1.as_mut(), contender.id(), 3).unwrap();
    assert!(cpu1.remote().take_preempt_requested());
    assert!(!cpu1.remote().needs_reschedule());

    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu0.as_mut());
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    assert_eq!(sleeper.wake_handle().wake(), crate::WakeResult::Notified);

    assert_eq!(
        cpu1.lock_run_queue(crate::RunQueueGuardSource::TestInspection)
            .nr_queued(),
        2
    );
    assert!(
        cpu1.remote().needs_reschedule(),
        "Linux idle-class wakeup must publish a strong reschedule even when another queued Fair \
         contender wins the EEVDF pick"
    );
}

fn remote_fifo_wake_fixture(
    current_priority: u8,
    woken_priority: u8,
) -> (
    Pin<Box<TaskSystem>>,
    Pin<Box<CpuLocal>>,
    Pin<Box<CpuLocal>>,
    ThreadHandle,
) {
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let sleeper = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(woken_priority).unwrap(),
        )))
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu1.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();
    system.block_current_at(cpu1.as_mut(), 2).unwrap();
    system.complete_context_switch(cpu1.as_mut()).unwrap();

    let current = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(current_priority).unwrap(),
        )))
        .unwrap();
    system.make_ready(current.id()).unwrap();
    system.enqueue_at(cpu1.as_mut(), current.id(), 3).unwrap();
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 3).unwrap().next(),
        current.id()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();
    (system, cpu0, cpu1, sleeper)
}

#[test]
fn lower_priority_remote_wake_uses_lower_priority_cpu_with_one_doorbell() {
    crate::test_runtime::reset_irq_state();
    let (system, mut cpu0, cpu1, sleeper) = remote_fifo_wake_fixture(10, 1);
    let local_runnable_before = cpu0
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .nr_queued();
    let remote_runnable_before = cpu1
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .nr_queued();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu0.as_mut());
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);

    assert_eq!(sleeper.wake_handle().wake(), crate::WakeResult::Notified);

    assert_eq!(
        system.thread_state(sleeper.id()).unwrap(),
        ThreadState::Ready
    );
    assert_eq!(
        cpu0.lock_run_queue(crate::RunQueueGuardSource::TestInspection)
            .nr_queued(),
        local_runnable_before + 1
    );
    assert_eq!(
        cpu1.lock_run_queue(crate::RunQueueGuardSource::TestInspection)
            .nr_queued(),
        remote_runnable_before
    );
    assert_eq!(
        crate::test_runtime::scheduler_ipi_send_count(),
        1,
        "wake preemption and the implied push callback must share one scheduler doorbell"
    );
}

#[test]
fn higher_priority_remote_wake_sends_one_reschedule_ipi() {
    crate::test_runtime::reset_irq_state();
    let (system, mut cpu0, cpu1, sleeper) = remote_fifo_wake_fixture(1, 10);
    let mut cpu1_only = CpuSet::empty(2);
    assert!(cpu1_only.insert(CpuId::new(1)));
    system.set_affinity(sleeper.id(), cpu1_only).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu0.as_mut());
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    let runnable_before = cpu1
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .nr_queued();

    assert_eq!(sleeper.wake_handle().wake(), crate::WakeResult::Notified);

    assert_eq!(
        system.thread_state(sleeper.id()).unwrap(),
        ThreadState::Ready
    );
    assert_eq!(
        cpu1.lock_run_queue(crate::RunQueueGuardSource::TestInspection)
            .nr_queued(),
        runnable_before + 1
    );
    assert_eq!(
        crate::test_runtime::scheduler_ipi_send_count(),
        1,
        "a direct wake above the target current priority must publish one reschedule edge"
    );
}

#[test]
fn singleton_rt_wake_bypasses_root_domain_priority_indexes() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let mut cpu0_only = CpuSet::empty(2);
    assert!(cpu0_only.insert(CpuId::new(0)));
    let sleeper = system
        .create_thread(
            ThreadSpec::new(SchedulePolicy::fifo(RtPriority::new(80).unwrap()))
                .with_affinity(cpu0_only),
        )
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu0.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu0.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    system.block_current_at(cpu0.as_mut(), 2).unwrap();
    system.complete_context_switch(cpu0.as_mut()).unwrap();

    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu1.as_mut());
    priority_index::reset_priority_index_lookups();
    assert_eq!(sleeper.wake_handle().wake(), crate::WakeResult::Notified);
    assert_eq!(
        priority_index::priority_index_lookups(),
        0,
        "Linux RT never enters cpupri when nr_cpus_allowed is one"
    );
}

fn install_running_fifo(
    system: &TaskSystem,
    cpu: Pin<&mut CpuLocal>,
    priority: u8,
    now_ns: u64,
) -> ThreadHandle {
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(priority).unwrap(),
        )))
        .unwrap();
    system.make_ready(thread.id()).unwrap();
    system.enqueue_at(cpu, thread.id(), now_ns).unwrap();
    thread
}

#[test]
fn wide_affinity_rt_wake_uses_cpu_priority_instead_of_load() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let sleeper = install_running_fifo(&system, cpu0.as_mut(), 50, 1);
    assert_eq!(
        system.schedule_at(cpu0.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    system.block_current_at(cpu0.as_mut(), 2).unwrap();
    system.complete_context_switch(cpu0.as_mut()).unwrap();

    let high = install_running_fifo(&system, cpu0.as_mut(), 90, 3);
    assert_eq!(
        system.schedule_at(cpu0.as_mut(), 3).unwrap().next(),
        high.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    let low = install_running_fifo(&system, cpu1.as_mut(), 10, 3);
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 3).unwrap().next(),
        low.id()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();

    assert_eq!(
        system.wake_thread_direct(Arc::clone(&sleeper.core), None),
        crate::WakeResult::Notified
    );
    assert_eq!(
        sleeper.core.sched().lock().placement.queued_cpu(),
        Some(CpuId::new(1)),
        "an RT wake must choose the CPU running the least urgent RT work, even when its previous \
         CPU is cache-hot"
    );
}

#[test]
fn rt_placement_does_not_select_a_cpu_running_deadline_work() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let deadline = system
        .create_thread(ThreadSpec::new(SchedulePolicy::deadline(
            DeadlinePolicy::new(10, 100, 1_000, DeadlineFlags::NONE).unwrap(),
        )))
        .unwrap();
    system.make_ready(deadline.id()).unwrap();
    system.enqueue_at(cpu1.as_mut(), deadline.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 1).unwrap().next(),
        deadline.id()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();

    let realtime = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(50).unwrap(),
        )))
        .unwrap();
    system.make_ready(realtime.id()).unwrap();
    system
        .place_ready_at(cpu1.as_mut(), realtime.id(), 2)
        .unwrap();

    assert_eq!(
        realtime
            .core
            .sched()
            .lock()
            .placement
            .committed_migration_target(),
        Some(CpuId::new(0)),
        "cpupri must publish a CPU with runnable Deadline work in the HIGHER bucket"
    );
}

#[test]
fn first_deadline_placement_does_not_invent_an_absolute_deadline() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let early = system
        .create_thread(ThreadSpec::new(SchedulePolicy::deadline(
            DeadlinePolicy::new(10, 100, 1_000, DeadlineFlags::NONE).unwrap(),
        )))
        .unwrap();
    system.make_ready(early.id()).unwrap();
    system.enqueue_at(cpu0.as_mut(), early.id(), 3).unwrap();
    assert_eq!(
        system.schedule_at(cpu0.as_mut(), 3).unwrap().next(),
        early.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();

    let late = system
        .create_thread(ThreadSpec::new(SchedulePolicy::deadline(
            DeadlinePolicy::new(10, 400, 1_000, DeadlineFlags::NONE).unwrap(),
        )))
        .unwrap();
    system.make_ready(late.id()).unwrap();
    system.enqueue_at(cpu1.as_mut(), late.id(), 3).unwrap();
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 3).unwrap().next(),
        late.id()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();

    let contender = system
        .create_thread(ThreadSpec::new(SchedulePolicy::deadline(
            DeadlinePolicy::new(100, 200, 1_000, DeadlineFlags::NONE).unwrap(),
        )))
        .unwrap();
    system.make_ready(contender.id()).unwrap();
    system
        .place_ready_at(cpu0.as_mut(), contender.id(), 4)
        .unwrap();
    assert_eq!(
        contender
            .core
            .sched()
            .lock()
            .placement
            .committed_migration_target(),
        None,
        "a new Deadline task has no absolute deadline before its first target-rq enqueue"
    );
}

#[test]
fn nonpreempting_rt_wake_kicks_overloaded_owner_balance() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let mut cpu0_only = CpuSet::empty(2);
    assert!(cpu0_only.insert(CpuId::new(0)));
    let sleeper = system
        .create_thread(
            ThreadSpec::new(SchedulePolicy::fifo(RtPriority::new(5).unwrap()))
                .with_affinity(cpu0_only),
        )
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu0.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu0.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    system.block_current_at(cpu0.as_mut(), 2).unwrap();
    system.complete_context_switch(cpu0.as_mut()).unwrap();

    let high = install_running_fifo(&system, cpu0.as_mut(), 90, 3);
    assert_eq!(
        system.schedule_at(cpu0.as_mut(), 3).unwrap().next(),
        high.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    let pushable = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(10).unwrap(),
        )))
        .unwrap();
    system.make_ready(pushable.id()).unwrap();
    system.enqueue_at(cpu0.as_mut(), pushable.id(), 4).unwrap();
    let _ = cpu0.remote().take_preempt_requested();

    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu1.as_mut());
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    assert_eq!(
        sleeper.wake_handle().wake_from_task(),
        crate::WakeResult::Notified
    );
    assert_eq!(
        crate::test_runtime::scheduler_ipi_send_count(),
        1,
        "an RT enqueue on an overloaded remote runqueue must queue owner-side push work even when \
         the wakee cannot preempt"
    );
}

#[test]
fn lowering_rt_priority_notifies_overloaded_root_domain_owner() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let cpu0_current = install_running_fifo(&system, cpu0.as_mut(), 90, 1);
    assert_eq!(
        system.schedule_at(cpu0.as_mut(), 1).unwrap().next(),
        cpu0_current.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    let pushable = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(10).unwrap(),
        )))
        .unwrap();
    system.make_ready(pushable.id()).unwrap();
    system.enqueue_at(cpu0.as_mut(), pushable.id(), 2).unwrap();
    let _ = cpu0.remote().take_preempt_requested();

    let cpu1_current = install_running_fifo(&system, cpu1.as_mut(), 50, 3);
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 3).unwrap().next(),
        cpu1_current.id()
    );
    system.complete_context_switch(cpu1.as_mut()).unwrap();
    let _ = cpu1.remote().take_preempt_requested();

    let runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu1.as_mut());
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    let decision = system.block_current_at(cpu1.as_mut(), 4).unwrap();
    assert_ne!(
        Some(decision.next()),
        cpu1.idle(),
        "the regression requires a priority drop to a non-idle task"
    );
    assert_eq!(
        crate::test_runtime::scheduler_ipi_send_count(),
        1,
        "when a CPU lowers its RT priority, Linux RT notifies an overloaded root-domain owner so \
         its queued RT task can be pushed immediately"
    );
    drop(runtime_handles);

    let _source_handles = InstalledTaskHandles::new(system.as_ref(), cpu0.as_mut());
    priority_index::reset_priority_index_lookups();
    let _outcome = system.schedule_if_requested_at(cpu0.as_mut(), 5).unwrap();
    assert_eq!(
        priority_index::priority_index_lookups(),
        1,
        "an RT push must consume cpupri once instead of scanning every CPU load summary"
    );
    assert_eq!(
        pushable
            .core
            .sched()
            .lock()
            .placement
            .committed_migration_target(),
        Some(CpuId::new(1)),
        "the overloaded owner must execute its queued balance callback even when its current RT \
         task keeps running"
    );
}

#[test]
fn priority_drop_serializes_root_domain_push_delivery() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(4)).unwrap());
    let mut cpus = (0..4)
        .map(|id| system.create_cpu_local(CpuId::new(id)).unwrap())
        .collect::<Vec<_>>();
    for cpu in &mut cpus {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    for (source, running_priority, queued_priority, now) in
        [(0usize, 90, 10, 1), (2usize, 80, 20, 3)]
    {
        let running = install_running_fifo(&system, cpus[source].as_mut(), running_priority, now);
        assert_eq!(
            system
                .schedule_at(cpus[source].as_mut(), now)
                .unwrap()
                .next(),
            running.id()
        );
        system
            .complete_context_switch(cpus[source].as_mut())
            .unwrap();
        let pushable = system
            .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
                RtPriority::new(queued_priority).unwrap(),
            )))
            .unwrap();
        system.make_ready(pushable.id()).unwrap();
        system
            .enqueue_at(cpus[source].as_mut(), pushable.id(), now + 1)
            .unwrap();
        let _ = cpus[source].remote().take_preempt_requested();
    }

    let lowering = install_running_fifo(&system, cpus[1].as_mut(), 50, 5);
    assert_eq!(
        system.schedule_at(cpus[1].as_mut(), 5).unwrap().next(),
        lowering.id()
    );
    system.complete_context_switch(cpus[1].as_mut()).unwrap();
    let _ = cpus[1].remote().take_preempt_requested();

    let lowering_handles = InstalledTaskHandles::new(system.as_ref(), cpus[1].as_mut());
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    let decision = system.block_current_at(cpus[1].as_mut(), 6).unwrap();
    assert_ne!(Some(decision.next()), cpus[1].idle());
    assert_eq!(
        crate::test_runtime::scheduler_ipi_send_count(),
        1,
        "Linux RT starts one root-domain push iterator instead of broadcasting an IPI to every \
         overloaded rq"
    );
    drop(lowering_handles);

    let _first_source_handles = InstalledTaskHandles::new(system.as_ref(), cpus[0].as_mut());
    let _outcome = system
        .schedule_if_requested_at(cpus[0].as_mut(), 7)
        .unwrap();
    assert_eq!(
        crate::test_runtime::scheduler_ipi_send_count(),
        3,
        "the first source emits one migration reschedule and then hands the serialized scan to \
         the next source"
    );
    assert!(
        cpus[2].remote().needs_reschedule(),
        "the second overload owner must hold the next root-domain push generation"
    );
}

#[test]
fn pinned_rt_work_does_not_kick_owner_push() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let mut cpu0_only = CpuSet::empty(2);
    assert!(cpu0_only.insert(CpuId::new(0)));
    let sleeper = system
        .create_thread(
            ThreadSpec::new(SchedulePolicy::fifo(RtPriority::new(5).unwrap()))
                .with_affinity(cpu0_only.clone()),
        )
        .unwrap();
    system.make_ready(sleeper.id()).unwrap();
    system.enqueue_at(cpu0.as_mut(), sleeper.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu0.as_mut(), 1).unwrap().next(),
        sleeper.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    system.block_current_at(cpu0.as_mut(), 2).unwrap();
    system.complete_context_switch(cpu0.as_mut()).unwrap();

    let high = install_running_fifo(&system, cpu0.as_mut(), 90, 3);
    assert_eq!(
        system.schedule_at(cpu0.as_mut(), 3).unwrap().next(),
        high.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    let pinned = system
        .create_thread(
            ThreadSpec::new(SchedulePolicy::fifo(RtPriority::new(10).unwrap()))
                .with_affinity(cpu0_only),
        )
        .unwrap();
    system.make_ready(pinned.id()).unwrap();
    system.enqueue_at(cpu0.as_mut(), pinned.id(), 4).unwrap();
    let _ = cpu0.remote().take_preempt_requested();

    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu1.as_mut());
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    assert_eq!(
        sleeper.wake_handle().wake_from_task(),
        crate::WakeResult::Notified
    );
    assert_eq!(
        crate::test_runtime::scheduler_ipi_send_count(),
        0,
        "single-CPU-affinity RT work cannot be pushed and must not ring an owner balance doorbell"
    );
}

#[test]
fn queued_affinity_update_refreshes_pushability_summary() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let high = install_running_fifo(&system, cpu0.as_mut(), 90, 1);
    assert_eq!(
        system.schedule_at(cpu0.as_mut(), 1).unwrap().next(),
        high.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    let queued = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(10).unwrap(),
        )))
        .unwrap();
    system.make_ready(queued.id()).unwrap();
    system.enqueue_at(cpu0.as_mut(), queued.id(), 2).unwrap();
    assert!(
        system
            .root_domain
            .cpu_has_overload(CpuId::new(0), SchedulingClass::Realtime)
    );

    let mut cpu0_only = CpuSet::empty(2);
    assert!(cpu0_only.insert(CpuId::new(0)));
    let narrow = system.request_affinity(queued.id(), cpu0_only).unwrap();
    system.drain_owner_control_at(cpu0.as_mut(), 3).unwrap();
    assert_eq!(narrow.try_result(), Some(Ok(())));
    assert!(
        !system
            .root_domain
            .cpu_has_overload(CpuId::new(0), SchedulingClass::Realtime)
    );

    let widen = system
        .request_affinity(queued.id(), CpuSet::all(2))
        .unwrap();
    system.drain_owner_control_at(cpu0.as_mut(), 4).unwrap();
    assert_eq!(widen.try_result(), Some(Ok(())));
    assert!(
        system
            .root_domain
            .cpu_has_overload(CpuId::new(0), SchedulingClass::Realtime)
    );
}

#[test]
fn unavailable_direct_wakers_do_not_coalesce_before_the_thread_lock() {
    let system = Arc::new(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let core = Arc::clone(&thread.core);
    let sched = core.sched().lock();
    let entries = Arc::new(AtomicUsize::new(0));

    let first = {
        let system = Arc::clone(&system);
        let core = Arc::clone(&core);
        let entries = Arc::clone(&entries);
        std::thread::spawn(move || {
            entries.fetch_add(1, Ordering::Release);
            system.wake_thread_direct(core, Some(CpuId::new(0)))
        })
    };
    let second = {
        let system = Arc::clone(&system);
        let core = Arc::clone(&core);
        let entries = Arc::clone(&entries);
        std::thread::spawn(move || {
            entries.fetch_add(1, Ordering::Release);
            system.wake_thread_direct(core, Some(CpuId::new(0)))
        })
    };
    while entries.load(Ordering::Acquire) != 2 {
        std::thread::yield_now();
    }
    assert!(
        !core.wake_is_pending(),
        "a waker blocked on thread state must not publish a bit that another waker can consume"
    );
    drop(sched);

    assert_eq!(first.join().unwrap(), crate::WakeResult::Unavailable);
    assert_eq!(second.join().unwrap(), crate::WakeResult::Unavailable);
}

#[test]
fn same_cpu_task_wake_before_park_preserves_the_notification_until_prepare() {
    crate::test_runtime::reset_irq_state();
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new_task_context(system.as_ref(), cpu.as_mut());

    dispatch::reset_wake_target_selections();
    assert_eq!(
        running.wake_handle().wake_from_task(),
        crate::WakeResult::Notified
    );
    assert_eq!(
        dispatch::wake_target_selections(),
        0,
        "waking the current running thread must not perform CPU placement"
    );

    let token = task_runtime::irq_guard_enter();
    assert_eq!(
        system.prepare_park(cpu.as_mut()).unwrap(),
        ParkPrepare::Notified,
        "a direct wake of the running owner must still win the next park race"
    );
    // SAFETY: this consumes the task-context owner token on the same host
    // thread after direct CpuLocal access has ended.
    unsafe { task_runtime::irq_guard_exit(token) };
}

#[test]
fn policy_reschedule_doorbell_runs_outside_cold_irq_lock_domains() {
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(thread.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), thread.id(), 0).unwrap();
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    let owner_scope_guards = crate::test_runtime::active_irq_guards();

    system
        .set_thread_policy(
            thread.id(),
            SchedulePolicy::fifo(crate::RtPriority::new(1).unwrap()),
        )
        .unwrap();

    assert_eq!(crate::test_runtime::scheduler_ipi_send_count(), 1);
    assert_eq!(
        crate::test_runtime::scheduler_ipi_irq_guards(),
        owner_scope_guards + 1,
        "the reschedule publication guard may cover the doorbell, but registry/root-domain guards \
         must be gone"
    );
}

#[test]
fn permanent_scheduler_ipi_failure_fails_at_the_publication_boundary() {
    let node = Box::pin(crate::inbox::InboxNode::new(InboxKind::OwnerControl));
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let remote = &system.cpu_remotes[1];
    assert!(remote.mark_online());
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::InvalidArgument);

    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        publish_test_scheduler_work(remote, test_inbox_node(&node), 2);
    }));
    assert!(
        failure.is_err(),
        "a scheduler transport that cannot guarantee delivery must fail-stop"
    );
}

#[test]
fn registered_remote_endpoint_is_separate_from_owner_mutable_state() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let owner_address = (cpu.as_ref().get_ref() as *const CpuLocal).addr();
    let endpoint_address = Arc::as_ptr(cpu.as_ref().get_ref().remote()).addr();
    assert_ne!(owner_address, endpoint_address);

    system.bring_cpu_online(cpu.as_mut()).unwrap();
    assert_eq!(
        (system.state.lock().cpu_remote(CpuId::new(0)).unwrap() as *const CpuRemote).addr(),
        endpoint_address
    );

    assert_eq!(
        (system.state.lock().cpu_remote(CpuId::new(0)).unwrap() as *const CpuRemote).addr(),
        endpoint_address,
        "owner reborrowing must not alias or invalidate the remote endpoint"
    );
}

#[test]
fn quiescent_cpu_can_cycle_offline_and_online() {
    crate::test_runtime::clear_task_handles();
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let cached_remote_handle = system.runtime_cpu_remote_handle(CpuId::new(1));
    assert!(!cached_remote_handle.is_none());
    let cached_remote = unsafe {
        // SAFETY: the handle originates from `system`, which remains alive for
        // the complete observation.
        &*core::ptr::with_exposed_provenance::<CpuRemote>(cached_remote_handle.into_raw())
    };
    assert!(!cached_remote.is_online());
    let mut affinity0 = CpuSet::empty(2);
    let mut affinity1 = CpuSet::empty(2);
    assert!(affinity0.insert(CpuId::new(0)));
    assert!(affinity1.insert(CpuId::new(1)));
    let _idle0 = system
        .register_idle_thread(
            cpu0.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
                .with_affinity(affinity0),
        )
        .unwrap();
    let _idle1 = system
        .register_idle_thread(
            cpu1.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
                .with_affinity(affinity1),
        )
        .unwrap();
    system.bring_cpu_online_at(cpu0.as_mut(), 10).unwrap();
    system.bring_cpu_online_at(cpu1.as_mut(), 10).unwrap();
    assert!(cached_remote.is_online());

    system.take_cpu_offline(cpu1.as_mut()).unwrap();
    assert_eq!(system.online_cpu_count(), 1);
    assert!(!cpu1.is_online());
    assert!(!cached_remote.is_online());
    assert!(system.cpu_remote(CpuId::new(1)).is_none());
    assert_eq!(
        system.runtime_cpu_remote_handle(CpuId::new(1)),
        cached_remote_handle,
        "offline publication must not replace the shutdown-lifetime endpoint"
    );

    system.bring_cpu_online_at(cpu1.as_mut(), 1_000).unwrap();
    assert_eq!(system.online_cpu_count(), 2);
    assert!(cpu1.is_online());
    assert!(cached_remote.is_online());
    assert_eq!(
        crate::test_runtime::take_cpu_lifecycle_events(),
        [
            crate::test_runtime::CpuLifecycleEvent::Online(crate::runtime::RuntimeCpuId::new(0),),
            crate::test_runtime::CpuLifecycleEvent::Online(crate::runtime::RuntimeCpuId::new(1),),
            crate::test_runtime::CpuLifecycleEvent::Offline(crate::runtime::RuntimeCpuId::new(1),),
            crate::test_runtime::CpuLifecycleEvent::Online(crate::runtime::RuntimeCpuId::new(1),),
        ],
        "scheduler publication must transact the matching runtime wake sources"
    );
}

#[test]
fn pending_remote_publication_prevents_cpu_offline() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let mut affinity0 = CpuSet::empty(2);
    let mut affinity1 = CpuSet::empty(2);
    assert!(affinity0.insert(CpuId::new(0)));
    assert!(affinity1.insert(CpuId::new(1)));
    let _idle0 = system
        .register_idle_thread(
            cpu0.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
                .with_affinity(affinity0),
        )
        .unwrap();
    let _idle1 = system
        .register_idle_thread(
            cpu1.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
                .with_affinity(affinity1),
        )
        .unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);

    let node = Box::pin(crate::inbox::InboxNode::new(InboxKind::OwnerControl));
    publish_test_scheduler_work(&system.cpu_remotes[1], test_inbox_node(&node), 7);

    assert_eq!(
        system.take_cpu_offline(cpu1.as_mut()),
        Err(TaskError::CpuNotQuiescent(1))
    );
    assert!(cpu1.is_online(), "a rejected transition must roll back");
    assert_eq!(system.online_cpu_count(), 2);
}

#[test]
fn pending_local_scheduler_work_prevents_cpu_offline() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    cpu1.request_scheduler_work();

    assert_eq!(
        system.take_cpu_offline(cpu1.as_mut()),
        Err(TaskError::CpuNotQuiescent(1)),
        "CPU hotplug must not erase scheduler work that has not reached a safe point"
    );
    assert!(cpu1.is_online(), "a rejected transition must roll back");
    assert!(cpu1.needs_reschedule());
    assert_eq!(system.online_cpu_count(), 2);
}

#[test]
fn migration_reservation_rejects_a_draining_target_before_placement_changes() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(thread.id()).unwrap();

    // Model the hotplug race after CPU 1 was selected but before the source
    // committed and published the detached migration.
    assert!(cpu1.remote().try_deactivate());
    assert!(cpu1.remote().try_begin_draining());
    assert!(matches!(
        system.prepare_owner_migration(&thread.core, CpuId::new(0), CpuId::new(1)),
        Err(TaskError::CpuOffline(1))
    ));

    let state = system.state.lock();
    let sched = state.thread_record(thread.id()).unwrap().sched.lock();
    assert_eq!(sched.placement.queued_cpu(), None);
    assert!(!sched.placement.has_pending_migration());
}

#[test]
fn migration_carrier_closes_hotplug_before_detached_placement_commits() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let mut affinity = CpuSet::empty(2);
    assert!(affinity.insert(CpuId::new(1)));
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_affinity(affinity))
        .unwrap();
    system.make_ready(thread.id()).unwrap();
    placement::drain_next_migration_cpus_before_publication();

    system
        .place_ready_at(cpu0.as_mut(), thread.id(), 0)
        .unwrap();
    system.drain_owner_control_at(cpu1.as_mut(), 0).unwrap();

    let sched = thread.core.sched().lock();
    assert_eq!(sched.placement.queued_cpu(), Some(CpuId::new(1)));
    assert!(!sched.placement.has_pending_migration());
}

#[test]
fn runtime_failure_leaves_cpu_lifecycle_transition_retryable() {
    crate::test_runtime::clear_task_handles();
    crate::test_runtime::configure_cpu_lifecycle(RuntimeStatus::Platform, RuntimeStatus::Success);
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let mut affinity0 = CpuSet::empty(2);
    let mut affinity1 = CpuSet::empty(2);
    assert!(affinity0.insert(CpuId::new(0)));
    assert!(affinity1.insert(CpuId::new(1)));
    let _idle0 = system
        .register_idle_thread(
            cpu0.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
                .with_affinity(affinity0),
        )
        .unwrap();
    let _idle1 = system
        .register_idle_thread(
            cpu1.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
                .with_affinity(affinity1),
        )
        .unwrap();

    assert_eq!(
        system.bring_cpu_online(cpu0.as_mut()),
        Err(TaskError::RuntimeFailure(RuntimeStatus::Platform as u32))
    );
    assert!(!cpu0.is_online());
    assert_eq!(system.online_cpu_count(), 0);

    crate::test_runtime::configure_cpu_lifecycle(RuntimeStatus::Success, RuntimeStatus::Success);
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    let _ = crate::test_runtime::take_cpu_lifecycle_events();

    crate::test_runtime::configure_cpu_lifecycle(RuntimeStatus::Success, RuntimeStatus::Platform);
    assert_eq!(
        system.take_cpu_offline(cpu1.as_mut()),
        Err(TaskError::RuntimeFailure(RuntimeStatus::Platform as u32))
    );
    assert!(cpu1.is_online());
    assert_eq!(system.online_cpu_count(), 2);

    crate::test_runtime::configure_cpu_lifecycle(RuntimeStatus::Success, RuntimeStatus::Success);
    system.take_cpu_offline(cpu1.as_mut()).unwrap();
    assert!(!cpu1.is_online());
    assert_eq!(system.online_cpu_count(), 1);
    assert_eq!(
        crate::test_runtime::take_cpu_lifecycle_events(),
        [
            crate::test_runtime::CpuLifecycleEvent::Offline(crate::runtime::RuntimeCpuId::new(1),),
            crate::test_runtime::CpuLifecycleEvent::Offline(crate::runtime::RuntimeCpuId::new(1),),
        ]
    );
}

#[test]
fn last_online_cpu_cannot_leave_the_root_domain() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut affinity = CpuSet::empty(1);
    assert!(affinity.insert(CpuId::new(0)));
    let _idle = system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
                .with_affinity(affinity),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    assert_eq!(
        system.take_cpu_offline(cpu.as_mut()),
        Err(TaskError::LastOnlineCpu(0))
    );
    assert!(cpu.is_online());
    assert_eq!(system.online_cpu_count(), 1);
}

#[test]
fn live_thread_without_remaining_affinity_prevents_cpu_offline() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let mut affinity0 = CpuSet::empty(2);
    let mut affinity1 = CpuSet::empty(2);
    assert!(affinity0.insert(CpuId::new(0)));
    assert!(affinity1.insert(CpuId::new(1)));
    let _idle0 = system
        .register_idle_thread(
            cpu0.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
                .with_affinity(affinity0),
        )
        .unwrap();
    let _idle1 = system
        .register_idle_thread(
            cpu1.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
                .with_affinity(affinity1.clone()),
        )
        .unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();

    let _pinned = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_affinity(affinity1))
        .unwrap();

    assert_eq!(
        system.take_cpu_offline(cpu1.as_mut()),
        Err(TaskError::CpuNotQuiescent(1))
    );
    assert!(cpu1.is_online());
    assert_eq!(system.online_cpu_count(), 2);
}

#[test]
fn configuration_rejects_batch_larger_than_irq_contract() {
    assert!(matches!(
        TaskSystem::new(TaskSystemConfig::new(1).with_batch_limit(crate::DEFAULT_BATCH_LIMIT + 1)),
        Err(TaskError::InvalidConfiguration)
    ));
}

#[test]
fn exhausted_thread_slot_generation_never_wraps_to_the_first_identity() {
    assert_ne!(
        next_generation(MAX_THREAD_GENERATION),
        1,
        "slot reuse must not make an old generation-1 ThreadId valid again"
    );
    let mut slot = ThreadSlot {
        generation: MAX_THREAD_GENERATION,
        record: None,
        pending_deadline_reservation: 0,
    };
    assert!(
        !advance_thread_slot_generation(&mut slot),
        "an exhausted empty slot must be retired rather than reused"
    );
    assert_eq!(slot.generation, MAX_THREAD_GENERATION);
}

use crate::{DeadlineFlags, DeadlinePolicy, FairMode, Nice, RtPriority, ThreadExtensionOps};

static DEADLINE_OVERRUN_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

struct InstalledTaskHandles {
    irq_token: Option<crate::runtime::IrqGuardToken>,
}

impl InstalledTaskHandles {
    fn new(system: Pin<&TaskSystem>, cpu: Pin<&mut CpuLocal>) -> Self {
        crate::test_runtime::install_task_handles(
            (system.get_ref() as *const TaskSystem).expose_provenance(),
            // SAFETY: the test fixture keeps the owner object pinned and
            // serializes every scheduler access until the handle is cleared.
            (unsafe { Pin::get_unchecked_mut(cpu) } as *mut CpuLocal).expose_provenance(),
        );
        Self {
            irq_token: Some(task_runtime::irq_guard_enter()),
        }
    }

    fn new_task_context(system: Pin<&TaskSystem>, cpu: Pin<&mut CpuLocal>) -> Self {
        crate::test_runtime::install_task_handles(
            (system.get_ref() as *const TaskSystem).expose_provenance(),
            // SAFETY: this fixture exposes the owner only through facade
            // scheduler guards after bootstrap setup has completed.
            (unsafe { Pin::get_unchecked_mut(cpu) } as *mut CpuLocal).expose_provenance(),
        );
        Self { irq_token: None }
    }
}

impl Drop for InstalledTaskHandles {
    fn drop(&mut self) {
        if let Some(token) = self.irq_token.take() {
            // SAFETY: construction entered this token on the same host test
            // thread and the fixture has finished every direct CpuLocal access.
            unsafe { task_runtime::irq_guard_exit(token) };
        }
        crate::test_runtime::clear_task_handles();
    }
}

#[test]
fn unsuccessful_fair_balance_backs_off_from_monotonic_completion_time() {
    const ENTRY_NOW_NS: u64 = BALANCE_INTERVAL_NS;
    const COMPLETION_NOW_NS: u64 = 10_000;
    const BALANCE_INTERVAL_NS: u64 = 1_000;

    let system = Box::pin(
        TaskSystem::new(TaskSystemConfig::new(1).with_balance_interval_ns(BALANCE_INTERVAL_NS))
            .unwrap(),
    );
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online_at(cpu.as_mut(), 0).unwrap();
    let contender = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(contender.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), contender.id(), 0).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    assert!(
        cpu.as_mut()
            .publish_fair_balance_due(MonotonicInstant::from_nanos(ENTRY_NOW_NS).unwrap())
    );
    crate::test_runtime::set_monotonic_ns(COMPLETION_NOW_NS);

    assert_eq!(system.balance_fair(cpu.as_mut()), Ok(None));
    assert!(
        !cpu.as_mut()
            .publish_fair_balance_due(MonotonicInstant::from_nanos(COMPLETION_NOW_NS).unwrap()),
        "completed balance work must not leave its next period already overdue"
    );
    assert!(
        !cpu.as_mut().publish_fair_balance_due(
            MonotonicInstant::from_nanos(COMPLETION_NOW_NS.saturating_add(BALANCE_INTERVAL_NS))
                .unwrap()
        ),
        "a balance pass that moved no work must back off instead of retrying at the minimum \
         interval"
    );
    assert!(
        cpu.as_mut().publish_fair_balance_due(
            MonotonicInstant::from_nanos(
                COMPLETION_NOW_NS.saturating_add(BALANCE_INTERVAL_NS.saturating_mul(2))
            )
            .unwrap()
        ),
        "the first unsuccessful balance pass must double its retry interval"
    );
}

#[test]
fn affinity_constrained_fair_balance_keeps_backing_off() {
    const INTERVAL_NS: u64 = 1_000;

    let system =
        TaskSystem::new(TaskSystemConfig::new(2).with_balance_interval_ns(INTERVAL_NS)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online_at(cpu.as_mut(), 0).unwrap();
    }

    let mut cpu0_only = CpuSet::empty(2);
    assert!(cpu0_only.insert(CpuId::new(0)));
    let pinned = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_affinity(cpu0_only))
        .unwrap();
    system.make_ready(pinned.id()).unwrap();
    system.enqueue_at(cpu0.as_mut(), pinned.id(), 0).unwrap();

    assert!(
        cpu0.as_mut()
            .publish_fair_balance_due(MonotonicInstant::from_nanos(INTERVAL_NS).unwrap())
    );
    crate::test_runtime::set_monotonic_ns(INTERVAL_NS);
    assert_eq!(system.balance_fair(cpu0.as_mut()), Ok(None));
    let second_balance_ns = INTERVAL_NS.saturating_mul(3);
    assert!(
        cpu0.as_mut()
            .publish_fair_balance_due(MonotonicInstant::from_nanos(second_balance_ns).unwrap())
    );

    crate::test_runtime::set_monotonic_ns(second_balance_ns);
    assert_eq!(system.balance_fair(cpu0.as_mut()), Ok(None));
    assert!(
        !cpu0.as_mut().publish_fair_balance_due(
            MonotonicInstant::from_nanos(second_balance_ns.saturating_add(INTERVAL_NS * 2))
                .unwrap()
        ),
        "an affinity-constrained domain must continue exponential backoff"
    );
    assert!(cpu0.as_mut().publish_fair_balance_due(
        MonotonicInstant::from_nanos(second_balance_ns.saturating_add(INTERVAL_NS * 4)).unwrap()
    ));
}

#[test]
fn successful_fair_balance_resets_the_minimum_interval() {
    const INTERVAL_NS: u64 = 1_000;

    let system =
        TaskSystem::new(TaskSystemConfig::new(2).with_balance_interval_ns(INTERVAL_NS)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online_at(cpu.as_mut(), 0).unwrap();
    }

    let movable = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let peer = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    for thread in [&movable, &peer] {
        system.make_ready(thread.id()).unwrap();
        system.enqueue_at(cpu0.as_mut(), thread.id(), 0).unwrap();
    }

    crate::test_runtime::set_monotonic_ns(INTERVAL_NS);
    assert!(
        cpu0.as_mut()
            .publish_fair_balance_due(MonotonicInstant::from_nanos(INTERVAL_NS).unwrap())
    );
    balance::reset_balance_candidate_visits();
    assert_eq!(system.balance_fair(cpu0.as_mut()), Ok(Some(movable.id())));
    assert_eq!(
        balance::balance_candidate_visits(),
        1,
        "a periodic balance transaction must select its source candidate only once"
    );
    assert!(
        cpu0.as_mut().publish_fair_balance_due(
            MonotonicInstant::from_nanos(INTERVAL_NS.saturating_mul(2)).unwrap()
        ),
        "successful migration must restore the configured minimum interval"
    );
}

#[test]
fn fair_balance_selects_one_candidate_across_multiple_destinations() {
    const INTERVAL_NS: u64 = 1_000;

    let system =
        TaskSystem::new(TaskSystemConfig::new(4).with_balance_interval_ns(INTERVAL_NS)).unwrap();
    let mut cpus = (0..4)
        .map(|index| system.create_cpu_local(CpuId::new(index)).unwrap())
        .collect::<Vec<_>>();
    for cpu in &mut cpus {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online_at(cpu.as_mut(), 0).unwrap();
    }
    let movable = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let peer = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    for thread in [&movable, &peer] {
        system.make_ready(thread.id()).unwrap();
        system.enqueue_at(cpus[0].as_mut(), thread.id(), 0).unwrap();
    }

    crate::test_runtime::set_monotonic_ns(INTERVAL_NS);
    assert!(
        cpus[0]
            .as_mut()
            .publish_fair_balance_due(MonotonicInstant::from_nanos(INTERVAL_NS).unwrap())
    );
    balance::reset_balance_candidate_visits();
    assert_eq!(
        system.balance_fair(cpus[0].as_mut()),
        Ok(Some(movable.id()))
    );
    assert_eq!(
        balance::balance_candidate_visits(),
        1,
        "one source-rq candidate transaction must choose among every eligible destination"
    );
}

#[test]
fn fair_balance_scans_past_an_affinity_constrained_candidate() {
    const INTERVAL_NS: u64 = 1_000;

    let system =
        TaskSystem::new(TaskSystemConfig::new(2).with_balance_interval_ns(INTERVAL_NS)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online_at(cpu.as_mut(), 0).unwrap();
    }

    let mut cpu0_only = CpuSet::empty(2);
    assert!(cpu0_only.insert(CpuId::new(0)));
    let pinned = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_affinity(cpu0_only))
        .unwrap();
    let movable = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    for thread in [&pinned, &movable] {
        system.make_ready(thread.id()).unwrap();
        system.enqueue_at(cpu0.as_mut(), thread.id(), 0).unwrap();
    }

    crate::test_runtime::set_monotonic_ns(INTERVAL_NS);
    assert!(
        cpu0.as_mut()
            .publish_fair_balance_due(MonotonicInstant::from_nanos(INTERVAL_NS).unwrap())
    );
    assert_eq!(
        system.balance_fair(cpu0.as_mut()),
        Ok(Some(movable.id())),
        "one constrained EEVDF candidate must not hide another movable entity"
    );
}

#[test]
fn periodic_fair_balance_does_not_move_light_work_toward_a_heavier_cpu() {
    const INTERVAL_NS: u64 = 1_000;

    let system =
        TaskSystem::new(TaskSystemConfig::new(2).with_balance_interval_ns(INTERVAL_NS)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online_at(cpu.as_mut(), 0).unwrap();
    }
    let light_policy = SchedulePolicy::fair(Nice::new(19).unwrap(), FairMode::Normal);
    for _ in 0..2 {
        let light = system.create_thread(ThreadSpec::new(light_policy)).unwrap();
        system.make_ready(light.id()).unwrap();
        system.enqueue_at(cpu0.as_mut(), light.id(), 0).unwrap();
    }
    let heavy = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fair(
            Nice::new(-20).unwrap(),
            FairMode::Normal,
        )))
        .unwrap();
    system.make_ready(heavy.id()).unwrap();
    system.enqueue_at(cpu1.as_mut(), heavy.id(), 0).unwrap();

    crate::test_runtime::set_monotonic_ns(INTERVAL_NS);
    assert!(
        cpu0.as_mut()
            .publish_fair_balance_due(MonotonicInstant::from_nanos(INTERVAL_NS).unwrap())
    );
    assert_eq!(
        system.balance_fair(cpu0.as_mut()),
        Ok(None),
        "count balancing must not move nice +19 work toward a CPU already carrying nice -20 demand"
    );
}

#[test]
fn local_timer_is_programmed_from_scheduler_completion_time() {
    const ENTRY_NOW_NS: u64 = 100;
    const COMPLETION_NOW_NS: u64 = 100_000_000;

    let system = Box::pin(
        TaskSystem::new(
            TaskSystemConfig::new(1).with_balance_interval_ns(COMPLETION_NOW_NS.saturating_mul(2)),
        )
        .unwrap(),
    );
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    crate::test_runtime::set_monotonic_ns(ENTRY_NOW_NS);
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system
        .bring_cpu_online_at(cpu.as_mut(), ENTRY_NOW_NS)
        .unwrap();
    let contender = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(contender.id()).unwrap();
    system
        .enqueue_at(cpu.as_mut(), contender.id(), ENTRY_NOW_NS)
        .unwrap();

    crate::test_runtime::set_monotonic_ns(COMPLETION_NOW_NS);
    crate::test_runtime::set_scheduler_ns(ENTRY_NOW_NS);
    system.program_local_timer(cpu.as_mut()).unwrap();
    let update = crate::test_runtime::take_scheduler_deadline_update()
        .expect("local timer programming must publish one complete update");
    let deadline_ns = update
        .deadline()
        .expect("the running fair dispatch must own a scheduler deadline")
        .as_nanos();
    assert!(
        deadline_ns > COMPLETION_NOW_NS,
        "published deadline {deadline_ns} must be relative to completion time \
         {COMPLETION_NOW_NS}, not entry time {ENTRY_NOW_NS}"
    );
}

#[test]
fn context_is_bound_to_the_allocated_thread_before_new_is_published() {
    crate::test_runtime::configure_context_binding(RuntimeStatus::Success);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let context = unsafe {
        // SAFETY: the unit runtime models this non-zero scalar as a live
        // context until the task-system fixture is dropped.
        ExecutionContextHandle::from_raw(0x1000)
    };
    let resources = unsafe {
        // SAFETY: the fake runtime accepts the unique context handle above;
        // the remaining resource handles are intentionally absent.
        ThreadResources::new(
            context,
            crate::runtime::StackHandle::NONE,
            crate::runtime::TlsHandle::NONE,
            crate::runtime::AddressSpaceToken::NONE,
        )
    };

    let thread = system
        .create_thread(unsafe {
            // SAFETY: this specification is the sole owner of `resources`.
            ThreadSpec::new(Default::default()).with_resources(resources)
        })
        .unwrap();

    assert_eq!(system.thread_state(thread.id()), Ok(ThreadState::New));
    assert_eq!(
        crate::test_runtime::last_context_binding(),
        Some(ContextThreadBinding {
            context,
            publication: thread.runtime_publication(),
        })
    );
}

#[test]
fn context_binding_runs_outside_irq_disabled_registry_section() {
    crate::test_runtime::configure_context_binding(RuntimeStatus::Success);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let context = unsafe {
        // SAFETY: the unit runtime models this non-zero scalar as a live
        // context until the task-system fixture is dropped.
        ExecutionContextHandle::from_raw(0x1800)
    };
    let resources = unsafe {
        // SAFETY: this specification is the sole owner of the modeled context.
        ThreadResources::new(
            context,
            crate::runtime::StackHandle::NONE,
            crate::runtime::TlsHandle::NONE,
            crate::runtime::AddressSpaceToken::NONE,
        )
    };

    let _thread = system
        .create_thread(unsafe {
            // SAFETY: ownership of `resources` is transferred exactly once.
            ThreadSpec::new(Default::default()).with_resources(resources)
        })
        .unwrap();

    assert_eq!(
        crate::test_runtime::irq_guards_at_context_bind(),
        0,
        "runtime binding may allocate or invoke platform code and must not run under an IRQ lock"
    );
}

#[test]
fn failed_context_binding_retires_the_allocated_generation() {
    crate::test_runtime::configure_resource_release(RuntimeStatus::Success);
    crate::test_runtime::configure_context_binding(RuntimeStatus::InvalidHandle);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let context = unsafe {
        // SAFETY: the unit runtime validates this modeled handle through its
        // configured failing context-binding result.
        ExecutionContextHandle::from_raw(0x2000)
    };
    let resources = unsafe {
        // SAFETY: ownership is transferred once into the failed create path.
        ThreadResources::new(
            context,
            crate::runtime::StackHandle::NONE,
            crate::runtime::TlsHandle::NONE,
            crate::runtime::AddressSpaceToken::NONE,
        )
    };

    let error = system
        .create_thread(unsafe {
            // SAFETY: this specification is the sole resource owner.
            ThreadSpec::new(Default::default()).with_resources(resources)
        })
        .unwrap_err();
    assert_eq!(
        error,
        TaskError::RuntimeFailure(RuntimeStatus::InvalidHandle as u32)
    );
    let failed = crate::test_runtime::last_context_binding().unwrap();

    crate::test_runtime::configure_context_binding(RuntimeStatus::Success);
    let replacement = system
        .create_thread(ThreadSpec::new(Default::default()))
        .unwrap();
    assert_eq!(replacement.id().slot(), failed.publication.identity().slot);
    assert_ne!(
        replacement.id().generation(),
        failed.publication.identity().generation
    );
    crate::test_runtime::configure_resource_release(RuntimeStatus::Unsupported);
}

#[test]
fn rejected_thread_releases_runtime_resources_before_extension() {
    use crate::test_runtime::ResourceReleaseEvent;

    static RELEASE_ORDER_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: no_extension_switch_in,
        on_switch_out: no_extension_switch_out,
        on_exit: no_extension_hook,
        on_deadline_overrun: no_extension_hook,
        drop: record_release_order_extension_drop,
    };

    crate::test_runtime::configure_resource_release(RuntimeStatus::Success);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let resources = unsafe {
        // SAFETY: the unit runtime accepts these unique modeled handles and
        // records their synchronous release order.
        ThreadResources::new(
            ExecutionContextHandle::from_raw(0x3000),
            crate::runtime::StackHandle::from_raw(0x4000),
            crate::runtime::TlsHandle::from_raw(0x5000),
            crate::runtime::AddressSpaceToken::NONE,
        )
    };
    let extension = unsafe {
        // SAFETY: the callback owns no external data and only records its drop.
        ThreadExtension::new(0, &RELEASE_ORDER_EXTENSION_OPS)
    };
    let result = system.create_thread(unsafe {
        // SAFETY: this specification uniquely owns every modeled resource.
        ThreadSpec::new(SchedulePolicy::default())
            .with_affinity(CpuSet::empty(2))
            .with_extension(extension)
            .with_resources(resources)
    });

    assert_eq!(result.unwrap_err(), TaskError::InvalidConfiguration);
    assert_eq!(
        crate::test_runtime::resource_release_events(),
        [
            ResourceReleaseEvent::DestroyContext,
            ResourceReleaseEvent::DeallocateTls,
            ResourceReleaseEvent::DeallocateStack,
            ResourceReleaseEvent::DropExtension,
        ]
    );
    crate::test_runtime::configure_resource_release(RuntimeStatus::Unsupported);
}

#[test]
fn busy_address_space_does_not_retain_completed_thread_resources() {
    use crate::test_runtime::ResourceReleaseEvent;

    static RETRY_RELEASE_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: no_extension_switch_in,
        on_switch_out: no_extension_switch_out,
        on_exit: no_extension_hook,
        on_deadline_overrun: no_extension_hook,
        drop: record_release_order_extension_drop,
    };

    crate::test_runtime::configure_resource_release(RuntimeStatus::Success);
    crate::test_runtime::configure_address_space_destroy(AddressSpaceDestroyOutcome::Active);
    crate::test_runtime::configure_address_space_reclaim_arm(AddressSpaceReclaimArmOutcome::Armed);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let resources = unsafe {
        // SAFETY: the unit runtime accepts these unique modeled handles and
        // retains them until its configured release operation succeeds.
        ThreadResources::new(
            ExecutionContextHandle::from_raw(0x6000),
            crate::runtime::StackHandle::from_raw(0x7000),
            crate::runtime::TlsHandle::from_raw(0x8000),
            crate::runtime::AddressSpaceToken::from_raw(0x9000),
        )
    };
    let extension = unsafe {
        // SAFETY: the callback owns no external data and only records its drop.
        ThreadExtension::new(0, &RETRY_RELEASE_EXTENSION_OPS)
    };
    let result = system.create_thread(unsafe {
        // SAFETY: this specification uniquely owns every modeled resource.
        ThreadSpec::new(SchedulePolicy::default())
            .with_affinity(CpuSet::empty(2))
            .with_extension(extension)
            .with_resources(resources)
    });

    assert_eq!(result.unwrap_err(), TaskError::InvalidConfiguration);
    assert_eq!(
        crate::test_runtime::resource_release_events(),
        [
            ResourceReleaseEvent::DestroyContext,
            ResourceReleaseEvent::DeallocateTls,
            ResourceReleaseEvent::DeallocateStack,
            ResourceReleaseEvent::DropExtension,
            ResourceReleaseEvent::DestroyAddressSpace,
        ],
        "active-mm lifetime must not retain thread-private resources"
    );

    let doorbell = system.task_work_doorbell();
    assert!(
        doorbell.claim_pending().is_none(),
        "a busy active-mm token must wait without an initial worker retry"
    );

    crate::test_runtime::configure_address_space_destroy(AddressSpaceDestroyOutcome::Released);
    system.publish_resource_release_ready();
    assert!(doorbell.claim_pending().is_some());
    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(
        crate::test_runtime::resource_release_events(),
        [
            ResourceReleaseEvent::DestroyContext,
            ResourceReleaseEvent::DeallocateTls,
            ResourceReleaseEvent::DeallocateStack,
            ResourceReleaseEvent::DropExtension,
            ResourceReleaseEvent::DestroyAddressSpace,
            ResourceReleaseEvent::DestroyAddressSpace,
        ]
    );
    crate::test_runtime::configure_address_space_reclaim_arm(AddressSpaceReclaimArmOutcome::Ready);
    crate::test_runtime::configure_resource_release(RuntimeStatus::Unsupported);
}

#[test]
fn current_address_space_detach_transfers_the_unique_token_once() {
    crate::test_runtime::configure_address_space_destroy(AddressSpaceDestroyOutcome::Released);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let resources = unsafe {
        // SAFETY: this fixture transfers one modeled address-space token into
        // the bootstrap record and takes it back through the detach operation.
        ThreadResources::new(
            ExecutionContextHandle::NONE,
            crate::runtime::StackHandle::NONE,
            crate::runtime::TlsHandle::NONE,
            crate::runtime::AddressSpaceToken::from_raw(0xb000),
        )
    };
    system
        .install_bootstrap_thread(cpu.as_mut(), unsafe {
            // SAFETY: the specification is the sole owner of `resources`.
            ThreadSpec::new(SchedulePolicy::default()).with_resources(resources)
        })
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let address_space = system.detach_current_address_space(cpu.as_mut()).unwrap();
    assert_eq!(address_space.handle().into_raw(), 0xb000);
    assert_eq!(
        system.detach_current_address_space(cpu.as_mut()),
        Err(TaskError::InvalidConfiguration),
        "one running task may transfer its address-space token only once"
    );

    system.release_address_space_token(address_space);
}

#[test]
fn thread_private_resource_release_failure_is_fatal() {
    let failure = std::thread::spawn(|| {
        crate::test_runtime::configure_resource_release(RuntimeStatus::Busy);
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let resources = unsafe {
            // SAFETY: the unit runtime models a unique context destruction right.
            ThreadResources::new(
                ExecutionContextHandle::from_raw(0xa000),
                crate::runtime::StackHandle::NONE,
                crate::runtime::TlsHandle::NONE,
                crate::runtime::AddressSpaceToken::NONE,
            )
        };

        system.release_unpublished_resources(resources);
    })
    .join();
    assert!(failure.is_err(), "runtime teardown failure must be fatal");
}

#[test]
fn busy_address_space_reclaim_waits_for_the_runtime_readiness_edge() {
    crate::test_runtime::configure_address_space_destroy(AddressSpaceDestroyOutcome::Active);
    crate::test_runtime::configure_address_space_reclaim_arm(AddressSpaceReclaimArmOutcome::Armed);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let resources = unsafe {
        // SAFETY: the unit runtime treats this unique token as an inert owning
        // identity until the configured reclaim operation succeeds.
        ThreadResources::new(
            ExecutionContextHandle::NONE,
            crate::runtime::StackHandle::NONE,
            crate::runtime::TlsHandle::NONE,
            crate::runtime::AddressSpaceToken::from_raw(0x9000),
        )
    };

    system.release_unpublished_resources(resources);
    let doorbell = system.task_work_doorbell();
    assert!(
        doorbell.claim_pending().is_none(),
        "a busy active-mm token must wait for a readiness edge without polling"
    );

    crate::test_runtime::configure_address_space_destroy(AddressSpaceDestroyOutcome::Released);
    system.publish_resource_release_ready();
    assert!(doorbell.claim_pending().is_some());
    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);

    crate::test_runtime::configure_address_space_reclaim_arm(AddressSpaceReclaimArmOutcome::Ready);
}

#[test]
fn stale_address_space_readiness_edge_does_not_spin_the_task_worker() {
    use crate::test_runtime::ResourceReleaseEvent;

    crate::test_runtime::clear_resource_release_events();
    crate::test_runtime::configure_address_space_destroy(AddressSpaceDestroyOutcome::Active);
    crate::test_runtime::configure_address_space_reclaim_arm(AddressSpaceReclaimArmOutcome::Armed);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let resources = unsafe {
        // SAFETY: the unit runtime treats this token as a unique address-space
        // destruction right until a later readiness edge makes release succeed.
        ThreadResources::new(
            ExecutionContextHandle::NONE,
            crate::runtime::StackHandle::NONE,
            crate::runtime::TlsHandle::NONE,
            crate::runtime::AddressSpaceToken::from_raw(0xa000),
        )
    };

    system.release_unpublished_resources(resources);
    system.publish_resource_release_ready();
    assert!(system.task_work_doorbell().claim_pending().is_some());

    let batch = system.service_deferred_task_work(64).unwrap();
    assert_eq!(
        batch.processed(),
        0,
        "a stale readiness edge must re-arm the active-mm waiter instead of reporting progress"
    );
    assert_eq!(
        crate::test_runtime::resource_release_events(),
        [
            ResourceReleaseEvent::DestroyAddressSpace,
            ResourceReleaseEvent::DestroyAddressSpace,
        ],
        "one readiness edge permits exactly one destroy attempt"
    );

    crate::test_runtime::configure_address_space_reclaim_arm(AddressSpaceReclaimArmOutcome::Ready);
    crate::test_runtime::configure_address_space_destroy(AddressSpaceDestroyOutcome::Released);
    system.publish_resource_release_ready();
    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
}

unsafe extern "Rust" fn record_release_order_extension_drop(_data: usize) {
    crate::test_runtime::record_resource_release_event(
        crate::test_runtime::ResourceReleaseEvent::DropExtension,
    );
}

#[test]
fn generation_rejects_a_stale_registry_identity() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let first = system
        .create_thread(ThreadSpec::new(Default::default()))
        .unwrap();
    let first_id = first.id();
    system.mark_exited(first_id).unwrap();
    drop(first);
    system.reap_thread(first_id).unwrap();
    let second = system
        .create_thread(ThreadSpec::new(Default::default()))
        .unwrap();
    assert_eq!(first_id.slot(), second.id().slot());
    assert_eq!(system.thread_state(first_id), Err(TaskError::StaleThreadId));
}

#[test]
fn detached_reaper_waits_for_the_last_external_handle() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let thread = system
        .create_thread(ThreadSpec::new(Default::default()))
        .unwrap();
    let id = thread.id();
    system.mark_exited(id).unwrap();

    assert_eq!(system.reap_unreferenced_exited(1).unwrap(), 0);
    drop(thread);
    assert_eq!(system.reap_unreferenced_exited(1).unwrap(), 1);
    assert_eq!(system.thread_state(id), Err(TaskError::StaleThreadId));
}

#[test]
fn detached_reaper_visits_only_exited_candidates() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let live = (0..32)
        .map(|_| {
            system
                .create_thread(ThreadSpec::new(SchedulePolicy::default()))
                .unwrap()
        })
        .collect::<Vec<_>>();
    let exited = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.mark_exited(exited.id()).unwrap();
    drop(exited);

    registry::reset_reaper_record_visits();
    assert_eq!(system.reap_unreferenced_exited(1).unwrap(), 1);
    assert_eq!(
        registry::reaper_record_visits(),
        1,
        "unrelated live threads must not participate in detached reaping"
    );

    drop(live);
}

#[test]
fn thread_creation_preallocates_exit_candidate_capacity() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let threads = (0..64)
        .map(|_| {
            system
                .create_thread(ThreadSpec::new(SchedulePolicy::default()))
                .unwrap()
        })
        .collect::<Vec<_>>();
    let reserved_capacity = system.state.lock().exited_work.capacity();
    assert!(reserved_capacity >= threads.len());

    for thread in &threads {
        system.mark_exited(thread.id()).unwrap();
    }
    let state = system.state.lock();
    assert_eq!(state.exited_work.candidate_count(), threads.len());
    assert_eq!(
        state.exited_work.capacity(),
        reserved_capacity,
        "thread exit and switch-tail publication must not grow storage"
    );
}

#[test]
fn last_non_idle_exit_publishes_work_only_after_switch_tail() {
    EXIT_CALLBACK_INVOCATIONS.store(0, Ordering::Release);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let extension = unsafe {
        // SAFETY: the callback table ignores its scalar payload and records
        // only ordinary-context exit invocation.
        ThreadExtension::new(0, &EXIT_CALLBACK_TEST_OPS)
    };
    let exiting = system
        .install_bootstrap_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()).with_extension(extension),
        )
        .unwrap();
    let exiting_id = exiting.id();
    let idle = system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    drop(exiting);
    drop(idle);

    let decision = system.exit_current_at(cpu.as_mut(), 0).unwrap();
    assert_eq!(decision.next(), cpu.idle().unwrap());
    assert!(
        !system.deferred_task_work_pending(),
        "the outgoing stack remains on_cpu until switch tail"
    );
    assert_eq!(EXIT_CALLBACK_INVOCATIONS.load(Ordering::Acquire), 0);

    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert!(system.deferred_task_work_pending());
    let batch = system.service_deferred_task_work(64).unwrap();
    assert!(batch.made_progress());
    assert_eq!(EXIT_CALLBACK_INVOCATIONS.load(Ordering::Acquire), 1);
    assert_eq!(
        system.thread_state(exiting_id),
        Err(TaskError::StaleThreadId),
        "the dedicated service pass must also reap the detached record"
    );
}

#[test]
fn deadline_callback_dispatch_visits_only_pending_candidates() {
    CANDIDATE_DEADLINE_CALLBACKS.store(0, Ordering::Release);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let fillers = (0..32)
        .map(|_| {
            system
                .create_thread(ThreadSpec::new(SchedulePolicy::default()))
                .unwrap()
        })
        .collect::<Vec<_>>();
    let deadline = SchedulePolicy::deadline(
        DeadlinePolicy::new(10, 20, 100, DeadlineFlags::DL_OVERRUN).unwrap(),
    );
    let extension = unsafe {
        // SAFETY: the static callback only increments an atomic test counter.
        ThreadExtension::new(0, &CANDIDATE_DEADLINE_TEST_EXTENSION_OPS)
    };
    let deadline_thread = system
        .create_thread(ThreadSpec::new(deadline).with_extension(extension))
        .unwrap();
    for thread in fillers.iter().chain(core::iter::once(&deadline_thread)) {
        system.make_ready(thread.id()).unwrap();
        system.enqueue_at(cpu.as_mut(), thread.id(), 0).unwrap();
    }
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        deadline_thread.id()
    );
    assert!(
        system
            .charge_current_at(cpu.as_mut(), 10, 10, 0)
            .unwrap()
            .deadline_overrun()
    );
    system.schedule_at(cpu.as_mut(), 10).unwrap();

    registry::reset_deadline_callback_record_visits();
    assert_eq!(system.dispatch_deadline_overruns(1), Ok(1));
    assert_eq!(CANDIDATE_DEADLINE_CALLBACKS.load(Ordering::Acquire), 1);
    assert_eq!(
        registry::deadline_callback_record_visits(),
        1,
        "callback dispatch must visit the published candidate, not every live thread"
    );
}

#[test]
fn deadline_overrun_published_during_callback_gets_a_fresh_delivery() {
    let callback = Box::leak(Box::new(DeadlineCallbackRace {
        entered: std::sync::Barrier::new(2),
        release: std::sync::Barrier::new(2),
        invocations: AtomicUsize::new(0),
    }));
    let system = Arc::new(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let extension = unsafe {
        // SAFETY: the leaked callback fixture outlives the thread extension.
        ThreadExtension::new(
            core::ptr::from_ref(callback).expose_provenance(),
            &RACING_DEADLINE_TEST_EXTENSION_OPS,
        )
    };
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_extension(extension))
        .unwrap();
    let core = {
        let state = system.state.lock();
        state
            .thread_record(thread.id())
            .unwrap()
            .sched
            .lock()
            .deadline
            .overrun_events = 1;
        Arc::clone(&state.thread_record(thread.id()).unwrap().core)
    };
    system.publish_deadline_overrun_work(core);

    let service_system = Arc::clone(&system);
    let service = std::thread::spawn(move || service_system.dispatch_deadline_overruns(1));
    callback.entered.wait();
    let core = {
        let state = system.state.lock();
        state
            .thread_record(thread.id())
            .unwrap()
            .sched
            .lock()
            .deadline
            .overrun_events += 1;
        Arc::clone(&state.thread_record(thread.id()).unwrap().core)
    };
    system.publish_deadline_overrun_work(core);
    callback.release.wait();

    assert_eq!(service.join().unwrap(), Ok(1));
    assert_eq!(callback.invocations.load(Ordering::Acquire), 1);
    assert_eq!(system.dispatch_deadline_overruns(1), Ok(1));
    assert_eq!(callback.invocations.load(Ordering::Acquire), 2);
    assert_eq!(system.dispatch_deadline_overruns(1), Ok(0));
}

#[test]
fn deadline_task_work_rotates_across_registry_slots() {
    ROTATING_DEADLINE_CALLBACKS.store(0, Ordering::Release);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let low_slot = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let high_slot = system
        .create_thread(
            ThreadSpec::new(SchedulePolicy::default()).with_extension(unsafe {
                // SAFETY: the static callbacks accept the scalar test payload.
                ThreadExtension::new(0, &ROTATING_DEADLINE_TEST_OPS)
            }),
        )
        .unwrap();
    let high_slot_id = high_slot.id();
    let (low_core, high_core) = {
        let state = system.state.lock();
        state
            .thread_record(low_slot.id())
            .unwrap()
            .sched
            .lock()
            .deadline
            .overrun_events = 1;
        state
            .thread_record(high_slot_id)
            .unwrap()
            .sched
            .lock()
            .deadline
            .overrun_events = 1;
        (
            Arc::clone(&state.thread_record(low_slot.id()).unwrap().core),
            Arc::clone(&state.thread_record(high_slot_id).unwrap().core),
        )
    };
    system.publish_deadline_overrun_work(low_core);
    system.publish_deadline_overrun_work(high_core);
    system.mark_exited(high_slot_id).unwrap();
    drop(high_slot);

    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(system.thread_state(high_slot_id), Ok(ThreadState::Exited));
    let low_core = {
        let state = system.state.lock();
        state
            .thread_record(low_slot.id())
            .unwrap()
            .sched
            .lock()
            .deadline
            .overrun_events += 1;
        Arc::clone(&state.thread_record(low_slot.id()).unwrap().core)
    };
    system.publish_deadline_overrun_work(low_core);

    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(ROTATING_DEADLINE_CALLBACKS.load(Ordering::Acquire), 1);
    assert_eq!(system.thread_state(high_slot_id), Ok(ThreadState::Exited));

    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(system.thread_state(high_slot_id), Ok(ThreadState::Exited));
    assert_eq!(system.service_deferred_task_work(1).unwrap().processed(), 1);
    assert_eq!(
        system.thread_state(high_slot_id),
        Err(TaskError::StaleThreadId),
        "a continuously replenished low slot must not starve exit and reaping"
    );
}

#[test]
fn last_handle_drop_publishes_after_its_strong_reference_is_released() {
    let system = Arc::new(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let thread_id = thread.id();
    system.mark_exited(thread_id).unwrap();

    assert!(system.task_work.claim_pending().is_some());
    let first_pass = system.service_deferred_task_work(64).unwrap();
    assert_eq!(first_pass.reaped_threads, 0);

    let barrier: &'static crate::task_work::TestPublishBarrier =
        Box::leak(Box::new(crate::task_work::TestPublishBarrier::new()));
    system.task_work.install_test_publish_barrier(barrier);
    let dropper = std::thread::spawn(move || drop(thread));
    barrier.wait_until_entered();

    assert!(system.task_work.claim_pending().is_some());
    let racing_pass = system.service_deferred_task_work(64).unwrap();
    barrier.release();
    dropper.join().unwrap();

    assert_eq!(
        racing_pass.reaped_threads, 1,
        "the drop notification must become visible only after its Arc count decreases"
    );
    assert_eq!(
        system.thread_state(thread_id),
        Err(TaskError::StaleThreadId)
    );
}

#[test]
fn public_task_work_consumers_cannot_bypass_single_consumer_ownership() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let _consumer = system.task_work.try_claim_consumer().unwrap();

    assert_eq!(
        system.dispatch_exit_callbacks(1),
        Err(TaskError::ThreadBusy)
    );
    assert_eq!(
        system.reap_unreferenced_exited(1),
        Err(TaskError::ThreadBusy)
    );
    assert_eq!(
        system.service_deferred_task_work(1),
        Err(TaskError::ThreadBusy)
    );
}

#[test]
fn switch_handoff_core_reference_does_not_block_detached_reaping() {
    let system = Arc::new(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let exiting = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let exiting_id = exiting.id();
    let idle = system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    drop(exiting);
    drop(idle);

    let decision = system.exit_current_at(cpu.as_mut(), 0).unwrap();
    assert_eq!(decision.next(), cpu.idle().unwrap());
    let barrier: &'static crate::task_work::TestPublishBarrier =
        Box::leak(Box::new(crate::task_work::TestPublishBarrier::new()));
    system.task_work.install_test_publish_barrier(barrier);
    let service_system = Arc::clone(&system);
    let service = std::thread::spawn(move || {
        barrier.wait_until_entered();
        assert!(service_system.task_work.claim_pending().is_some());
        let batch = service_system.service_deferred_task_work(64).unwrap();
        barrier.release();
        batch
    });

    system.complete_context_switch(cpu.as_mut()).unwrap();
    let racing_pass = service.join().unwrap();
    assert_eq!(
        racing_pass.reaped_threads, 1,
        "scheduler-internal core references must not count as external lifetime leases"
    );
    assert_eq!(
        system.thread_state(exiting_id),
        Err(TaskError::StaleThreadId)
    );
}

#[test]
fn owned_reap_returns_handle_until_other_wake_references_are_gone() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let thread = system
        .create_thread(ThreadSpec::new(Default::default()))
        .unwrap();
    let id = thread.id();
    let late_wake = thread.wake_handle();
    system.mark_exited(id).unwrap();

    let error = system.reap_thread_handle(thread).unwrap_err();
    assert_eq!(error.task_error(), TaskError::ThreadBusy);
    let thread = error.into_retry_handle();
    assert_eq!(system.reap_unreferenced_exited(1).unwrap(), 0);

    drop(late_wake);
    system.reap_thread_handle(thread).unwrap();
    assert_eq!(system.thread_state(id), Err(TaskError::StaleThreadId));
}

#[test]
fn current_entry_can_release_its_lookup_lease_before_nonreturning_exit() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    // SAFETY: the no-op callback table accepts the zero-sized test payload.
    let extension = unsafe { ThreadExtension::new(0, &DEADLINE_TEST_EXTENSION_OPS) };
    let thread = system
        .create_thread(ThreadSpec::new(Default::default()).with_extension(extension))
        .unwrap();
    let lease = system
        .thread_extension_lease(thread.clone())
        .unwrap()
        .unwrap();

    let view = unsafe {
        // SAFETY: the registry record retains the extension until this
        // test marks and reaps the current-entry model below.
        lease.release_for_current_thread_entry()
    };
    assert!(core::ptr::eq(view.ops(), &DEADLINE_TEST_EXTENSION_OPS));
    system.mark_exited(thread.id()).unwrap();
    assert!(
        system
            .service_deferred_task_work(1)
            .unwrap()
            .made_progress()
    );
    system.reap_thread_handle(thread).unwrap();
}

#[test]
fn reaper_waits_until_embedded_sleep_timer_is_detached() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let thread = system
        .create_thread(ThreadSpec::new(Default::default()))
        .unwrap();
    let id = thread.id();
    thread.core.register_sleep_timer(CpuId::new(0), 1);
    system.mark_exited(id).unwrap();

    assert_eq!(system.reap_thread(id), Err(TaskError::ThreadBusy));
    assert!(thread.core.complete_sleep_timer(1));
    system.reap_thread_handle(thread).unwrap();
}

#[test]
fn exited_context_cannot_be_reaped_before_switch_tail() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let bootstrap = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let exiting = bootstrap.id();
    drop(bootstrap);
    let decision = system.exit_current_at(cpu.as_mut(), 0).unwrap();
    assert_ne!(decision.next(), exiting);
    assert_eq!(system.reap_thread(exiting), Err(TaskError::ThreadBusy));

    system.complete_context_switch(cpu.as_mut()).unwrap();
    system.reap_thread(exiting).unwrap();
}

#[test]
fn prepared_exit_rejects_new_remote_affinity_delivery() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let exiting = system
        .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let exiting_id = exiting.id();
    let exiting_core = Arc::downgrade(&exiting.core);
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    crate::test_runtime::set_scheduler_ns(0);
    let _clock = system.sample_owner_rq_clock(cpu0.as_ref().get_ref());
    let permit = system
        .prepare_current_exit_inner(cpu0.as_mut(), false)
        .unwrap();
    let mut target_only = CpuSet::empty(2);
    assert!(target_only.insert(CpuId::new(1)));
    system.set_affinity(exiting_id, target_only).unwrap();
    assert!(
        !cpu0.has_remote_work(),
        "a prepared exit must reject new affinity delivery reservations"
    );

    crate::test_runtime::set_scheduler_ns(1);
    let _clock = system.sample_owner_rq_clock(cpu0.as_ref().get_ref());
    system
        .commit_current_exit_after_owner_drain(cpu0.as_mut(), permit)
        .unwrap();
    drop(exiting);
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    assert_eq!(cpu1.load_summary().queued_count(), 0);
    let core = exiting_core
        .upgrade()
        .expect("the registry still retains the exited core before reaping");
    assert_eq!(core.scheduler_inbox_delivery_count(), 0);
    assert!(!core.sched().lock().placement.has_pending_migration());
    drop(core);
    assert!(
        system
            .service_deferred_task_work(1)
            .unwrap()
            .made_progress()
    );
    assert_eq!(
        system.thread_state(exiting_id),
        Err(TaskError::StaleThreadId)
    );
    assert!(
        exiting_core.upgrade().is_none(),
        "reaping must release the prepared exit permit's core reference"
    );
}

#[test]
fn prepared_exit_rejects_a_synchronous_deadline_policy_transaction() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let exiting = system
        .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let exiting_id = exiting.id();
    let exiting_core = Arc::downgrade(&exiting.core);
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    crate::test_runtime::set_scheduler_ns(0);
    let _clock = system.sample_owner_rq_clock(cpu0.as_ref().get_ref());
    let permit = system
        .prepare_current_exit_inner(cpu0.as_mut(), false)
        .unwrap();
    let deadline =
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 10, DeadlineFlags::NONE).unwrap());
    assert_eq!(
        system.set_thread_policy(exiting_id, deadline),
        Err(TaskError::NotReady)
    );
    assert!(
        !cpu0.has_remote_work(),
        "a prepared exit must reject new policy delivery reservations"
    );

    crate::test_runtime::set_scheduler_ns(1);
    let _clock = system.sample_owner_rq_clock(cpu0.as_ref().get_ref());
    system
        .commit_current_exit_after_owner_drain(cpu0.as_mut(), permit)
        .unwrap();
    drop(exiting);
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    assert!(
        cpu0.deadline_members_are_empty_for_test(),
        "rejected policy delivery must not register an exited Deadline member"
    );
    let core = exiting_core
        .upgrade()
        .expect("the registry still retains the exited core before reaping");
    assert_eq!(core.scheduler_inbox_delivery_count(), 0);
    drop(core);
    assert_eq!(
        system.deadline_activity(exiting_id),
        Err(TaskError::InvalidConfiguration),
        "prepared exit must not leave an active Deadline entity"
    );
    assert!(
        system
            .service_deferred_task_work(1)
            .unwrap()
            .made_progress()
    );
    assert_eq!(
        system.thread_state(exiting_id),
        Err(TaskError::StaleThreadId)
    );
    assert!(
        exiting_core.upgrade().is_none(),
        "reaping must release the prepared exit permit's core reference"
    );
}

#[test]
fn failed_owner_batch_releases_all_detached_payloads() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let thread_id = thread.id();
    let thread_core = Arc::downgrade(&thread.core);
    system.make_ready(thread_id).unwrap();
    system.enqueue_at(cpu1.as_mut(), thread_id, 0).unwrap();

    assert!(thread.core.reserve_scheduler_inbox_delivery());
    let pointer = Arc::as_ptr(&thread.core);
    unsafe {
        // SAFETY: this test follows the production publication contract and
        // transfers the retained count into the migration inbox below.
        Arc::increment_strong_count(pointer);
    }
    let node = unsafe {
        // SAFETY: the retained Arc pins the embedded migration node until
        // the owner detaches and reconstructs this payload.
        Pin::new_unchecked((*pointer).migration_node())
    };
    let malformed_owner = InboxMessage::migration_with_payload(
        thread_id,
        CpuId::new(0),
        CpuId::new(1),
        thread_id.generation() as u64,
        1_024,
        pointer.expose_provenance(),
    );
    assert_eq!(
        cpu1.remote().publish_owner_control(node, malformed_owner),
        PublishResult::Published
    );
    system
        .set_thread_policy(
            thread_id,
            SchedulePolicy::fifo(RtPriority::new(80).unwrap()),
        )
        .unwrap();
    assert_eq!(
        system.drain_owner_control_at(cpu1.as_mut(), 1),
        Err(TaskError::InvalidConfiguration)
    );
    assert_eq!(thread.core.scheduler_inbox_delivery_count(), 0);

    system.dequeue(cpu1.as_mut(), thread_id).unwrap();
    system.mark_exited(thread_id).unwrap();
    drop(thread);
    assert!(
        system
            .service_deferred_task_work(1)
            .unwrap()
            .made_progress()
    );
    assert_eq!(
        system.thread_state(thread_id),
        Err(TaskError::StaleThreadId),
        "an error in one detached message must release later retained payloads"
    );
    assert!(
        thread_core.upgrade().is_none(),
        "the detached batch guard must release every suffix payload Arc"
    );
}

#[test]
fn invalid_switch_tail_state_is_rejected_before_runtime_commit() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let bootstrap = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let exiting = bootstrap.id();
    let exiting_core = Arc::clone(&bootstrap.core);
    drop(bootstrap);
    system.exit_current_at(cpu.as_mut(), 0).unwrap();
    crate::test_runtime::reset_context_switch_tail_count();

    exiting_core
        .sched()
        .lock()
        .placement
        .inject_missing_on_cpu();
    assert!(matches!(
        system.complete_context_switch(cpu.as_mut()),
        Err(TaskError::InvalidConfiguration)
    ));
    assert_eq!(
        crate::test_runtime::context_switch_tail_count(),
        0,
        "runtime tail must not commit before scheduler handoff validation"
    );
    assert!(
        cpu.switch_handoff().is_some(),
        "a rejected pre-commit handoff must remain retryable"
    );

    exiting_core
        .sched()
        .lock()
        .placement
        .inject_exiting_on_cpu(cpu.owner());
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert_eq!(crate::test_runtime::context_switch_tail_count(), 1);
    drop(exiting_core);
    system.reap_thread(exiting).unwrap();
}

#[test]
fn switch_tail_defers_exit_callback_until_scheduler_guards_are_released() {
    EXIT_CALLBACK_INVOCATIONS.store(0, Ordering::Release);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    // SAFETY: the test callback table owns no external resource and treats
    // the zero payload as an opaque value.
    let extension = unsafe { ThreadExtension::new(0, &EXIT_CALLBACK_TEST_OPS) };
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

    let exiting = bootstrap.id();
    drop(bootstrap);
    system.exit_current_at(cpu.as_mut(), 0).unwrap();
    system.complete_context_switch(cpu.as_mut()).unwrap();

    assert_eq!(
        EXIT_CALLBACK_INVOCATIONS.load(Ordering::Acquire),
        0,
        "context-switch tail must not invoke task-context exit callbacks"
    );
    assert_eq!(system.dispatch_exit_callbacks(1).unwrap(), 1);
    assert_eq!(EXIT_CALLBACK_INVOCATIONS.load(Ordering::Acquire), 1);
    assert_eq!(system.thread_state(exiting), Ok(ThreadState::Exited));
}

#[test]
fn scheduler_work_without_preemption_preserves_current_dispatch() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let control_drains = cpu.remote().owner_control_inbox().drain_attempts();
    dispatch::reset_owner_dispatch_constructions();
    cpu.request_scheduler_work();
    assert!(matches!(
        system.schedule_if_requested_at(cpu.as_mut(), 1).unwrap(),
        SchedulerOutcome::Quiescent
    ));
    assert_eq!(
        cpu.remote().owner_control_inbox().drain_attempts(),
        control_drains,
        "a work-only safe point must not enter an empty policy inbox"
    );
    assert_eq!(
        dispatch::owner_dispatch_constructions(),
        0,
        "a work-only safe point must retain the running dispatch instead of rebuilding it"
    );
    system
        .charge_current_at(cpu.as_mut(), 2, 1, 0)
        .expect("scheduler-only work must not discard the running dispatch");
}

#[test]
fn rq_commit_preserves_a_request_published_after_the_decision_claim() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    cpu.request_scheduler_work();
    let remote = Arc::clone(cpu.remote());
    let initial_request = remote.claim_scheduler_request();
    let mut transaction = OwnerRqTxn::begin(&system, &remote);
    transaction.adopt_scheduler_request(initial_request);
    let decision_request = transaction.merge_scheduler_request();
    assert!(!decision_request.preempt_requested());

    remote.request_reschedule();
    transaction.commit_and_acknowledge_scheduler_request();

    assert!(
        remote.needs_reschedule(),
        "a generation published after the scheduler decision must remain pending for the next pass",
    );
}

#[test]
fn running_rt_task_has_one_active_schedule_owner() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    system
        .set_thread_policy(
            running.id(),
            SchedulePolicy::round_robin(RtPriority::new(1).unwrap()),
        )
        .unwrap();
    system.drain_owner_control_at(cpu.as_mut(), 0).unwrap();

    let thread_owner = {
        let state = system.state.lock();
        let sched = state.thread_record(running.id()).unwrap().sched.lock();
        usize::from(sched.policy.owns_active())
    };
    let rq_owners = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .debug_schedule_owner_count(running.id());
    assert_eq!(
        thread_owner + rq_owners,
        1,
        "one runnable entity must have exactly one mutable schedule-state owner",
    );
}

#[test]
fn policy_update_commits_without_owner_control_delivery() {
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

    let control_drains = cpu.remote().owner_control_inbox().drain_attempts();
    system
        .set_thread_policy(
            running.id(),
            SchedulePolicy::fifo(RtPriority::new(1).unwrap()),
        )
        .unwrap();
    assert_eq!(
        system.thread_policy(running.id()).unwrap(),
        SchedulePolicy::fifo(RtPriority::new(1).unwrap())
    );
    assert!(!cpu.remote().owner_control_inbox().has_pending());

    system.schedule_if_requested_at(cpu.as_mut(), 1).unwrap();

    assert_eq!(
        cpu.remote().owner_control_inbox().drain_attempts(),
        control_drains,
        "a synchronous policy transaction must not enter the owner-control inbox"
    );
    assert!(
        !cpu.remote().owner_control_inbox().has_pending(),
        "a policy transaction must not create owner-control work"
    );
}

#[test]
fn owner_policy_apply_notifies_extension_at_the_owner_timestamp() {
    POLICY_APPLIED_CALLBACKS.store(0, Ordering::Release);
    POLICY_APPLIED_AT_NS.store(0, Ordering::Release);

    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let extension = unsafe {
        // SAFETY: the static callbacks retain no data and run synchronously
        // while this TaskSystem owns the extension.
        ThreadExtension::new(0, &DEADLINE_TEST_EXTENSION_OPS)
            .with_running_policy_applied_hook(record_policy_applied)
    };
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_extension(extension))
        .unwrap();
    system.make_ready(thread.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), thread.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        thread.id()
    );

    crate::test_runtime::set_scheduler_ns_for_cpu(0, 1_050_000);
    system
        .set_thread_policy(
            thread.id(),
            SchedulePolicy::fair(Nice::new(5).unwrap(), FairMode::Normal),
        )
        .unwrap();
    assert_eq!(POLICY_APPLIED_CALLBACKS.load(Ordering::Acquire), 1);
    assert_eq!(POLICY_APPLIED_AT_NS.load(Ordering::Acquire), 1_050_000);
}

#[test]
fn inactive_policy_apply_does_not_invoke_an_execution_owner_hook() {
    POLICY_APPLIED_CALLBACKS.store(0, Ordering::Release);

    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let extension = unsafe {
        // SAFETY: the static callbacks retain no data and run synchronously
        // while this TaskSystem owns the extension.
        ThreadExtension::new(0, &DEADLINE_TEST_EXTENSION_OPS)
            .with_running_policy_applied_hook(record_policy_applied)
    };
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_extension(extension))
        .unwrap();
    let updated_policy = SchedulePolicy::fair(Nice::new(5).unwrap(), FairMode::Normal);

    system
        .set_thread_policy(thread.id(), updated_policy)
        .unwrap();

    assert_eq!(POLICY_APPLIED_CALLBACKS.load(Ordering::Acquire), 0);

    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    system.make_ready(thread.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), thread.id(), 0).unwrap();
    let decision = system.schedule_at(cpu.as_mut(), 0).unwrap();

    assert_eq!(decision.next(), thread.id());
    assert_eq!(thread.policy(), updated_policy);
}

#[test]
fn fair_policy_update_reweights_lag_without_resetting_service_history() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let first = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let second = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    for thread in [&first, &second] {
        system.make_ready(thread.id()).unwrap();
        system.enqueue_at(cpu.as_mut(), thread.id(), 0).unwrap();
    }

    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        first.id()
    );
    system
        .charge_current_at(cpu.as_mut(), 400_000, 400_000, 0)
        .unwrap();
    assert_eq!(
        system
            .yield_current_at(cpu.as_mut(), 400_000)
            .unwrap()
            .next(),
        second.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    system
        .charge_current_at(cpu.as_mut(), 800_000, 400_000, 0)
        .unwrap();
    assert_eq!(
        system
            .yield_current_at(cpu.as_mut(), 800_000)
            .unwrap()
            .next(),
        first.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    system
        .charge_current_at(cpu.as_mut(), 1_050_000, 250_000, 0)
        .unwrap();

    let before = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .current_scheduling_entity()
        .unwrap()
        .fair()
        .unwrap();
    assert_eq!(before.vruntime(), 650_000);
    assert_eq!(before.remaining_request_ns(), 450_000);
    let virtual_time = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .virtual_time();
    assert_eq!(virtual_time, 525_000);

    let nice = Nice::new(5).unwrap();
    system
        .set_thread_policy(first.id(), SchedulePolicy::fair(nice, FairMode::Normal))
        .unwrap();
    system
        .drain_owner_control_at(cpu.as_mut(), 1_050_000)
        .unwrap();
    let reweighted = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .current_scheduling_entity()
        .expect("the running task must retain its rq-owned Fair entity")
        .fair()
        .unwrap();
    let lag =
        (virtual_time as i128 - 650_000_i128) * Nice::ZERO.weight() as i128 / nice.weight() as i128;
    let expected_vruntime = (virtual_time as i128 - lag) as u64;
    let remaining_request_ns = before.remaining_request_ns();
    let expected_remaining_delta =
        (u128::from(remaining_request_ns) * 1024 / u128::from(nice.weight())) as u64;
    assert_eq!(reweighted.vruntime(), expected_vruntime);
    assert_eq!(reweighted.remaining_request_ns(), remaining_request_ns);
    assert_eq!(
        reweighted.virtual_deadline(),
        expected_vruntime + expected_remaining_delta
    );

    system
        .set_thread_policy(first.id(), SchedulePolicy::fair(nice, FairMode::Batch))
        .unwrap();
    system
        .drain_owner_control_at(cpu.as_mut(), 1_050_000)
        .unwrap();
    let batch = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .current_scheduling_entity()
        .expect("the running task must retain its rq-owned Batch entity")
        .fair()
        .unwrap();
    assert_eq!(batch.vruntime(), reweighted.vruntime());
    assert_eq!(batch.virtual_deadline(), reweighted.virtual_deadline());
    assert_eq!(batch.remaining_request_ns(), remaining_request_ns);

    system
        .set_thread_policy(
            first.id(),
            SchedulePolicy::fair(Nice::new(-20).unwrap(), FairMode::Idle),
        )
        .unwrap();
    system
        .drain_owner_control_at(cpu.as_mut(), 1_050_000)
        .unwrap();
    let idle = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .current_scheduling_entity()
        .expect("the running task must retain its rq-owned SCHED_IDLE entity")
        .fair()
        .unwrap();
    assert_eq!(idle.nice(), Nice::LOWEST);
    assert_eq!(idle.remaining_request_ns(), remaining_request_ns);
}

#[test]
fn scheduler_shared_locks_use_the_irq_domain_without_preempt_guards() {
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    for _ in 0..2 {
        let thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.make_ready(thread.id()).unwrap();
        system.enqueue_at(cpu.as_mut(), thread.id(), 0).unwrap();
    }
    system.schedule_at(cpu.as_mut(), 0).unwrap();

    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    let outer_irq_depth = crate::test_runtime::active_irq_guards();
    crate::test_runtime::reset_irq_guard_entries();
    crate::test_runtime::reset_preempt_guard_entries();
    system.yield_current_at(cpu.as_mut(), 1).unwrap();

    assert!(
        crate::test_runtime::irq_guard_entries() > 0,
        "thread state and target runqueue locks must use the IRQ-safe shared lock domain"
    );
    assert_eq!(
        crate::test_runtime::active_irq_guards(),
        outer_irq_depth,
        "every nested irq-safe lock must restore the scheduler baton's IRQ state"
    );
    assert_eq!(
        crate::test_runtime::preempt_guard_entries(),
        0,
        "the scheduler/IRQ owner baton already retains the CPU; internal task-state locks must \
         reuse that ownership instead of nesting ordinary preemption guards"
    );
}

#[test]
fn running_idle_to_normal_transition_uses_both_class_virtual_times() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let idle = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    system
        .set_thread_policy(idle.id(), SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
        .unwrap();
    system.drain_owner_control_at(cpu.as_mut(), 0).unwrap();

    let normal = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(normal.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), normal.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        normal.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    system
        .charge_current_at(cpu.as_mut(), 1_000_000, 1_000_000, 0)
        .unwrap();
    assert_eq!(
        system
            .block_current_at(cpu.as_mut(), 1_000_000)
            .unwrap()
            .next(),
        idle.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    system
        .charge_current_at(cpu.as_mut(), 1_001_000, 1_000, 0)
        .unwrap();

    let normal_virtual_time = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .virtual_time();
    assert_eq!(normal_virtual_time, 1_000_000);
    system
        .set_thread_policy(idle.id(), SchedulePolicy::default())
        .unwrap();
    system
        .drain_owner_control_at(cpu.as_mut(), 1_001_000)
        .unwrap();

    let transitioned = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .current_scheduling_entity()
        .expect("the running policy target must remain rq->curr")
        .fair()
        .unwrap();
    assert_eq!(
        transitioned.vruntime(),
        normal_virtual_time,
        "a zero-lag entity must be rebased onto the destination class's V",
    );
}

#[test]
fn running_normal_to_idle_transition_settles_then_rebases_lag() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let normal = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    system
        .set_thread_policy(
            normal.id(),
            SchedulePolicy::fair(Nice::ZERO, FairMode::Idle),
        )
        .unwrap();
    system
        .drain_owner_control_at(cpu.as_mut(), 1_000_000)
        .unwrap();

    let transitioned = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .current_scheduling_entity()
        .expect("the running policy target must remain rq->curr")
        .fair()
        .unwrap();
    assert_eq!(transitioned.mode(), FairMode::Idle);
    assert_eq!(
        transitioned.vruntime(),
        cpu.lock_run_queue(crate::RunQueueGuardSource::TestInspection)
            .virtual_time_for_mode(FairMode::Idle),
        "settled zero lag must be expressed relative to the destination V domain",
    );
}

#[test]
fn bounded_inbox_remainder_stays_sticky_across_scheduler_entry() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let mut nodes = Vec::with_capacity(cpu.batch_limit() * 2 + 1);
    for slot in 0..=cpu.batch_limit() * 2 {
        nodes.push(Box::pin(crate::inbox::InboxNode::new(
            crate::inbox::InboxKind::OwnerControl,
        )));
        let message = InboxMessage::migration(
            ThreadId::from_parts(slot as u32, 1),
            CpuId::new(0),
            CpuId::new(0),
            slot as u64,
            1_024,
        );
        assert_eq!(
            cpu.remote()
                .publish_owner_control(test_inbox_node(nodes.last().unwrap()), message),
            PublishResult::Published
        );
    }

    let first = system.drain_owner_control_at(cpu.as_mut(), 1).unwrap();
    assert_eq!(first.drained(), cpu.batch_limit());
    assert!(first.pending());
    assert!(
        system
            .schedule_if_requested_at(cpu.as_mut(), 1)
            .unwrap()
            .owner_work_pending()
    );
    assert!(cpu.needs_reschedule());

    let second = system.drain_owner_control_at(cpu.as_mut(), 2).unwrap();
    assert_eq!(second.drained(), 1);
    assert!(!second.pending());
    assert!(matches!(
        system.schedule_if_requested_at(cpu.as_mut(), 2).unwrap(),
        SchedulerOutcome::Quiescent
    ));
    system.charge_current_at(cpu.as_mut(), 3, 1, 0).unwrap();
}

#[test]
fn future_deadline_members_do_not_create_scheduler_work() {
    let system = TaskSystem::new(TaskSystemConfig::new(1).with_batch_limit(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    for _ in 0..2 {
        let policy =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 10, 100, DeadlineFlags::NONE).unwrap());
        let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
        system.make_ready(thread.id()).unwrap();
        system.enqueue_at(cpu.as_mut(), thread.id(), 0).unwrap();
    }

    // Consume the one-time preemption request created by initial placement.
    // Neither future CBS deadline is due, so an ordinary safe point must not
    // manufacture more scheduler work merely to scan the reservation set.
    assert!(cpu.remote().take_preempt_requested());
    let outcome = system.schedule_if_requested_at(cpu.as_mut(), 0).unwrap();
    assert!(matches!(outcome, SchedulerOutcome::Quiescent));
}

#[test]
fn forced_yield_clears_slice_expiration_accounted_by_that_schedule() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let policy = SchedulePolicy::round_robin_with_quantum(RtPriority::new(50).unwrap(), 8).unwrap();
    for _ in 0..2 {
        let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
        system.make_ready(thread.id()).unwrap();
        system.enqueue_at(cpu.as_mut(), thread.id(), 0).unwrap();
    }
    system.schedule_at(cpu.as_mut(), 0).unwrap();

    system.yield_current_at(cpu.as_mut(), 8).unwrap();

    let request_state = cpu.remote().scheduler_request_state_for_test();
    assert!(
        !cpu.remote().take_preempt_requested(),
        "the scheduling transaction already handled the RR expiration it accounted: \
         {request_state:?}"
    );
}

#[test]
fn single_runnable_fair_yield_preserves_the_active_request() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let current = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let before = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .current_scheduling_entity()
        .expect("bootstrap current must be owned by rq->curr")
        .fair()
        .unwrap();
    let decision = system.yield_current_at(cpu.as_mut(), 0).unwrap();
    let after = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .current_scheduling_entity()
        .expect("no-switch yield must retain rq->curr")
        .fair()
        .unwrap();

    assert_eq!(decision.previous(), Some(current.id()));
    assert_eq!(decision.next(), current.id());
    assert_eq!(
        after, before,
        "Linux fair yield is a no-op when no peer is runnable"
    );
    assert!(
        !system
            .charge_current_until_at(cpu.as_mut(), 1, 0)
            .unwrap()
            .slice_expired(),
        "a no-switch yield must preserve the active runtime dispatch"
    );
}

#[test]
fn dequeue_removes_obsolete_scheduler_clockevent() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let contender = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(contender.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), contender.id(), 0).unwrap();

    crate::test_runtime::set_scheduler_ns_for_cpu(cpu.owner().as_u32(), 0);
    assert!(
        cpu.as_mut()
            .next_oneshot_deadline(MonotonicInstant::from_nanos(0).unwrap())
            .is_some(),
        "two fair entities require a scheduler clockevent"
    );
    system.dequeue(cpu.as_mut(), contender.id()).unwrap();

    assert_eq!(
        cpu.as_mut()
            .next_oneshot_deadline(MonotonicInstant::from_nanos(0).unwrap()),
        None,
        "removing the only queued contender must remove the obsolete scheduler clockevent"
    );
}

#[test]
fn running_migration_is_published_only_after_switch_tail() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(thread.id()).unwrap();
    system.enqueue_at(cpu0.as_mut(), thread.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu0.as_mut(), 0).unwrap().next(),
        thread.id()
    );

    let mut target_only = CpuSet::empty(2);
    target_only.insert(CpuId::new(1));
    system.set_affinity(thread.id(), target_only).unwrap();
    system.drain_owner_control_at(cpu0.as_mut(), 1).unwrap();
    let decision = system
        .schedule_if_requested_at(cpu0.as_mut(), 1)
        .unwrap()
        .decision()
        .unwrap();
    assert_eq!(decision.previous(), Some(thread.id()));
    assert!(!cpu1.has_remote_work());

    system.complete_context_switch(cpu0.as_mut()).unwrap();
    assert!(cpu1.has_remote_work());
    let transfer = system.drain_owner_control_at(cpu1.as_mut(), 2).unwrap();
    assert_eq!(transfer.drained(), 1);
    assert!(!transfer.pending());
}

#[test]
fn queued_affinity_migration_captures_lag_before_detaching_from_source() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        let bootstrap = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        system
            .set_thread_policy(
                bootstrap.id(),
                SchedulePolicy::fifo(RtPriority::new(1).unwrap()),
            )
            .unwrap();
        system.drain_owner_control_at(cpu.as_mut(), 0).unwrap();
    }

    let migrating = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let peer = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    for thread in [&migrating, &peer] {
        system.make_ready(thread.id()).unwrap();
        system.enqueue(cpu0.as_mut(), thread.id()).unwrap();
    }

    cpu0.lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .set_virtual_time_for_test(1_000);
    for (thread, vruntime, deadline) in [(&migrating, 900, 950), (&peer, 1_100, 1_200)] {
        let entity = SchedulingEntity::Fair(FairEntity::test_state(
            Nice::ZERO,
            FairMode::Normal,
            vruntime,
            deadline,
        ));
        let remote = Arc::clone(cpu0.remote());
        let mut transaction = OwnerRqTxn::begin(&system, &remote);
        let mut active = transaction.reclassify_task(thread.id()).into_active();
        *active.entity_mut() = entity;
        transaction.enqueue_task(
            QueuedThread::new(
                thread.id(),
                active,
                Arc::clone(&thread.core),
                false,
                true,
                RqTaskMetadata::test(2),
            ),
            EnqueueReason::Preempted,
            None,
        );
        transaction.commit();
    }

    let mut cpu1_only = CpuSet::empty(2);
    assert!(cpu1_only.insert(CpuId::new(1)));
    system.set_affinity(migrating.id(), cpu1_only).unwrap();
    system.drain_owner_control_at(cpu0.as_mut(), 1).unwrap();

    assert_eq!(
        migrating
            .core
            .sched()
            .lock()
            .policy
            .active()
            .entity()
            .fair()
            .unwrap()
            .saved_migration(),
        Some((100, 50)),
        "source vlag must be saved against the weighted average before dequeue changes it"
    );
}

#[test]
fn placement_rejects_an_unrelated_cpu_claim() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(thread.id()).unwrap();
    system.enqueue_at(cpu0.as_mut(), thread.id(), 0).unwrap();

    // A stale remote publication cannot manufacture an independent `on_cpu`
    // owner alongside the runqueue owner.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        system
            .state
            .lock()
            .thread_record_mut(thread.id())
            .unwrap()
            .sched
            .lock()
            .placement
            .set_next_task(CpuId::new(1));
    }));
    assert!(result.is_err());
    assert_eq!(
        system
            .state
            .lock()
            .thread_record(thread.id())
            .unwrap()
            .sched
            .lock()
            .placement
            .queued_cpu(),
        Some(CpuId::new(0))
    );
}

#[test]
fn owner_current_affinity_change_does_not_publish_a_self_request() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }

    let mut target_only = CpuSet::empty(2);
    target_only.insert(CpuId::new(1));
    assert!(
        system
            .set_current_affinity(cpu0.as_mut(), target_only)
            .unwrap()
    );
    assert!(
        !cpu0.has_remote_work(),
        "the owner can commit its migration directly at the next schedule-out"
    );
    assert_eq!(system.thread_state(running.id()), Ok(ThreadState::Running));
    assert_eq!(
        running.assigned_cpu(),
        Some(CpuId::new(0)),
        "an affinity destination must not replace physical source ownership before switch tail"
    );
    assert_eq!(
        running.scheduler_fence_cpu(),
        Some(CpuId::new(0)),
        "a post-publication scheduler fence must rendezvous with the physical owner"
    );

    let decision = system.yield_current_at(cpu0.as_mut(), 1).unwrap();
    assert_eq!(decision.previous(), Some(running.id()));
    assert_ne!(
        decision.next(),
        running.id(),
        "the only runnable Fair owner must not self-dispatch after its affinity excludes this CPU"
    );
    assert_eq!(decision.switch_reason(), SwitchReason::Migrated);
}

#[test]
fn schedule_out_rechecks_affinity_under_the_thread_lock() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let idle0 = system
        .register_idle_thread(
            cpu0.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system
        .register_idle_thread(
            cpu1.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();

    // Model the exact SMP interleaving that used to corrupt CpuLocal:
    // the owner observed no migration, then a remote affinity writer made
    // this CPU illegal before requeue acquired the thread lock. Affinity is
    // authoritative even if a stale migration hint has not been installed.
    let mut target_only = CpuSet::empty(2);
    assert!(target_only.insert(CpuId::new(1)));
    {
        let mut sched = running.core.sched().lock();
        sched.affinity.affinity = Arc::new(target_only);
        sched.placement.request_migration(None);
    }
    running.core.set_wake_cpu_hint(CpuId::new(1));

    let decision = system.schedule_at(cpu0.as_mut(), 1).unwrap();
    assert_eq!(decision.switch_reason(), SwitchReason::Migrated);
    assert_eq!(decision.next(), idle0.id());
    assert_eq!(cpu0.current(), Some(idle0.id()));
    assert_eq!(system.thread_state(running.id()), Ok(ThreadState::Ready));
    assert_eq!(
        running
            .core
            .sched()
            .lock()
            .placement
            .committed_migration_target(),
        Some(CpuId::new(1))
    );
}

#[test]
fn owner_schedule_applies_affinity_before_picking_the_next_task() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let _idle0 = system
        .register_idle_thread(
            cpu0.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system
        .register_idle_thread(
            cpu1.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();

    let candidate = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(candidate.id()).unwrap();
    system.enqueue_at(cpu0.as_mut(), candidate.id(), 0).unwrap();

    let mut cpu1_only = CpuSet::empty(2);
    assert!(cpu1_only.insert(CpuId::new(1)));
    system.set_affinity(candidate.id(), cpu1_only).unwrap();

    let next = system.schedule_at(cpu0.as_mut(), 1).unwrap();
    assert_eq!(next.next(), running.id());
    assert_eq!(
        candidate
            .core
            .sched()
            .lock()
            .placement
            .committed_migration_target(),
        Some(CpuId::new(1))
    );
    assert!(cpu1.has_remote_work());
}

#[test]
fn affinity_update_preserves_an_in_flight_switch_handoff() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let idle0 = system
        .register_idle_thread(
            cpu0.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system
        .register_idle_thread(
            cpu1.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();

    let remote = Arc::clone(cpu0.remote());
    let initial_target = {
        let mut sched = running.core.sched().lock();
        let mut transaction = OwnerRqTxn::begin(&system, &remote);
        let initial_target = system.schedule_out_owner_running_in_rq(
            cpu0.as_mut(),
            &mut transaction,
            Arc::clone(&running.core),
            &mut sched,
            1,
            EnqueueReason::Yield,
        );
        transaction.commit();
        initial_target
    };
    assert!(initial_target.migration.is_none());
    {
        let sched = running.core.sched().lock();
        assert_eq!(sched.placement.queued_cpu(), Some(CpuId::new(0)));
        assert_eq!(sched.placement.on_cpu(), Some(CpuId::new(0)));
        assert!(!sched.placement.has_pending_migration());
    }

    // The remote setter may update task metadata now, but it must not rewrite
    // the queued destination already owned by the outgoing switch transaction.
    let mut cpu1_only = CpuSet::empty(2);
    assert!(cpu1_only.insert(CpuId::new(1)));
    system.set_affinity(running.id(), cpu1_only).unwrap();
    {
        let sched = running.core.sched().lock();
        assert_eq!(sched.placement.queued_cpu(), Some(CpuId::new(0)));
        assert_eq!(sched.placement.on_cpu(), Some(CpuId::new(0)));
        assert!(!sched.placement.has_pending_migration());
    }

    crate::test_runtime::set_scheduler_ns(2);
    let next = {
        let mut transaction = OwnerRqTxn::begin(&system, &remote);
        let next =
            system.pick_owner_next_in_rq(cpu0.as_mut(), &mut transaction, Some(running.id()));
        transaction.commit();
        next
    };
    assert_eq!(next.core.id(), running.id());
    TaskSystem::stage_switch_handoff(
        cpu0.as_mut(),
        Some(running.id()),
        Some(Arc::clone(&running.core)),
        Arc::clone(&next.core),
        None,
    );
    assert!(
        cpu0.switch_handoff().is_none(),
        "reselecting current creates no artificial context-switch tail"
    );
    assert!(
        !cpu1.has_remote_work(),
        "migration cannot publish before the outgoing stack is inactive"
    );

    assert!(
        cpu0.has_remote_work(),
        "the affinity request remains owned by the source rq after a concurrent stale pick"
    );
    assert!(
        !cpu1.has_remote_work(),
        "an owner-control publication cannot mutate a remote rq"
    );
    system.drain_owner_work(cpu0.as_mut()).unwrap();
    assert_eq!(
        running.core.sched().lock().placement.requested_migration(),
        Some(CpuId::new(1))
    );
    let decision = system.schedule_at(cpu0.as_mut(), 3).unwrap();
    assert_eq!(decision.next(), idle0.id());
    assert!(
        !cpu1.has_remote_work(),
        "the prepared migration remains pinned until switch tail releases on_cpu"
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    assert!(cpu1.has_remote_work());
}

#[test]
fn rt_deadline_put_prev_keeps_one_runqueue_owner() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let policy =
        SchedulePolicy::deadline(DeadlinePolicy::new(10, 100, 1_000, DeadlineFlags::NONE).unwrap());
    let deadline = system.create_thread(ThreadSpec::new(policy)).unwrap();
    system.make_ready(deadline.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), deadline.id(), 1).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 1).unwrap().next(),
        deadline.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert!(
        cpu.lock_run_queue(crate::RunQueueGuardSource::TestInspection)
            .is_linked_current(deadline.id())
    );
    let queued_before = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .nr_queued();
    let remote = Arc::clone(cpu.remote());
    {
        let mut sched = deadline.core.sched().lock();
        let mut transaction = OwnerRqTxn::begin(&system, &remote);
        system.schedule_out_owner_running_in_rq(
            cpu.as_mut(),
            &mut transaction,
            Arc::clone(&deadline.core),
            &mut sched,
            2,
            EnqueueReason::Preempted,
        );
        transaction.commit();
    }

    let sched = deadline.core.sched().lock();
    assert_eq!(sched.lifecycle.state(), ThreadState::Ready);
    assert_eq!(sched.placement.execution_cpu(), Some(CpuId::new(0)));
    assert_eq!(sched.placement.queued_cpu(), Some(CpuId::new(0)));
    drop(sched);
    assert_eq!(cpu.current(), None);
    let run_queue = cpu.lock_run_queue(crate::RunQueueGuardSource::TestInspection);
    assert!(run_queue.queued_thread(deadline.id()).is_some());
    assert_eq!(run_queue.nr_queued(), queued_before + 1);
}

#[test]
fn deadline_runqueue_ledger_tracks_policy_replacement() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();

    let deadline = system
        .create_thread(ThreadSpec::new(SchedulePolicy::deadline(
            DeadlinePolicy::new(4, 8, 8, DeadlineFlags::NONE).unwrap(),
        )))
        .unwrap();
    system.make_ready(deadline.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), deadline.id(), 0).unwrap();
    let bandwidth = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .deadline_bandwidth();
    assert_eq!(bandwidth.this_bw_scaled(), 500_000_000);
    assert_eq!(bandwidth.running_bw_scaled(), 500_000_000);

    system
        .set_thread_policy(deadline.id(), SchedulePolicy::default())
        .unwrap();
    system.drain_owner_control_at(cpu.as_mut(), 1).unwrap();
    let bandwidth = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .deadline_bandwidth();
    assert_eq!(bandwidth.this_bw_scaled(), 0);
    assert_eq!(bandwidth.running_bw_scaled(), 0);

    system
        .set_thread_policy(
            deadline.id(),
            SchedulePolicy::deadline(DeadlinePolicy::new(2, 10, 20, DeadlineFlags::NONE).unwrap()),
        )
        .unwrap();
    system.drain_owner_control_at(cpu.as_mut(), 2).unwrap();
    let bandwidth = cpu
        .lock_run_queue(crate::RunQueueGuardSource::TestInspection)
        .deadline_bandwidth();
    assert_eq!(bandwidth.this_bw_scaled(), 100_000_000);
    assert_eq!(bandwidth.running_bw_scaled(), 100_000_000);
}

#[test]
fn queued_rt_deadline_reclassification_preserves_pushable_membership() {
    crate::test_runtime::reset_irq_state();
    crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success);
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }
    let rt = SchedulePolicy::fifo(RtPriority::new(80).unwrap());
    let thread = system.create_thread(ThreadSpec::new(rt)).unwrap();
    system.make_ready(thread.id()).unwrap();
    system.enqueue_at(cpu0.as_mut(), thread.id(), 0).unwrap();
    {
        let run_queue = cpu0.lock_run_queue(crate::RunQueueGuardSource::TestInspection);
        assert_eq!(run_queue.nr_running(), 1);
        assert!(run_queue.has_pushable_realtime());
        assert!(!run_queue.has_pushable_deadline());
    }

    let deadline =
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 4, 10, DeadlineFlags::NONE).unwrap());
    system.set_thread_policy(thread.id(), deadline).unwrap();
    {
        let run_queue = cpu0.lock_run_queue(crate::RunQueueGuardSource::TestInspection);
        assert_eq!(run_queue.nr_running(), 1);
        assert!(!run_queue.has_pushable_realtime());
        assert!(run_queue.has_pushable_deadline());
        assert_eq!(
            run_queue.queued_thread(thread.id()).unwrap().policy(),
            deadline
        );
    }

    system.set_thread_policy(thread.id(), rt).unwrap();
    let run_queue = cpu0.lock_run_queue(crate::RunQueueGuardSource::TestInspection);
    assert_eq!(run_queue.nr_running(), 1);
    assert!(run_queue.has_pushable_realtime());
    assert!(!run_queue.has_pushable_deadline());
    assert_eq!(run_queue.queued_thread(thread.id()).unwrap().policy(), rt);
}

#[test]
fn blocked_affinity_update_completes_after_switch_tail() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    let blocked = system
        .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system
        .register_idle_thread(
            cpu0.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system
        .register_idle_thread(
            cpu1.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();

    let decision = system.block_current_at(cpu0.as_mut(), 1).unwrap();
    assert_eq!(decision.switch_reason(), SwitchReason::Blocked);
    assert_eq!(system.thread_state(blocked.id()), Ok(ThreadState::Blocked));

    // Model a remote setter that observes the blocked task after selection
    // committed, but before the incoming context consumes switch tail. The
    // old CPU is only a transient `on_cpu` lifetime owner now; it must not
    // become a permanent affinity-completion owner.
    let mut cpu1_only = CpuSet::empty(2);
    assert!(cpu1_only.insert(CpuId::new(1)));
    let change = system.request_affinity(blocked.id(), cpu1_only).unwrap();
    assert_eq!(change.try_result(), None);

    let deferred = system.drain_owner_control_at(cpu0.as_mut(), 2).unwrap();
    assert_eq!(deferred.drained(), 0);
    assert!(deferred.pending());

    system.complete_context_switch(cpu0.as_mut()).unwrap();
    system.drain_owner_control_at(cpu0.as_mut(), 2).unwrap();

    assert_eq!(change.try_result(), Some(Ok(())));
    assert_eq!(blocked.core.wake_cpu_hint(), Some(CpuId::new(1)));
}

#[test]
fn initial_placement_hands_affinity_pinned_thread_to_its_owner_cpu() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();

    let mut cpu1_only = CpuSet::empty(2);
    cpu1_only.insert(CpuId::new(1));
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()).with_affinity(cpu1_only))
        .unwrap();
    system.make_ready(thread.id()).unwrap();

    system
        .place_ready_at(cpu0.as_mut(), thread.id(), 0)
        .unwrap();
    assert!(cpu1.has_remote_work());
    system.drain_owner_control_at(cpu1.as_mut(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 0).unwrap().next(),
        thread.id()
    );
}

#[test]
fn class_order_is_deadline_then_rt_then_fair() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let policies = [
        SchedulePolicy::fair(Nice::ZERO, FairMode::Normal),
        SchedulePolicy::fifo(RtPriority::new(1).unwrap()),
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap()),
    ];
    let mut ids = Vec::new();
    for policy in policies {
        let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
        system.make_ready(thread.id()).unwrap();
        system.enqueue_at(cpu.as_mut(), thread.id(), 0).unwrap();
        ids.push(thread.id());
    }
    assert_eq!(system.schedule_at(cpu.as_mut(), 0).unwrap().next(), ids[2]);
}

#[test]
fn load_summary_publication_does_not_scan_runnable_threads() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    let threads = (0..64)
        .map(|_| {
            let thread = system
                .create_thread(ThreadSpec::new(SchedulePolicy::default()))
                .unwrap();
            system.make_ready(thread.id()).unwrap();
            system.enqueue_at(cpu0.as_mut(), thread.id(), 0).unwrap();
            thread
        })
        .collect::<Vec<_>>();

    balance::reset_balance_candidate_visits();
    OwnerRqTxn::begin(&system, cpu0.remote()).commit();

    assert_eq!(
        balance::balance_candidate_visits(),
        0,
        "rq summary publication must use incrementally maintained owner state"
    );
    assert_eq!(threads.len(), 64);
}

#[test]
fn clock_only_owner_transaction_does_not_republish_unchanged_load() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online_at(cpu.as_mut(), 0).unwrap();

    balance::reset_load_summary_publications();
    crate::system::cpu::reset_rt_bandwidth_lock_acquisitions();
    let _clock = system.sample_owner_rq_clock(cpu.as_ref().get_ref());

    assert_eq!(
        balance::load_summary_publications(),
        0,
        "updating rq->clock without changing runnable state must not rewrite the remote load \
         seqlock"
    );
    assert_eq!(
        crate::system::cpu::rt_bandwidth_lock_acquisitions(),
        0,
        "a clock-only rq transaction must not acquire a detached RT runtime ledger lock"
    );
}

#[test]
fn one_owner_selection_publishes_one_load_summary() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online_at(cpu.as_mut(), 0).unwrap();
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(thread.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), thread.id(), 0).unwrap();

    balance::reset_load_summary_publications();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        thread.id()
    );

    assert_eq!(
        balance::load_summary_publications(),
        1,
        "the already-published owner snapshot must serve the post-selection balance tail"
    );
}

#[test]
fn ordinary_owner_selection_does_not_enter_balance_without_pending_work() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online_at(cpu.as_mut(), 0).unwrap();
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(thread.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), thread.id(), 0).unwrap();

    balance::reset_owner_balance_passes();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        thread.id()
    );

    assert_eq!(
        balance::owner_balance_passes(),
        0,
        "a normal owner selection must not enter SMP balancing without idle-pull, RT/DL overload, \
         or periodic Fair work"
    );
}

#[test]
fn deadline_affinity_must_cover_online_root_domain() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    let mut affinity = CpuSet::empty(2);
    affinity.insert(CpuId::new(0));
    let policy =
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap());
    assert!(matches!(
        system.create_thread(ThreadSpec::new(policy).with_affinity(affinity)),
        Err(TaskError::DeadlineAffinity)
    ));
}

#[test]
fn active_sleep_timer_pins_affinity_placement_to_its_owner_cpu() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    system.bring_cpu_online(cpu0.as_mut()).unwrap();
    system.bring_cpu_online(cpu1.as_mut()).unwrap();
    let thread = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    thread.core.register_sleep_timer(CpuId::new(1), 7);

    let mut excludes_owner = CpuSet::empty(2);
    excludes_owner.insert(CpuId::new(0));
    assert_eq!(
        system.set_affinity(thread.id(), excludes_owner),
        Err(TaskError::ActiveTimerAffinity)
    );

    let mut includes_owner = CpuSet::empty(2);
    includes_owner.insert(CpuId::new(1));
    system.set_affinity(thread.id(), includes_owner).unwrap();
    assert_eq!(thread.assigned_cpu(), Some(CpuId::new(0)));
    assert_eq!(thread.core.wake_cpu_hint(), Some(CpuId::new(1)));
    assert!(thread.core.complete_sleep_timer(7));
}

#[test]
fn queued_pi_owner_is_reclassified_inside_one_owner_rq_transaction() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fair(
            Nice::new(19).unwrap(),
            FairMode::Normal,
        )))
        .unwrap();
    let competitor = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let waiter = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(99).unwrap(),
        )))
        .unwrap();
    for thread in [&owner, &competitor] {
        system.make_ready(thread.id()).unwrap();
        system.enqueue_at(cpu.as_mut(), thread.id(), 0).unwrap();
    }
    let lock = PiMutexCore::new();

    let wait = commit_pi_wait(&system, &lock, waiter.id(), owner.id()).unwrap();

    assert!(matches!(
        owner.effective_policy(),
        SchedulePolicy::Fifo { priority } if priority.get() == 99
    ));
    assert_eq!(system.snapshot(cpu.as_ref()).unwrap().runnable(), 2);
    let drain = system.drain_owner_control_at(cpu.as_mut(), 1).unwrap();
    assert_eq!(drain.drained(), 0);
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 1).unwrap().next(),
        owner.id()
    );
    system.pi_wait_cancel(wait).unwrap();
}

#[test]
fn interruptible_pi_cancel_defers_to_an_ownerless_handoff() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = create_online_pi_cpu(&system);
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let waiter = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    place_pi_owner(&system, cpu.as_mut(), &owner);
    let lock = PiMutexCore::new();
    let token = commit_pi_wait(&system, &lock, waiter.id(), owner.id()).unwrap();

    system
        .pi_mutex_release(lock.mutex_ref().unwrap(), owner.id())
        .unwrap();
    assert!(token.can_claim());
    assert_eq!(
        system.pi_wait_try_cancel(&token).unwrap(),
        crate::PiWaitCancelOutcome::HandoffPending,
        "Linux rtmutex tries to take an ownerless handoff before honoring interruption"
    );

    system.pi_mutex_claim(&token).unwrap();
    assert!(token.is_granted());
}

#[test]
fn deadline_pi_boost_overrides_constrained_wake_throttling() {
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let idle = system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.make_ready(owner.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), owner.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        owner.id()
    );
    assert_eq!(
        system.block_current_at(cpu.as_mut(), 0).unwrap().next(),
        idle.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();

    let donor_policy =
        SchedulePolicy::deadline(DeadlinePolicy::new(4, 8, 10, DeadlineFlags::NONE).unwrap());
    let donor = system.create_thread(ThreadSpec::new(donor_policy)).unwrap();
    system.make_ready(donor.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), donor.id(), 0).unwrap();
    system.dequeue(cpu.as_mut(), donor.id()).unwrap();
    let lock = PiMutexCore::new();
    let wait = commit_pi_wait(&system, &lock, donor.id(), owner.id()).unwrap();

    assert_eq!(owner.wake_handle().wake(), crate::WakeResult::Notified);

    assert_eq!(
        system.thread_state(owner.id()).unwrap(),
        ThreadState::Ready,
        "Linux Deadline PI boost must override constrained wake throttling"
    );
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 9).unwrap().next(),
        owner.id()
    );
    system.pi_wait_cancel(wait).unwrap();
}

#[test]
fn effective_rt_entity_never_replaces_the_base_rr_accounting() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let base = SchedulePolicy::round_robin_with_quantum(RtPriority::new(20).unwrap(), 10).unwrap();
    let owner = system.create_thread(ThreadSpec::new(base)).unwrap();
    let donor = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(80).unwrap(),
        )))
        .unwrap();
    system.make_ready(owner.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), owner.id(), 0).unwrap();
    let lock = PiMutexCore::new();
    let wait = commit_pi_wait(&system, &lock, donor.id(), owner.id()).unwrap();
    system.drain_owner_control_at(cpu.as_mut(), 0).unwrap();

    let rq = cpu.lock_run_queue(crate::RunQueueGuardSource::TestInspection);
    let (effective_policy, effective_entity) = rq.scheduling_state(owner.id()).unwrap();
    assert_eq!(
        effective_policy,
        SchedulePolicy::round_robin_with_quantum(RtPriority::new(80).unwrap(), 10).unwrap(),
        "Linux RT PI changes effective priority without changing RR policy"
    );
    assert!(effective_entity.matches_policy(base));
    assert!(
        rq.base_scheduling_entity(owner.id())
            .unwrap()
            .matches_policy(base)
    );
    drop(rq);
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        owner.id()
    );
    assert!(
        !system
            .charge_current_at(cpu.as_mut(), 4, 4, 0)
            .unwrap()
            .slice_expired()
    );
    system.pi_wait_cancel(wait).unwrap();
    system.drain_owner_control_at(cpu.as_mut(), 4).unwrap();
    system.schedule_at(cpu.as_mut(), 4).unwrap();
    assert!(
        system
            .charge_current_at(cpu.as_mut(), 10, 6, 0)
            .unwrap()
            .slice_expired(),
        "RR quantum must accumulate across PI boost and deboost"
    );
}

#[test]
fn chained_and_multi_lock_donations_are_withdrawn_independently() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = create_online_pi_cpu(&system);
    let first_owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fair(
            Nice::new(19).unwrap(),
            FairMode::Normal,
        )))
        .unwrap();
    let second_owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fair(
            Nice::new(10).unwrap(),
            FairMode::Normal,
        )))
        .unwrap();
    let urgent = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(99).unwrap(),
        )))
        .unwrap();
    place_pi_owner(&system, cpu.as_mut(), &first_owner);
    place_pi_owner(&system, cpu.as_mut(), &second_owner);
    let first_lock = PiMutexCore::new();
    let second_lock = PiMutexCore::new();
    let chained =
        commit_pi_wait(&system, &first_lock, second_owner.id(), first_owner.id()).unwrap();
    let urgent_wait =
        commit_pi_wait(&system, &second_lock, urgent.id(), second_owner.id()).unwrap();
    assert!(matches!(
        first_owner.effective_policy(),
        SchedulePolicy::Fifo { priority } if priority.get() == 99
    ));

    system.pi_wait_cancel(urgent_wait).unwrap();
    assert_eq!(second_owner.effective_policy(), second_owner.policy());
    assert_eq!(first_owner.effective_policy(), second_owner.policy());

    system.pi_wait_cancel(chained).unwrap();
    assert_eq!(first_owner.effective_policy(), first_owner.policy());
}

#[test]
fn pi_registration_does_not_scan_unrelated_registry_slots() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = create_online_pi_cpu(&system);
    let unrelated = (0..128)
        .map(|_| {
            system
                .create_thread(ThreadSpec::new(SchedulePolicy::default()))
                .unwrap()
        })
        .collect::<Vec<_>>();
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let waiter = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(90).unwrap(),
        )))
        .unwrap();
    place_pi_owner(&system, cpu.as_mut(), &owner);
    registry::reset_pi_donor_record_visits();

    let lock = PiMutexCore::new();
    let token = commit_pi_wait(&system, &lock, waiter.id(), owner.id()).unwrap();

    assert_eq!(
        registry::pi_donor_record_visits(),
        0,
        "per-lock cached top publication must not scan registry records"
    );
    drop(unrelated);
    system.pi_wait_cancel(token).unwrap();
}

#[test]
fn equal_urgency_pi_registration_does_not_rescan_owner_donors() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = create_online_pi_cpu(&system);
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let first = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let second = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    place_pi_owner(&system, cpu.as_mut(), &owner);
    let lock = PiMutexCore::new();
    let first_wait = commit_pi_wait(&system, &lock, first.id(), owner.id()).unwrap();
    registry::reset_pi_donor_record_visits();

    let second_wait = commit_pi_wait(&system, &lock, second.id(), owner.id()).unwrap();

    assert_eq!(
        registry::pi_donor_record_visits(),
        0,
        "an equal-urgency waiter cannot change the owner or its upstream donation chain"
    );
    assert_eq!(owner.effective_policy(), owner.policy());
    system.pi_wait_cancel(second_wait).unwrap();
    system.pi_wait_cancel(first_wait).unwrap();
}

#[test]
fn blocked_waiter_policy_change_updates_the_cached_lock_top_without_owner_scan() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fair(
            Nice::new(19).unwrap(),
            FairMode::Normal,
        )))
        .unwrap();
    let first = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(80).unwrap(),
        )))
        .unwrap();
    let second = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(90).unwrap(),
        )))
        .unwrap();
    system.make_ready(owner.id()).unwrap();
    system.enqueue(cpu.as_mut(), owner.id()).unwrap();
    let lock = PiMutexCore::new();
    let first_wait = commit_pi_wait(&system, &lock, first.id(), owner.id()).unwrap();
    let second_wait = commit_pi_wait(&system, &lock, second.id(), owner.id()).unwrap();
    assert_eq!(
        system
            .state
            .lock()
            .thread_record(owner.id())
            .unwrap()
            .sched
            .lock()
            .pi
            .donor,
        Some(second.id())
    );

    registry::reset_pi_donor_record_visits();
    system
        .set_thread_policy(
            second.id(),
            SchedulePolicy::fifo(RtPriority::new(70).unwrap()),
        )
        .unwrap();
    let state = system.state.lock();
    let owner_sched = state.thread_record(owner.id()).unwrap().sched.lock();
    assert_eq!(owner_sched.pi.donor, Some(first.id()));
    assert_eq!(
        owner.effective_policy(),
        SchedulePolicy::fifo(RtPriority::new(80).unwrap())
    );
    drop(owner_sched);
    drop(state);
    assert_eq!(
        registry::pi_donor_record_visits(),
        0,
        "a blocked waiter key update must not scan every waiter twice"
    );

    system.pi_wait_cancel(second_wait).unwrap();
    system.pi_wait_cancel(first_wait).unwrap();
}

#[test]
fn failed_pi_registration_does_not_publish_a_partial_edge() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = create_online_pi_cpu(&system);
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let waiter = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(90).unwrap(),
        )))
        .unwrap();
    place_pi_owner(&system, cpu.as_mut(), &owner);
    {
        let state = system.state.lock();
        state
            .thread_record(owner.id())
            .unwrap()
            .sched
            .lock()
            .policy
            .dispatch_generation = u64::MAX;
    }

    let lock = PiMutexCore::new();
    let result = commit_pi_wait(&system, &lock, waiter.id(), owner.id());

    assert_eq!(result.unwrap_err(), TaskError::InvalidConfiguration);
    assert!(release_pi_for_thread(&lock, owner.id()).unwrap());
    let state = system.state.lock();
    assert_eq!(
        state
            .thread_record(waiter.id())
            .unwrap()
            .sched
            .lock()
            .pi
            .blocked_on,
        None
    );
    assert!(
        state
            .thread_record(owner.id())
            .unwrap()
            .sched
            .lock()
            .pi
            .donors
            .is_empty()
    );
}

#[test]
fn failed_pi_release_preserves_the_unselected_wait_transaction() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = create_online_pi_cpu(&system);
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let waiter = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(90).unwrap(),
        )))
        .unwrap();
    place_pi_owner(&system, cpu.as_mut(), &owner);
    let lock = PiMutexCore::new();
    let token = commit_pi_wait(&system, &lock, waiter.id(), owner.id()).unwrap();
    {
        let state = system.state.lock();
        state
            .thread_record(owner.id())
            .unwrap()
            .sched
            .lock()
            .policy
            .dispatch_generation = u64::MAX;
    }

    assert_eq!(
        system
            .pi_mutex_release(lock.mutex_ref().unwrap(), owner.id())
            .unwrap_err(),
        TaskError::InvalidConfiguration
    );
    assert!(!token.is_granted());
    let state = system.state.lock();
    assert_eq!(
        state
            .thread_record(waiter.id())
            .unwrap()
            .sched
            .lock()
            .pi
            .blocked_on,
        Some(PiWaitRegistration {
            lock: lock.mutex_ref().unwrap().raw(),
            key: PiWaitKey::new(
                waiter.core.effective_pi_wait_urgency(),
                waiter.id().as_u64(),
                waiter.id()
            ),
            generation: token.generation(),
        })
    );
    assert!(
        !state
            .thread_record(owner.id())
            .unwrap()
            .sched
            .lock()
            .pi
            .donors
            .is_empty()
    );
    drop(state);
    {
        let state = system.state.lock();
        state
            .thread_record(owner.id())
            .unwrap()
            .sched
            .lock()
            .policy
            .dispatch_generation = 0;
    }
    system.pi_wait_cancel(token).unwrap();
}

#[test]
fn pi_release_atomically_selects_and_preserves_the_wait_transaction() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let waiter = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(90).unwrap(),
        )))
        .unwrap();
    place_pi_owner(&system, cpu.as_mut(), &owner);
    system.make_ready(waiter.id()).unwrap();
    let lock = PiMutexCore::new();
    let token = commit_pi_wait(&system, &lock, waiter.id(), owner.id()).unwrap();

    system
        .pi_mutex_release(lock.mutex_ref().unwrap(), owner.id())
        .unwrap();

    assert!(!token.is_granted());
    assert!(token.can_claim());
    let state = system.state.lock();
    assert_eq!(
        state
            .thread_record(waiter.id())
            .unwrap()
            .sched
            .lock()
            .pi
            .blocked_on,
        Some(PiWaitRegistration {
            lock: lock.mutex_ref().unwrap().raw(),
            key: PiWaitKey::new(
                waiter.core.effective_pi_wait_urgency(),
                waiter.id().as_u64(),
                waiter.id()
            ),
            generation: token.generation(),
        })
    );
    assert!(
        state
            .thread_record(owner.id())
            .unwrap()
            .sched
            .lock()
            .pi
            .donors
            .is_empty()
    );
    drop(state);
    system.pi_mutex_claim(&token).unwrap();
    assert!(token.is_granted());
    assert!(release_pi_for_thread(&lock, waiter.id()).unwrap());
}

#[test]
fn pi_claim_retries_when_a_more_urgent_waiter_wins_the_ownerless_window() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = create_online_pi_cpu(&system);
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let first = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(80).unwrap(),
        )))
        .unwrap();
    let later = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(99).unwrap(),
        )))
        .unwrap();
    place_pi_owner(&system, cpu.as_mut(), &owner);
    let lock = PiMutexCore::new();
    let first_token = commit_pi_wait(&system, &lock, first.id(), owner.id()).unwrap();

    system
        .pi_mutex_release(lock.mutex_ref().unwrap(), owner.id())
        .unwrap();
    assert!(first_token.can_claim());
    let later_token = match system
        .pi_mutex_lock_slow(lock.mutex_ref().unwrap(), later.id(), later.id().as_u64())
        .unwrap()
    {
        PiMutexLockResult::Waiting(token) => token,
        PiMutexLockResult::Acquired => panic!("ownerless PI handoff must retain its waiter tree"),
    };
    assert!(later_token.can_claim());
    assert!(!first_token.can_claim());

    assert_eq!(
        system.pi_mutex_claim(&first_token).unwrap(),
        PiMutexClaimOutcome::Retry,
        "losing a Linux rtmutex-style ownerless claim race must request a retry"
    );
    assert_eq!(
        system.pi_mutex_claim(&later_token).unwrap(),
        PiMutexClaimOutcome::Claimed
    );
    system.pi_wait_cancel(first_token).unwrap();
    assert!(release_pi_for_thread(&lock, later.id()).unwrap());
}

#[test]
fn pi_chain_walk_ignores_a_previous_owner_that_requeues_on_the_origin_lock() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = create_online_pi_cpu(&system);
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let waiter = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    place_pi_owner(&system, cpu.as_mut(), &owner);
    let lock = PiMutexCore::new();
    let waiter_token = commit_pi_wait(&system, &lock, waiter.id(), owner.id()).unwrap();

    system
        .pi_mutex_release(lock.mutex_ref().unwrap(), owner.id())
        .unwrap();
    let owner_token = match system
        .pi_mutex_lock_slow(lock.mutex_ref().unwrap(), owner.id(), u64::MAX)
        .unwrap()
    {
        PiMutexLockResult::Waiting(token) => token,
        PiMutexLockResult::Acquired => panic!("ownerless handoff must retain its waiter tree"),
    };

    assert_eq!(
        system.recompute_pi_chain(
            owner.id(),
            Some(lock.mutex_ref().unwrap().raw()),
            waiter.id(),
        ),
        Ok(()),
        "a released owner may requeue before the original waiter's chain walk resumes"
    );

    if waiter_token.can_claim() {
        system.pi_mutex_claim(&waiter_token).unwrap();
        system.pi_wait_cancel(owner_token).unwrap();
        assert!(release_pi_for_thread(&lock, waiter.id()).unwrap());
    } else {
        assert!(owner_token.can_claim());
        system.pi_mutex_claim(&owner_token).unwrap();
        system.pi_wait_cancel(waiter_token).unwrap();
        assert!(release_pi_for_thread(&lock, owner.id()).unwrap());
    }
}

#[test]
fn pi_release_wakes_the_selected_waiter_before_returning() {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let waiter = system
        .create_thread(ThreadSpec::new(SchedulePolicy::fifo(
            RtPriority::new(90).unwrap(),
        )))
        .unwrap();
    place_pi_owner(&system, cpu.as_mut(), &owner);
    system.make_ready(waiter.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), waiter.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        waiter.id()
    );
    system.complete_context_switch(cpu.as_mut()).unwrap();
    system.block_current_at(cpu.as_mut(), 1).unwrap();
    system.complete_context_switch(cpu.as_mut()).unwrap();
    assert_eq!(waiter.state(), ThreadState::Blocked);

    let lock = PiMutexCore::new();
    let token = commit_pi_wait(&system, &lock, waiter.id(), owner.id()).unwrap();
    system
        .pi_mutex_release(lock.mutex_ref().unwrap(), owner.id())
        .unwrap();

    let state_after_release = waiter.state();
    system.pi_mutex_claim(&token).unwrap();
    assert!(release_pi_for_thread(&lock, waiter.id()).unwrap());
    assert_eq!(
        state_after_release,
        ThreadState::Ready,
        "Linux rtmutex unlock owns wake_q completion; callers cannot be required to publish a \
         second wake after metadata handoff"
    );
}

#[test]
fn deadline_pi_charges_the_owner_local_cbs_without_debiting_the_donor() {
    DEADLINE_OVERRUN_CALLBACKS.store(0, Ordering::Relaxed);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let deadline = SchedulePolicy::deadline(
        DeadlinePolicy::new(10, 20, 100, DeadlineFlags::DL_OVERRUN).unwrap(),
    );
    let extension = unsafe { ThreadExtension::new(0, &DEADLINE_TEST_EXTENSION_OPS) };
    let donor = system
        .create_thread(ThreadSpec::new(deadline).with_extension(extension))
        .unwrap();
    let lock = PiMutexCore::new();
    for thread in [&owner, &donor] {
        system.make_ready(thread.id()).unwrap();
        system.enqueue_at(cpu.as_mut(), thread.id(), 0).unwrap();
    }
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        donor.id()
    );
    let wait = commit_pi_wait(&system, &lock, donor.id(), owner.id()).unwrap();
    system.drain_owner_control_at(cpu.as_mut(), 0).unwrap();
    assert_eq!(
        system.block_current_at(cpu.as_mut(), 0).unwrap().next(),
        owner.id()
    );

    let charged = system.charge_current_at(cpu.as_mut(), 10, 10, 0).unwrap();
    assert!(!charged.slice_expired());
    assert!(
        !charged.deadline_overrun(),
        "Linux reports DL overrun from the boosted owner's local flags, not donor flags"
    );
    assert_eq!(DEADLINE_OVERRUN_CALLBACKS.load(Ordering::Relaxed), 0);
    system.schedule_at(cpu.as_mut(), 10).unwrap();

    let donor_runtime = system.deadline_runtime(donor.id()).unwrap();
    assert_eq!(donor_runtime.remaining_runtime_ns(), 10);
    assert_eq!(donor_runtime.overruns(), 0);
    let owner_runtime = system.deadline_runtime(owner.id()).unwrap();
    assert_eq!(owner_runtime.donor(), Some(donor.id()));
    assert!(owner_runtime.pi_boosted());
    assert_eq!(owner_runtime.remaining_runtime_ns(), 10);
    assert_eq!(owner_runtime.overruns(), 1);
    assert_eq!(system.dispatch_deadline_overruns(1), Ok(0));
    assert_eq!(DEADLINE_OVERRUN_CALLBACKS.load(Ordering::Relaxed), 0);
    system.pi_wait_cancel(wait).unwrap();
}

#[test]
fn remote_pi_owner_charges_its_local_cbs_with_donor_parameters() {
    let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
    let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
    let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
    for cpu in [&mut cpu0, &mut cpu1] {
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
    }
    let donor_policy =
        SchedulePolicy::deadline(DeadlinePolicy::new(10, 20, 100, DeadlineFlags::RECLAIM).unwrap());
    let donor = system.create_thread(ThreadSpec::new(donor_policy)).unwrap();
    let owner = system
        .create_thread(ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    for thread in [&donor, &owner] {
        system.make_ready(thread.id()).unwrap();
    }
    system.enqueue_at(cpu0.as_mut(), donor.id(), 0).unwrap();
    system.enqueue_at(cpu1.as_mut(), owner.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu0.as_mut(), 0).unwrap().next(),
        donor.id()
    );
    assert_eq!(
        system.schedule_at(cpu1.as_mut(), 0).unwrap().next(),
        owner.id()
    );

    let lock = PiMutexCore::new();
    let wait = commit_pi_wait(&system, &lock, donor.id(), owner.id()).unwrap();
    assert_ne!(
        system.block_current_at(cpu0.as_mut(), 0).unwrap().next(),
        donor.id()
    );
    system.complete_context_switch(cpu0.as_mut()).unwrap();
    system.drain_owner_control_at(cpu1.as_mut(), 0).unwrap();

    let owner_runtime = system.deadline_runtime(owner.id()).unwrap();
    assert_eq!(owner_runtime.donor(), Some(donor.id()));
    assert!(owner_runtime.pi_boosted());

    system.charge_current_at(cpu1.as_mut(), 5, 5, 0).unwrap();
    {
        let mut transaction = OwnerRqTxn::begin(&system, cpu1.remote());
        system.commit_owner_current_dispatch_in_rq(&mut transaction);
        transaction.commit();
    }
    assert_eq!(
        system
            .deadline_runtime(donor.id())
            .unwrap()
            .remaining_runtime_ns(),
        10
    );
    assert_eq!(
        system
            .deadline_runtime(owner.id())
            .unwrap()
            .remaining_runtime_ns(),
        5
    );
    system.pi_wait_cancel(wait).unwrap();
}

#[test]
fn coalesced_deadline_refresh_recomputes_the_owner_cbs_state() {
    crate::test_runtime::set_monotonic_ns(0);
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let policy =
        SchedulePolicy::deadline(DeadlinePolicy::new(10, 20, 100, DeadlineFlags::NONE).unwrap());
    let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
    system.make_ready(thread.id()).unwrap();
    system.enqueue_at(cpu.as_mut(), thread.id(), 0).unwrap();
    assert_eq!(
        system.schedule_at(cpu.as_mut(), 0).unwrap().next(),
        thread.id()
    );
    assert!(
        system
            .charge_current_at(cpu.as_mut(), 10, 10, 0)
            .unwrap()
            .slice_expired()
    );
    system.schedule_at(cpu.as_mut(), 10).unwrap();
    system.complete_context_switch(cpu.as_mut()).unwrap();

    let core = {
        let state = system.state.lock();
        Arc::clone(&state.thread_record(thread.id()).unwrap().core)
    };
    assert_eq!(
        system
            .deadline_runtime(thread.id())
            .unwrap()
            .remaining_runtime_ns(),
        0
    );
    assert!(core.sched().lock().deadline.cbs_timer.is_some());
    {
        let mut sched = core.sched().lock();
        TaskSystem::cancel_owner_deadline_timers_locked(&core, &mut sched, cpu.remote());
    }
    assert!(core.sched().lock().deadline.cbs_timer.is_none());
    let first = cpu.remote().begin_owner_delivery().unwrap();
    system.publish_owner_deadline_refresh_reserved(&core, CpuId::new(0), first);
    let second = cpu.remote().begin_owner_delivery().unwrap();
    system.publish_owner_deadline_refresh_reserved(&core, CpuId::new(0), second);
    assert_eq!(
        core.scheduler_inbox_delivery_count(),
        1,
        "the dedicated intrusive node must coalesce the newer refresh"
    );

    system.drain_owner_control_at(cpu.as_mut(), 10).unwrap();

    assert!(
        core.sched().lock().deadline.cbs_timer.is_some(),
        "the coalesced message must recompute CBS state from the owner rq"
    );
    assert_eq!(core.scheduler_inbox_delivery_count(), 0);
}

#[test]
fn wake_before_park_is_consumed_without_blocking() {
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

    assert_eq!(running.wake_handle().wake(), crate::WakeResult::Notified);

    assert_eq!(
        system.prepare_park(cpu.as_mut()).unwrap(),
        ParkPrepare::Notified
    );
    assert_eq!(
        system.thread_state(running.id()).unwrap(),
        ThreadState::Running
    );
}

#[test]
fn wake_during_parking_cancels_schedule_out() {
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    let ParkPrepare::Prepared(mut ticket) = system.prepare_park(cpu.as_mut()).unwrap() else {
        panic!("fresh park must publish PARKING");
    };

    assert_eq!(running.wake_handle().wake(), crate::WakeResult::Notified);

    assert!(matches!(
        system
            .commit_park_at_for_test(cpu.as_mut(), &mut ticket, 0)
            .unwrap(),
        ParkCommit::Notified
    ));
    assert_eq!(
        system.thread_state(running.id()).unwrap(),
        ThreadState::Running
    );
    assert!(matches!(
        system.commit_park_at_for_test(cpu.as_mut(), &mut ticket, 0),
        Err(TaskError::StaleThreadId)
    ));
    assert_eq!(
        system.thread_state(running.id()).unwrap(),
        ThreadState::Running,
        "a resolved park ticket must not start another block transition"
    );
}

#[test]
fn wake_that_observes_running_before_parking_rechecks_under_the_thread_lock() {
    let system = Arc::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

    dispatch::arm_wake_before_thread_lock_race(system.as_ref().get_ref(), running.id());
    let wake_system = Pin::clone(&system);
    let wake_core = Arc::clone(&running.core);
    let waker = std::thread::spawn(move || {
        wake_system
            .as_ref()
            .get_ref()
            .wake_thread_direct(wake_core, Some(CpuId::new(0)))
    });
    while !dispatch::wake_before_thread_lock_race_entered() {
        core::hint::spin_loop();
    }

    let ParkPrepare::Prepared(mut ticket) = system.prepare_park(cpu.as_mut()).unwrap() else {
        panic!("fresh park must publish PARKING");
    };
    dispatch::complete_wake_before_thread_lock_race();

    assert_eq!(waker.join().unwrap(), crate::WakeResult::Notified);
    assert!(matches!(
        system
            .commit_park_at_for_test(cpu.as_mut(), &mut ticket, 0)
            .unwrap(),
        ParkCommit::Notified
    ));
    assert_eq!(
        system.thread_state(running.id()).unwrap(),
        ThreadState::Running
    );
}

#[test]
fn wake_between_park_check_and_block_transition_cancels_schedule_out() {
    let system = Arc::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let _idle = system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    let ParkPrepare::Prepared(mut ticket) = system.prepare_park(cpu.as_mut()).unwrap() else {
        panic!("fresh park must publish PARKING");
    };

    park_exit::arm_park_commit_wake_race(system.as_ref().get_ref(), running.id());
    let wake_system = Pin::clone(&system);
    let wake_core = Arc::clone(&running.core);
    let waker = std::thread::spawn(move || {
        while !park_exit::park_commit_wake_race_entered() {
            core::hint::spin_loop();
        }
        let result = wake_system
            .as_ref()
            .get_ref()
            .wake_thread_direct(wake_core, Some(CpuId::new(0)));
        park_exit::complete_park_commit_wake_race();
        result
    });

    let commit = system
        .commit_park_at_for_test(cpu.as_mut(), &mut ticket, 0)
        .unwrap();
    assert_eq!(waker.join().unwrap(), crate::WakeResult::Notified);
    assert!(
        matches!(commit, ParkCommit::Notified),
        "a wake serialized before the final block transition must win"
    );
    assert_eq!(
        system.thread_state(running.id()).unwrap(),
        ThreadState::Running
    );
}

#[test]
fn wake_after_final_park_check_serializes_with_blocked_publication() {
    use std::{sync::mpsc, time::Duration};

    let system = Arc::pin(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    let running = system
        .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
        .unwrap();
    let _idle = system
        .register_idle_thread(
            cpu.as_mut(),
            ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        )
        .unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    let ParkPrepare::Prepared(mut ticket) = system.prepare_park(cpu.as_mut()).unwrap() else {
        panic!("fresh park must publish PARKING");
    };
    drop(runtime_handles);

    park_exit::arm_park_after_final_wake_check(system.as_ref().get_ref(), running.id());
    let commit_system = Pin::clone(&system);
    let commit = std::thread::spawn(move || {
        let mut cpu = cpu;
        let _runtime_handles = InstalledTaskHandles::new(commit_system.as_ref(), cpu.as_mut());
        let result = commit_system
            .commit_park_at_for_test(cpu.as_mut(), &mut ticket, 0)
            .unwrap();
        (cpu, result)
    });
    while !park_exit::park_after_final_wake_check_entered() {
        core::hint::spin_loop();
    }

    dispatch::arm_wake_during_final_park_publication(system.as_ref().get_ref(), running.id());
    let wake_system = Pin::clone(&system);
    let wake_core = Arc::clone(&running.core);
    let (wake_result_tx, wake_result_rx) = mpsc::sync_channel(1);
    let waker = std::thread::spawn(move || {
        let result = wake_system
            .as_ref()
            .get_ref()
            .wake_thread_direct(wake_core, Some(CpuId::new(0)));
        wake_result_tx.send(result).unwrap();
    });
    while !dispatch::wake_during_final_park_publication_entered() {
        core::hint::spin_loop();
    }
    dispatch::complete_wake_during_final_park_publication();

    let wake_escaped_locked_park = wake_result_rx.recv_timeout(Duration::from_millis(250)).ok();
    park_exit::complete_park_after_final_wake_check();
    let (mut cpu, commit_result) = commit.join().unwrap();
    while running.state() != ThreadState::Waking {
        core::hint::spin_loop();
    }
    assert!(
        matches!(wake_result_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the waker must retain enqueue ownership while the old stack is on_cpu"
    );
    let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
    system.complete_context_switch(cpu.as_mut()).unwrap();
    let wake_result = wake_escaped_locked_park.unwrap_or_else(|| wake_result_rx.recv().unwrap());
    waker.join().unwrap();

    assert!(
        wake_escaped_locked_park.is_none(),
        "a wake after the final park check must wait for the task lock instead of publishing a \
         lockless bit that Blocked can overtake"
    );
    assert!(matches!(commit_result, ParkCommit::Blocked(_)));
    assert_eq!(wake_result, crate::WakeResult::Notified);
    assert_eq!(
        system.thread_state(running.id()).unwrap(),
        ThreadState::Ready
    );
}

static DEADLINE_TEST_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: no_extension_switch_in,
    on_switch_out: no_extension_switch_out,
    on_exit: no_extension_hook,
    on_deadline_overrun: count_deadline_overrun,
    drop: no_extension_drop,
};

static CANDIDATE_DEADLINE_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

static CANDIDATE_DEADLINE_TEST_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: no_extension_switch_in,
    on_switch_out: no_extension_switch_out,
    on_exit: no_extension_hook,
    on_deadline_overrun: count_candidate_deadline_overrun,
    drop: no_extension_drop,
};

struct DeadlineCallbackRace {
    entered: std::sync::Barrier,
    release: std::sync::Barrier,
    invocations: AtomicUsize,
}

static RACING_DEADLINE_TEST_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: no_extension_switch_in,
    on_switch_out: no_extension_switch_out,
    on_exit: no_extension_hook,
    on_deadline_overrun: race_deadline_overrun,
    drop: no_extension_drop,
};

static POLICY_APPLIED_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static POLICY_APPLIED_AT_NS: AtomicU64 = AtomicU64::new(0);

static ROTATING_DEADLINE_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

static ROTATING_DEADLINE_TEST_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: no_extension_switch_in,
    on_switch_out: no_extension_switch_out,
    on_exit: no_extension_hook,
    on_deadline_overrun: count_rotating_deadline_overrun,
    drop: no_extension_drop,
};

static EXIT_CALLBACK_INVOCATIONS: AtomicUsize = AtomicUsize::new(0);

static EXIT_CALLBACK_TEST_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: no_extension_switch_in,
    on_switch_out: no_extension_switch_out,
    on_exit: count_exit_callback,
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
    _reason: SwitchReason,
) {
}

unsafe extern "Rust" fn count_deadline_overrun(_data: usize, _thread: ThreadId) {
    DEADLINE_OVERRUN_CALLBACKS.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "Rust" fn count_candidate_deadline_overrun(_data: usize, _thread: ThreadId) {
    CANDIDATE_DEADLINE_CALLBACKS.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "Rust" fn race_deadline_overrun(data: usize, _thread: ThreadId) {
    let callback = unsafe {
        // SAFETY: the extension payload comes from a leaked DeadlineCallbackRace.
        &*core::ptr::with_exposed_provenance::<DeadlineCallbackRace>(data)
    };
    let previous = callback.invocations.fetch_add(1, Ordering::AcqRel);
    if previous == 0 {
        callback.entered.wait();
        callback.release.wait();
    }
}

unsafe extern "Rust" fn record_policy_applied(
    _data: usize,
    _thread: ThreadId,
    _policy: SchedulePolicy,
    observed_ns: u64,
) {
    POLICY_APPLIED_AT_NS.store(observed_ns, Ordering::Release);
    POLICY_APPLIED_CALLBACKS.fetch_add(1, Ordering::Release);
}

unsafe extern "Rust" fn count_rotating_deadline_overrun(_data: usize, _thread: ThreadId) {
    ROTATING_DEADLINE_CALLBACKS.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "Rust" fn count_exit_callback(_data: usize, _thread: ThreadId) {
    EXIT_CALLBACK_INVOCATIONS.fetch_add(1, Ordering::Release);
}

unsafe extern "Rust" fn no_extension_drop(_data: usize) {}
