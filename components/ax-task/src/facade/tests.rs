#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{
        CpuId, Nice, RtPriority, SchedulePolicy, SwitchReason, ThreadExtension, ThreadExtensionOps,
        ThreadSpec,
        inbox::{InboxKind, InboxMessage, InboxNode, PublishResult},
        runtime::{AddressSpaceHandle, AddressSpaceToken},
        test_runtime,
        timer::ExpiredTaskDeadline,
    };

    static PARKING_EXIT_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
    static REENTRANT_EXIT_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
    static REENTRANT_EXIT_CALLBACKS_IN_IRQ_EXIT: AtomicUsize = AtomicUsize::new(0);
    static SWITCH_IN_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

    fn instant(nanos: u64) -> MonotonicInstant {
        MonotonicInstant::from_nanos(nanos).unwrap()
    }

    fn deadline(nanos: u64) -> MonotonicDeadline {
        MonotonicDeadline::from_nanos(nanos).unwrap()
    }

    fn arm_test_deadline(
        cpu: Pin<&CpuLocal>,
        node: &crate::timer::TaskDeadlineNode,
        deadline: MonotonicDeadline,
        kind: TaskDeadlineKind,
    ) -> crate::timer::TaskDeadlineRegistration {
        cpu.remote()
            .lock_deadline_activity(crate::DeadlineBaseGuardSource::TestInspection)
            .queue
            .arm(node, deadline, kind)
            .unwrap()
    }

    fn next_test_deadline(cpu: Pin<&CpuLocal>) -> Option<MonotonicDeadline> {
        cpu.remote()
            .read_deadline_base(crate::DeadlineBaseGuardSource::TestInspection)
            .queue
            .next_deadline()
    }

    fn owner_snapshot(system: &TaskSystem, cpu: Pin<&CpuLocal>) -> crate::CpuSnapshot {
        let _irq = RuntimeIrqGuard::enter();
        system.snapshot(cpu).unwrap()
    }

    fn publish_unrelated_expired_deadline(
        system: &TaskSystem,
        cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> ThreadHandle {
        let unrelated = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let _registration = arm_test_deadline(
            cpu.as_ref(),
            unrelated.sleep_timer(),
            deadline(now_ns),
            TaskDeadlineKind::park_timeout(0),
        );
        assert_eq!(on_clock_event(instant(now_ns), 1).unwrap().expired(), 1);
        unrelated
    }

    static ORDERING_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: record_switch_in_after_address_space,
        on_switch_out: ignore_switch_out,
        on_exit: ignore_thread_event,
        on_deadline_overrun: ignore_thread_event,
        drop: ignore_drop,
    };

    static PARKING_EXIT_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: ignore_switch_in,
        on_switch_out: ignore_switch_out,
        on_exit: count_parking_exit,
        on_deadline_overrun: ignore_thread_event,
        drop: ignore_drop,
    };

    static REENTRANT_EXIT_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: ignore_switch_in,
        on_switch_out: ignore_switch_out,
        on_exit: count_reentrant_exit,
        on_deadline_overrun: ignore_thread_event,
        drop: ignore_drop,
    };

    static TRACE_ORDER_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: ignore_switch_in,
        on_switch_out: record_switch_out,
        on_exit: ignore_thread_event,
        on_deadline_overrun: ignore_thread_event,
        drop: ignore_drop,
    };

    #[test]
    fn preparing_next_address_space_does_not_run_incoming_thread_callbacks() {
        test_runtime::reset_installed_address_space();
        SWITCH_IN_CALLBACKS.store(0, Ordering::Release);
        let extension = unsafe {
            // SAFETY: the callback table interprets data only as the expected
            // address-space scalar and owns no external resource.
            ThreadExtension::new(0, &ORDERING_EXTENSION_OPS)
        };

        prepare_next_address_space(
            AddressSpaceHandle::NONE,
            AddressSpaceHandle::NONE,
            ThreadId::from_parts(1, 1),
        );

        assert_eq!(test_runtime::installed_address_space(), Some(0));
        assert_eq!(
            SWITCH_IN_CALLBACKS.load(Ordering::Acquire),
            0,
            "incoming callbacks must not run before current-thread publication"
        );
        let _extension = extension;
    }

    #[test]
    fn incoming_callback_observes_the_published_current_thread() {
        use crate::{
            ThreadResources,
            runtime::{ExecutionContextHandle, StackHandle, TlsHandle},
        };

        test_runtime::reset_installed_address_space();
        test_runtime::reset_context_switch_tail_count();
        SWITCH_IN_CALLBACKS.store(0, Ordering::Release);
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let previous = unsafe {
            // SAFETY: the test runtime treats these distinct scalar handles as
            // inert identities for the modeled switch lifetime.
            ThreadResources::new(
                ExecutionContextHandle::from_raw(11),
                StackHandle::from_raw(12),
                TlsHandle::from_raw(13),
                AddressSpaceToken::NONE,
            )
        };
        system
            .install_bootstrap_thread(cpu.as_mut(), unsafe {
                ThreadSpec::new(SchedulePolicy::default()).with_resources(previous)
            })
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let extension = unsafe {
            // SAFETY: the callback table stores no borrowed state and its data
            // is the expected lazy-kernel address-space identity.
            ThreadExtension::new(0, &ORDERING_EXTENSION_OPS)
        };
        let next = system
            .create_thread(unsafe {
                ThreadSpec::new(SchedulePolicy::fifo(RtPriority::new(1).unwrap()))
                    .with_resources(ThreadResources::new(
                        ExecutionContextHandle::from_raw(21),
                        StackHandle::from_raw(22),
                        TlsHandle::from_raw(23),
                        AddressSpaceToken::NONE,
                    ))
                    .with_extension(extension)
            })
            .unwrap();
        system.make_ready(next.id()).unwrap();
        system.enqueue(cpu.as_mut(), next.id()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let _context_switch = test_runtime::allow_context_switch();

        let outcome = schedule_current_cpu().unwrap();

        assert_eq!(outcome.decision().unwrap().next(), next.id());
        assert_eq!(current_thread_id().unwrap(), next.id());
        assert_eq!(test_runtime::context_switch_tail_count(), 1);
        assert_eq!(SWITCH_IN_CALLBACKS.load(Ordering::Acquire), 1);
    }

    #[test]
    fn membarrier_registration_publishes_requested_then_ready_for_current_mm() {
        use crate::{
            MembarrierError, ThreadResources,
            runtime::{ExecutionContextHandle, MembarrierRegistration, StackHandle, TlsHandle},
        };

        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let address_space = unsafe {
            // SAFETY: the unit runtime treats these distinct scalar resources
            // as live for the complete fixture and consumes them at teardown.
            AddressSpaceToken::from_raw(0x4d42_1000)
        };
        let address_space_handle = address_space.handle();
        let resources = unsafe {
            // SAFETY: every non-zero scalar has one unique fixture owner.
            ThreadResources::new(
                ExecutionContextHandle::from_raw(0x4d42_1001),
                StackHandle::from_raw(0x4d42_1002),
                TlsHandle::from_raw(0x4d42_1003),
                address_space,
            )
        };
        system
            .install_bootstrap_thread(cpu.as_mut(), unsafe {
                ThreadSpec::new(SchedulePolicy::default()).with_resources(resources)
            })
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        // Linux permits GLOBAL_EXPEDITED regardless of whether the caller's
        // mm registered it; registration only controls which remote rq values
        // are selected as targets.
        membarrier(MembarrierCommand::GlobalExpedited).unwrap();
        assert_eq!(
            membarrier(MembarrierCommand::PrivateExpedited),
            Err(MembarrierError::NotRegistered)
        );
        register_current_membarrier(MembarrierRegistration::PrivateExpedited).unwrap();
        let state = task_runtime::address_space_membarrier_state(address_space_handle);
        assert!(state.requested(MembarrierRegistration::PrivateExpedited));
        assert!(state.ready(MembarrierRegistration::PrivateExpedited));
        let rq_state = cpu.remote().lock_run_queue(crate::RunQueueGuardSource::TestInspection).membarrier_state();
        assert_eq!(rq_state.identity(), state.identity());
        assert!(rq_state.requested(MembarrierRegistration::PrivateExpedited));
        assert!(
            !rq_state.ready(MembarrierRegistration::PrivateExpedited),
            "registration synchronizes requested state before mm publishes ready"
        );
        membarrier(MembarrierCommand::PrivateExpedited).unwrap();
    }

    #[test]
    fn timer_expiry_during_parking_is_committed_by_the_owner_thread() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut ticket) = prepare_current_park(&permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = permit;
        arm_current_park_deadline(&running, &mut ticket, deadline(0)).unwrap();

        assert_eq!(on_clock_event(instant(0), 64).unwrap().expired(), 1);
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            crate::ThreadState::Parking,
            "hard IRQ only publishes soft-timer work"
        );
        assert_eq!(
            owner_snapshot(&system, cpu.as_ref()).runnable(),
            1,
            "Linux rq->nr_running still accounts current until the owner park transaction commits"
        );
        assert!(
            !current_cpu_needs_resched().unwrap(),
            "a soft timeout must wake ktimers/%u instead of forcing an IRQ-return scheduler pass"
        );
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            crate::ThreadState::Parking
        );
        assert!(!owner_snapshot(&system, cpu.as_ref()).need_resched());

        commit_current_park(&mut ticket).unwrap();
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            crate::ThreadState::Running
        );
        assert_eq!(
            owner_snapshot(&system, cpu.as_ref()).runnable(),
            1,
            "a timeout which wins before park commit keeps current runnable"
        );
        assert!(
            !owner_snapshot(&system, cpu.as_ref()).need_resched(),
            "a timeout which wins before park commit leaves current running and consumes the \
             request"
        );
        assert!(!cancel_current_park_deadline(&running, &mut ticket).unwrap());
    }

    #[test]
    fn os_waiter_park_transaction_restores_running_on_cancel() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        let CurrentParkStart::Prepared(park) = begin_current_park().unwrap() else {
            panic!("fresh OS waiter park must publish Parking");
        };
        assert_eq!(park.thread_id(), running.id());
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            crate::ThreadState::Parking
        );

        park.cancel().unwrap();
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            crate::ThreadState::Running
        );
    }

    #[test]
    fn scheduler_safe_point_does_not_scan_an_idle_deadline_queue() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        assert_eq!(
            system
                .service_ktimer_work(cpu.as_mut())
                .unwrap()
                .processed(),
            0
        );
        assert_eq!(
            cpu.deadline_expire_passes_for_test(),
            0,
            "an idle scheduler safe point must not enter the timer expiry engine"
        );
    }

    #[cfg(feature = "qperf-metrics")]
    #[test]
    fn empty_deadline_base_probes_do_not_enter_irq_scope() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let before = crate::qperf_scheduler_metrics_snapshot();

        assert_eq!(cpu.as_mut().next_oneshot_deadline(instant(1)), None);
        assert_eq!(cpu.as_mut().next_oneshot_deadline(instant(2)), None);
        assert_eq!(
            cpu.as_mut().take_due_scheduler_deadline(instant(2)),
            (None, false)
        );
        let soft = cpu.as_mut().promote_due_task_deadlines(instant(2), 1);
        assert_eq!(soft.processed(), 0);
        assert_eq!(soft.expired(), 0);
        assert!(!soft.pending());
        assert_eq!(soft.next_deadline(), None);

        let after = crate::qperf_scheduler_metrics_snapshot();
        let observation_entries = after.irq_ticket_cpu_deadline_observation_entries
            - before.irq_ticket_cpu_deadline_observation_entries;
        let hard_expiry_entries = after.irq_ticket_cpu_deadline_hard_expiry_entries
            - before.irq_ticket_cpu_deadline_hard_expiry_entries;
        let soft_expiry_entries = after.irq_ticket_cpu_deadline_soft_expiry_entries
            - before.irq_ticket_cpu_deadline_soft_expiry_entries;
        assert_eq!(
            (
                observation_entries,
                hard_expiry_entries,
                soft_expiry_entries
            ),
            (0, 0, 0),
            "an empty deadline base must not be locked for observation or expiry"
        );
    }

    #[test]
    fn scheduler_fast_path_does_not_read_the_physical_clock_without_task_deadlines() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::reset_monotonic_reads();

        assert!(matches!(
            schedule_current_cpu().unwrap(),
            SchedulerOutcome::Quiescent
        ));
        assert_eq!(
            test_runtime::monotonic_reads(),
            0,
            "a scheduler fast path without task deadlines must only sample the runqueue clock"
        );
    }

    #[test]
    fn forced_yield_does_not_republish_an_unchanged_clockevent() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        let initial = {
            let _irq = RuntimeIrqGuard::enter();
            cpu.as_mut()
                .next_scheduler_deadline_update(
                    instant(0),
                    crate::SchedulerDeadlineDerivationSource::ScheduleSelection,
                )
                .unwrap()
        };
        task_runtime::publish_scheduler_deadline(initial);
        assert!(test_runtime::take_scheduler_deadline_update().is_some());

        yield_current_cpu().unwrap();

        assert!(
            test_runtime::take_scheduler_deadline_update().is_none(),
            "sched_yield must not republish an unchanged logical clockevent"
        );
    }

    #[test]
    fn current_affinity_update_does_not_service_unrelated_deadlines() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let unrelated =
            publish_unrelated_expired_deadline(system.as_ref().get_ref(), cpu.as_mut(), 10);
        test_runtime::reset_monotonic_reads();

        set_current_thread_affinity(CpuSet::all(1)).unwrap();

        assert_eq!(
            test_runtime::monotonic_reads(),
            0,
            "an affinity update that does not migrate must not enter a scheduler safe point"
        );
        let mut expired = [ExpiredTaskDeadline::EMPTY; 1];
        assert_eq!(cpu.as_mut().take_expired_task_deadlines(&mut expired), 1);
        assert_eq!(
            expired[0].thread(),
            Some(unrelated.id()),
            "affinity mutation must leave unrelated task deadlines to the safe-point owner"
        );
    }

    #[test]
    fn park_commit_does_not_service_unrelated_deadlines() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let unrelated =
            publish_unrelated_expired_deadline(system.as_ref().get_ref(), cpu.as_mut(), 10);
        let permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut ticket) = prepare_current_park(&permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = permit;
        assert_eq!(running.wake_handle().wake(), crate::WakeResult::Notified);
        test_runtime::reset_monotonic_reads();
        test_runtime::reset_scheduler_reads();

        commit_current_park(&mut ticket).unwrap();

        assert_eq!(
            test_runtime::monotonic_reads(),
            0,
            "park commit must not read the physical clock when no timer publication changes"
        );
        assert_eq!(
            test_runtime::scheduler_reads(),
            1,
            "park commit must use one runqueue-clock sample"
        );
        let mut expired = [ExpiredTaskDeadline::EMPTY; 1];
        assert_eq!(cpu.as_mut().take_expired_task_deadlines(&mut expired), 1);
        assert_eq!(
            expired[0].thread(),
            Some(unrelated.id()),
            "park commit must not consume another thread's deadline work"
        );
    }

    #[test]
    fn scheduler_safe_point_never_executes_irq_claimed_soft_timeout() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut ticket) = prepare_current_park(&permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = permit;
        arm_current_park_deadline(&running, &mut ticket, deadline(10)).unwrap();

        test_runtime::set_monotonic_ns(10);
        assert!(
            schedule_current_cpu().unwrap().parking_deferred(),
            "an unclaimed soft timer must not change the PARKING handshake"
        );
        assert!(
            !running.core.take_park_notification(),
            "scheduler entry must not poll an unclaimed timer heap"
        );

        assert_eq!(on_clock_event(instant(10), 64).unwrap().expired(), 1);
        assert!(schedule_current_cpu().unwrap().parking_deferred());
        assert!(
            !running.core.take_park_notification(),
            "the IRQ-off scheduler frame must leave soft timeout wakeup to the timer worker"
        );
        assert_eq!(
            system
                .service_ktimer_work(cpu.as_mut())
                .unwrap()
                .processed(),
            1
        );
        assert!(
            running.core.take_park_notification(),
            "the task-context timer worker must complete the IRQ-claimed expiration"
        );
        assert!(
            !cancel_current_park_deadline(&running, &mut ticket).unwrap(),
            "the claimed timer must be physically consumed exactly once"
        );
        cancel_current_park(&mut ticket).unwrap();
    }

    #[test]
    fn clock_event_publishes_rr_rotation_without_timer_backlog() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let policy =
            SchedulePolicy::round_robin_with_quantum(crate::RtPriority::new(1).unwrap(), 10)
                .unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        system.set_thread_policy(running.id(), policy).unwrap();
        test_runtime::set_scheduler_ns(0);
        system.drain_owner_control(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        // Reclassifying the running task publishes one Linux-style reschedule
        // request. Consume that owner transaction before isolating the RR
        // quantum expiry exercised below.
        schedule_current_cpu().unwrap();
        assert!(!current_cpu_needs_resched().unwrap());
        let peer = system.create_thread(ThreadSpec::new(policy)).unwrap();
        system.make_ready(peer.id()).unwrap();
        {
            let _irq = RuntimeIrqGuard::enter();
            system.enqueue(cpu.as_mut(), peer.id()).unwrap();
        }
        test_runtime::set_scheduler_ns(10);
        let outcome = on_clock_event(instant(10), 64).unwrap();

        assert!(outcome.slice_expired());
        assert_eq!(outcome.expired(), 0);
        assert!(
            !outcome.pending(),
            "a completed RR class hook must not manufacture timer backlog"
        );
        assert!(
            current_cpu_needs_resched().unwrap(),
            "the scheduler core must publish owner-local preemption before returning to runtime"
        );
    }

    #[test]
    fn scheduler_work_preserves_owner_preemption_policy() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        cpu.request_scheduler_work();
        assert!(!cpu.remote().take_preempt_requested());

        assert!(
            !cpu.remote().take_preempt_requested(),
            "delivery must enter the owner safe point without forcing a task switch"
        );
    }

    #[test]
    fn irq_budget_exhaustion_defers_backlog_without_resolution_rate_rearm() {
        let system =
            Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1).with_batch_limit(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let first = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let second = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let _first_registration = arm_test_deadline(
            cpu.as_ref(),
            first.sleep_timer(),
            deadline(10),
            TaskDeadlineKind::park_timeout(0),
        );
        let _second_registration = arm_test_deadline(
            cpu.as_ref(),
            second.sleep_timer(),
            deadline(10),
            TaskDeadlineKind::park_timeout(0),
        );

        test_runtime::set_monotonic_ns(10);
        test_runtime::set_scheduler_ns(10);
        let outcome = on_clock_event(instant(10), 1).unwrap();

        assert_eq!(outcome.expired(), 1);
        assert!(outcome.pending());
        assert!(
            outcome
                .update()
                .deadline()
                .is_none_or(|deadline| deadline.as_nanos() > 11),
            "an expired bounded backlog must be advanced by ktimers, not by a timer interrupt \
             rearmed at the 1ns hardware resolution"
        );
        assert!(
            cpu.remote().ktimer_event().is_pending(),
            "hard IRQ must publish the per-CPU ktimer event after transferring timeout payload"
        );
    }

    #[test]
    fn clockevent_buffer_preserves_unconsumed_expirations() {
        let system = Box::pin(
            TaskSystem::new(
                crate::TaskSystemConfig::new(1)
                    .with_thread_capacity(3)
                    .with_batch_limit(2),
            )
            .unwrap(),
        );
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let first = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let second = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let _first_registration = arm_test_deadline(
            cpu.as_ref(),
            first.sleep_timer(),
            deadline(10),
            TaskDeadlineKind::park_timeout(1),
        );
        let _second_registration = arm_test_deadline(
            cpu.as_ref(),
            second.sleep_timer(),
            deadline(10),
            TaskDeadlineKind::park_timeout(1),
        );

        assert_eq!(on_clock_event(instant(10), 2).unwrap().expired(), 2);
        let mut first_event = [ExpiredTaskDeadline::EMPTY; 1];
        assert_eq!(
            cpu.as_mut().take_expired_task_deadlines(&mut first_event),
            1
        );
        let mut second_event = [ExpiredTaskDeadline::EMPTY; 1];
        assert_eq!(
            cpu.as_mut().take_expired_task_deadlines(&mut second_event),
            1
        );
        assert_ne!(first_event[0].thread(), second_event[0].thread());
        assert_eq!(
            cpu.as_mut().take_expired_task_deadlines(&mut second_event),
            0
        );
    }

    #[test]
    fn ktimer_worker_finishes_clockevent_backlog_without_another_irq() {
        let system = Box::pin(
            TaskSystem::new(
                crate::TaskSystemConfig::new(1)
                    .with_thread_capacity(4)
                    .with_batch_limit(1),
            )
            .unwrap(),
        );
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let threads: [ThreadHandle; 3] = core::array::from_fn(|_| {
            system
                .create_thread(ThreadSpec::new(SchedulePolicy::default()))
                .unwrap()
        });
        let _registrations = threads
            .iter()
            .map(|thread| {
                arm_test_deadline(
                    cpu.as_ref(),
                    thread.sleep_timer(),
                    deadline(10),
                    TaskDeadlineKind::park_timeout(1),
                )
            })
            .collect::<alloc::vec::Vec<_>>();

        test_runtime::set_monotonic_ns(10);
        test_runtime::set_scheduler_ns(10);
        let irq = on_clock_event(instant(10), 1).unwrap();
        assert_eq!(irq.expired(), 1);
        assert!(irq.pending());
        let mut processed = 0;
        let mut passes = 0;
        while !cpu
            .remote()
            .read_deadline_base(crate::DeadlineBaseGuardSource::TestInspection)
            .queue
            .is_empty()
            || cpu.has_expired_task_deadlines()
        {
            let batch = system.service_ktimer_work(cpu.as_mut()).unwrap();
            processed += batch.processed();
            passes += 1;
            assert!(passes <= 3, "bounded ktimer work must make progress");
        }
        assert_eq!(processed, 3);
        assert!(
            cpu.remote()
                .read_deadline_base(crate::DeadlineBaseGuardSource::TestInspection)
                .queue
                .is_empty()
        );
    }

    #[test]
    fn unavailable_park_deadline_owner_preserves_ticket_for_retry() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut ticket) = prepare_current_park(&permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = permit;
        arm_current_park_deadline(&running, &mut ticket, deadline(10)).unwrap();
        let token = ticket
            .deadline()
            .expect("armed park deadline token")
            .token();

        running
            .core
            .register_sleep_timer(CpuId::new(1), token.generation());
        assert_eq!(
            cancel_current_park_deadline(&running, &mut ticket),
            Err(TaskError::CpuOffline(1))
        );
        assert!(
            ticket.has_deadline(),
            "an unavailable owner must not consume the move-only deadline token"
        );
        assert_eq!(
            next_test_deadline(cpu.as_ref()),
            Some(deadline(10)),
            "a failed cancellation must leave the physical queue entry intact"
        );

        running
            .core
            .register_sleep_timer(CpuId::new(0), token.generation());
        assert!(cancel_current_park_deadline(&running, &mut ticket).unwrap());
        assert!(!ticket.has_deadline());
        assert_eq!(next_test_deadline(cpu.as_ref()), None);
        cancel_current_park(&mut ticket).unwrap();
    }

    #[test]
    fn migrated_waiter_cancels_the_deadline_on_its_remote_timer_base() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(2)).unwrap());
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system
            .install_bootstrap_thread(cpu1.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu0.as_mut()).unwrap();
        system.bring_cpu_online(cpu1.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu0.as_mut());

        let mut ticket = crate::ParkTicket::new(running.id(), 1);
        let registration = arm_test_deadline(
            cpu1.as_ref(),
            running.sleep_timer(),
            deadline(10),
            TaskDeadlineKind::park_timeout(ticket.generation()),
        );
        let token = registration.token();
        running
            .core
            .register_sleep_timer(CpuId::new(1), token.generation());
        ticket.attach_deadline(registration).unwrap();

        assert!(cancel_current_park_deadline(&running, &mut ticket).unwrap());
        assert!(!ticket.has_deadline());
        assert_eq!(running.core.sleep_timer_cpu(), None);
        assert_eq!(next_test_deadline(cpu1.as_ref()), None);
    }

    #[test]
    fn migrated_waiter_claims_expiration_from_its_remote_timer_base() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(2)).unwrap());
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system
            .install_bootstrap_thread(cpu1.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu0.as_mut()).unwrap();
        system.bring_cpu_online(cpu1.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu0.as_mut());

        let mut ticket = crate::ParkTicket::new(running.id(), 1);
        let registration = arm_test_deadline(
            cpu1.as_ref(),
            running.sleep_timer(),
            deadline(10),
            TaskDeadlineKind::park_timeout(ticket.generation()),
        );
        let token = registration.token();
        running
            .core
            .register_sleep_timer(CpuId::new(1), token.generation());
        ticket.attach_deadline(registration).unwrap();
        assert_eq!(
            cpu1.as_mut()
                .promote_due_task_deadlines(instant(10), 1)
                .expired(),
            1
        );

        assert!(!cancel_current_park_deadline(&running, &mut ticket).unwrap());
        assert!(!ticket.has_deadline());
        assert_eq!(running.core.sleep_timer_cpu(), None);
        assert!(!cpu1.has_expired_task_deadlines());
    }

    #[test]
    fn stale_expiration_cannot_notify_a_new_park_generation() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        let first_permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut first) = prepare_current_park(&first_permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = first_permit;
        arm_current_park_deadline(&running, &mut first, deadline(10)).unwrap();
        assert_eq!(on_clock_event(instant(10), 1).unwrap().expired(), 1);
        assert!(!cancel_current_park_deadline(&running, &mut first).unwrap());
        cancel_current_park(&mut first).unwrap();

        let second_permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut second) = prepare_current_park(&second_permit).unwrap()
        else {
            panic!("the next park generation must be independently prepared");
        };
        let _ = second_permit;
        arm_current_park_deadline(&running, &mut second, deadline(100)).unwrap();

        test_runtime::set_monotonic_ns(10);
        schedule_current_cpu().unwrap();
        assert!(
            !running.core.take_park_notification(),
            "an expiration buffered for the preceding park generation must not wake the rearm"
        );

        assert!(cancel_current_park_deadline(&running, &mut second).unwrap());
        cancel_current_park(&mut second).unwrap();
    }

    #[test]
    fn failed_deadline_update_rolls_back_the_unpublished_park_timer() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut ticket) = prepare_current_park(&permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = permit;
        cpu.as_mut()
            .set_scheduler_deadline_generation_for_test(u64::MAX);

        assert_eq!(
            arm_current_park_deadline(&running, &mut ticket, deadline(10)),
            Err(TaskError::InvalidConfiguration)
        );
        assert!(!ticket.has_deadline());
        assert_eq!(running.core.sleep_timer_cpu(), None);
        assert_eq!(next_test_deadline(cpu.as_ref()), None);

        cancel_current_park(&mut ticket).unwrap();
    }

    #[test]
    fn failed_cancel_update_preserves_the_live_deadline_transaction() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut ticket) = prepare_current_park(&permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = permit;
        arm_current_park_deadline(&running, &mut ticket, deadline(10)).unwrap();
        cpu.as_mut()
            .set_scheduler_deadline_generation_for_test(u64::MAX);

        assert_eq!(
            cancel_current_park_deadline(&running, &mut ticket),
            Err(TaskError::InvalidConfiguration)
        );
        assert!(
            ticket.has_deadline(),
            "a failed publication-state update must roll back the queue cancellation"
        );
        assert_eq!(running.core.sleep_timer_cpu(), Some(CpuId::new(0)));
        assert_eq!(next_test_deadline(cpu.as_ref()), Some(deadline(10)));

        cpu.as_mut().set_scheduler_deadline_generation_for_test(1);
        assert!(cancel_current_park_deadline(&running, &mut ticket).unwrap());
        cancel_current_park(&mut ticket).unwrap();
    }

    #[test]
    fn parking_safe_point_is_bounded_and_does_not_run_task_work() {
        PARKING_EXIT_CALLBACKS.store(0, Ordering::Release);
        let owner_work_nodes = [
            Box::pin(InboxNode::new(InboxKind::OwnerControl)),
            Box::pin(InboxNode::new(InboxKind::OwnerControl)),
        ];
        let system =
            Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1).with_batch_limit(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let extension = unsafe {
            // SAFETY: the static callback table interprets no extension data.
            ThreadExtension::new(0, &PARKING_EXIT_EXTENSION_OPS)
        };
        let exited = system
            .create_thread(
                ThreadSpec::new(SchedulePolicy::fifo(RtPriority::new(1).unwrap()))
                    .with_extension(extension),
            )
            .unwrap();
        system.make_ready(exited.id()).unwrap();
        system.enqueue(cpu.as_mut(), exited.id()).unwrap();
        assert_eq!(system.schedule(cpu.as_mut()).unwrap().next(), exited.id());
        system.complete_context_switch(cpu.as_mut()).unwrap();
        let exit_decision = system.exit_current(cpu.as_mut()).unwrap();
        assert_ne!(exit_decision.next(), exited.id());
        assert_eq!(PARKING_EXIT_CALLBACKS.load(Ordering::Acquire), 0);

        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut ticket) = prepare_current_park(&permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = permit;

        for (index, node) in owner_work_nodes.iter().enumerate() {
            let slot = (index + 1) as u32;
            let message = InboxMessage::migration(
                ThreadId::from_parts(slot, 1),
                CpuId::new(0),
                CpuId::new(0),
                u64::from(slot),
                1_024,
            );
            let node = unsafe {
                // The pinned fixture is declared before the task system, so it
                // outlives the CPU inbox even when one bounded batch remains.
                Pin::new_unchecked(&*(node.as_ref().get_ref() as *const InboxNode))
            };
            assert_eq!(
                cpu.remote().publish_owner_control(node, message),
                PublishResult::Published
            );
        }

        assert!(schedule_current_cpu().unwrap().parking_deferred());
        assert!(
            cpu.has_remote_work(),
            "one owner-work batch must remain pending"
        );
        assert!(
            cpu.needs_reschedule(),
            "remaining work must retain its doorbell"
        );
        assert_eq!(
            PARKING_EXIT_CALLBACKS.load(Ordering::Acquire),
            0,
            "task-work must not run while current owns a park token"
        );
        cancel_current_park(&mut ticket).unwrap();
    }

    #[test]
    fn scheduler_frame_guard_covers_work_before_the_context_switch() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::reset_scheduler_frame_state();

        let _decision = schedule_current_cpu().unwrap();

        assert_eq!(
            test_runtime::scheduler_frame_state(),
            (0, 1, 0),
            "an empty safe point needs only the scheduler baton"
        );
    }

    #[test]
    fn irq_exit_scheduler_reentry_does_not_nest_task_work_on_one_thread_stack() {
        REENTRANT_EXIT_CALLBACKS.store(0, Ordering::Release);
        REENTRANT_EXIT_CALLBACKS_IN_IRQ_EXIT.store(0, Ordering::Release);
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let bootstrap = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let extension = unsafe {
            // SAFETY: the callback table owns no external data and records only
            // whether task work ran inside the configured scheduler reentry.
            ThreadExtension::new(0, &REENTRANT_EXIT_EXTENSION_OPS)
        };
        let exiting = system
            .create_thread(
                ThreadSpec::new(SchedulePolicy::fifo(RtPriority::new(1).unwrap()))
                    .with_extension(extension),
            )
            .unwrap();
        system.make_ready(exiting.id()).unwrap();
        system.enqueue(cpu.as_mut(), exiting.id()).unwrap();
        assert_eq!(system.schedule(cpu.as_mut()).unwrap().next(), exiting.id());
        system.complete_context_switch(cpu.as_mut()).unwrap();
        assert_eq!(
            system.exit_current(cpu.as_mut()).unwrap().next(),
            bootstrap.id()
        );
        system.complete_context_switch(cpu.as_mut()).unwrap();

        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::reset_scheduler_frame_state();
        test_runtime::configure_irq_exit_schedule_reentry(1);
        assert!(matches!(
            schedule_current_cpu().unwrap(),
            SchedulerOutcome::Quiescent
        ));
        test_runtime::configure_irq_exit_schedule_reentry(0);

        assert_eq!(
            REENTRANT_EXIT_CALLBACKS.load(Ordering::Acquire),
            0,
            "scheduler frames must only publish task work for the service thread"
        );
        let batch = system.service_deferred_task_work(1).unwrap();
        assert!(batch.made_progress());
        assert_eq!(REENTRANT_EXIT_CALLBACKS.load(Ordering::Acquire), 1);
        assert_eq!(
            REENTRANT_EXIT_CALLBACKS_IN_IRQ_EXIT.load(Ordering::Acquire),
            0,
            "nested scheduler completion must not recursively run task work on the active stack"
        );
        assert_eq!(
            test_runtime::scheduler_frame_state().1,
            1,
            "IRQ guard exit under preemption exclusion must not create a nested scheduler frame"
        );
    }

    #[test]
    fn busy_task_work_consumer_is_retryable_for_the_service_thread() {
        let system = TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap();
        let doorbell = system.task_work_doorbell();
        let _consumer = doorbell.try_claim_consumer().unwrap();

        assert_eq!(service_task_work_pass(&system, &doorbell, 1).unwrap(), None);
        assert!(
            doorbell.is_pending(),
            "the worker must retain a sticky retry after losing consumer ownership"
        );
    }

    #[test]
    fn repeated_task_work_publication_yields_before_retry() {
        assert_eq!(
            task_work_service_action(Some(crate::DeferredTaskWorkBatch::default()), true, 64,),
            TaskWorkServiceAction::Yield,
            "a sticky publication after an under-budget pass must not spin the service thread"
        );
    }

    #[test]
    fn scheduler_safe_point_drains_owner_control_after_resched_bit_was_consumed() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let waiting = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.make_ready(waiting.id()).unwrap();
        system.enqueue(cpu.as_mut(), waiting.id()).unwrap();

        system
            .set_affinity(waiting.id(), crate::CpuSet::all(1))
            .unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        assert!(cpu.has_remote_work());
        assert!(cpu.needs_reschedule());

        // Forced schedule paths used to consume the sticky bit without first
        // draining owner control. Claiming scheduler entry must re-observe the
        // published affinity request and preserve a doorbell for the next
        // bounded drain.
        let _ = cpu.remote().take_preempt_requested();
        assert!(cpu.needs_reschedule());

        let _outcome = schedule_current_cpu().unwrap();
        assert!(
            !cpu.has_remote_work(),
            "pending owner work must be sufficient to enter the scheduler safe point"
        );
    }

    #[test]
    fn context_switch_uses_one_scheduler_frame() {
        use crate::{
            ThreadResources,
            runtime::{ExecutionContextHandle, StackHandle, TlsHandle},
        };

        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let bootstrap_resources = unsafe {
            ThreadResources::new(
                ExecutionContextHandle::from_raw(1),
                StackHandle::from_raw(2),
                TlsHandle::from_raw(3),
                AddressSpaceToken::NONE,
            )
        };
        system
            .install_bootstrap_thread(cpu.as_mut(), unsafe {
                ThreadSpec::new(SchedulePolicy::default()).with_resources(bootstrap_resources)
            })
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let next_resources = unsafe {
            ThreadResources::new(
                ExecutionContextHandle::from_raw(4),
                StackHandle::from_raw(5),
                TlsHandle::from_raw(6),
                AddressSpaceToken::NONE,
            )
        };
        let next = system
            .create_thread(unsafe {
                ThreadSpec::new(SchedulePolicy::fifo(crate::RtPriority::new(1).unwrap()))
                    .with_resources(next_resources)
            })
            .unwrap();
        system.make_ready(next.id()).unwrap();
        system.enqueue(cpu.as_mut(), next.id()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let _context_switch = test_runtime::allow_context_switch();
        test_runtime::reset_scheduler_frame_state();
        test_runtime::reset_cpu_handle_reads();

        let decision = schedule_current_cpu().unwrap().decision().unwrap();

        assert!(decision.requires_context_switch());
        assert_eq!(
            test_runtime::cpu_handle_reads(),
            (2, 0),
            "scheduler entry and switch return must use current-CPU owner snapshots without \
             resolving the current endpoint through the global registry"
        );
        assert_eq!(
            test_runtime::cpu_owner_claims(),
            2,
            "the common scheduler path must use one owner transaction before switch and one \
             switch-tail transaction after return"
        );
        assert_eq!(
            test_runtime::scheduler_frame_state(),
            (0, 1, 1),
            "one scheduling operation must use one scheduler baton while irq-safe shared locks \
             nest inside it"
        );
        assert_eq!(
            test_runtime::irq_guards_at_context_switch(),
            0,
            "ordinary same-CPU IRQ tokens must be released before raw switch"
        );
    }

    #[test]
    fn switch_trace_observes_previous_before_switch_out_callback() {
        use crate::{
            ThreadResources,
            runtime::{ExecutionContextHandle, StackHandle, TlsHandle},
            test_runtime::SwitchObservation,
        };

        test_runtime::reset_switch_observations();
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let previous_resources = unsafe {
            // SAFETY: the unit runtime treats these unique scalar handles as
            // inert identities for the duration of this switch.
            ThreadResources::new(
                ExecutionContextHandle::from_raw(11),
                StackHandle::from_raw(12),
                TlsHandle::from_raw(13),
                AddressSpaceToken::NONE,
            )
        };
        let extension = unsafe {
            // SAFETY: the static callback records only value-type switch data.
            ThreadExtension::new(0, &TRACE_ORDER_EXTENSION_OPS)
        };
        let previous = system
            .install_bootstrap_thread(cpu.as_mut(), unsafe {
                ThreadSpec::new(SchedulePolicy::default())
                    .with_resources(previous_resources)
                    .with_extension(extension)
            })
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let next_resources = unsafe {
            // SAFETY: this bundle owns distinct inert handles.
            ThreadResources::new(
                ExecutionContextHandle::from_raw(21),
                StackHandle::from_raw(22),
                TlsHandle::from_raw(23),
                AddressSpaceToken::NONE,
            )
        };
        let next = system
            .create_thread(unsafe {
                ThreadSpec::new(SchedulePolicy::fifo(crate::RtPriority::new(1).unwrap()))
                    .with_resources(next_resources)
            })
            .unwrap();
        system.make_ready(next.id()).unwrap();
        system.enqueue(cpu.as_mut(), next.id()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let _context_switch = test_runtime::allow_context_switch();

        let outcome = schedule_current_cpu().unwrap();

        assert!(outcome.decision().unwrap().requires_context_switch());
        let observations = test_runtime::take_switch_observations();
        assert_eq!(observations.len(), 2);
        let SwitchObservation::Trace(record) = observations[0] else {
            panic!("the allocation-free trace must run before OS switch-out");
        };
        assert_eq!(record.previous_thread, previous.id().as_u64());
        assert_eq!(record.next_thread, next.id().as_u64());
        assert_eq!(
            observations[1],
            SwitchObservation::SwitchOut {
                thread: previous.id(),
                reason: SwitchReason::Preempted,
            }
        );
    }

    unsafe extern "Rust" fn record_switch_in_after_address_space(
        data: usize,
        thread: ThreadId,
        _policy: SchedulePolicy,
    ) {
        assert_eq!(test_runtime::installed_address_space(), Some(data));
        assert_eq!(current_thread_id().unwrap(), thread);
        SWITCH_IN_CALLBACKS.fetch_add(1, Ordering::AcqRel);
    }

    unsafe extern "Rust" fn ignore_switch_out(
        _data: usize,
        _thread: ThreadId,
        _reason: SwitchReason,
    ) {
    }

    unsafe extern "Rust" fn record_switch_out(
        _data: usize,
        thread: ThreadId,
        reason: SwitchReason,
    ) {
        test_runtime::record_switch_out(thread, reason);
    }

    unsafe extern "Rust" fn ignore_thread_event(_data: usize, _thread: ThreadId) {}

    unsafe extern "Rust" fn ignore_switch_in(
        _data: usize,
        _thread: ThreadId,
        _policy: SchedulePolicy,
    ) {
    }

    unsafe extern "Rust" fn count_parking_exit(_data: usize, _thread: ThreadId) {
        PARKING_EXIT_CALLBACKS.fetch_add(1, Ordering::AcqRel);
    }

    unsafe extern "Rust" fn count_reentrant_exit(_data: usize, _thread: ThreadId) {
        REENTRANT_EXIT_CALLBACKS.fetch_add(1, Ordering::AcqRel);
        if test_runtime::irq_exit_schedule_reentry_active() {
            REENTRANT_EXIT_CALLBACKS_IN_IRQ_EXIT.fetch_add(1, Ordering::AcqRel);
        }
    }

    unsafe extern "Rust" fn ignore_drop(_data: usize) {}

    #[test]
    fn blocking_context_is_rejected_before_parking_publication() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::set_schedule_context_safe(false);

        let result = acquire_blocking_permit();
        let published_state = system.thread_state(running.id()).unwrap();
        test_runtime::set_schedule_context_safe(true);

        assert!(matches!(result, Err(TaskError::UnsafeContext)));
        assert_eq!(published_state, crate::ThreadState::Running);
    }

    #[test]
    fn hard_irq_cannot_publish_policy_update() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let replacement = SchedulePolicy::fifo(RtPriority::new(1).unwrap());
        test_runtime::set_hard_irq(true);

        let result = set_thread_policy(running.id(), replacement);

        test_runtime::set_hard_irq(false);
        assert_eq!(result, Err(TaskError::UnsafeContext));
        assert_eq!(
            system.thread_policy(running.id()).unwrap(),
            SchedulePolicy::default(),
            "a hard-IRQ caller must not partially publish scheduler policy"
        );
    }

    #[test]
    fn affinity_is_not_published_before_the_scheduler_frame_is_owned() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(2)).unwrap());
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu0.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu0.as_mut()).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        system.bring_cpu_online(cpu1.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu0.as_mut());
        let original = system.thread_affinity(running.id()).unwrap();
        let mut cpu1_only = CpuSet::empty(2);
        assert!(cpu1_only.insert(CpuId::new(1)));
        test_runtime::set_scheduler_frame_enter_status(RuntimeStatus::UnsafeContext);

        let result = set_current_thread_affinity(cpu1_only);

        test_runtime::set_scheduler_frame_enter_status(RuntimeStatus::Success);
        assert!(matches!(result, Err(TaskError::UnsafeContext)));
        assert_eq!(
            system.thread_affinity(running.id()).unwrap(),
            original,
            "a failed scheduler-frame acquisition must not partially publish affinity"
        );
    }

    #[test]
    fn preparing_exit_keeps_the_current_thread_running_until_commit() {
        use crate::{
            ThreadResources,
            runtime::{ExecutionContextHandle, StackHandle, TlsHandle},
        };

        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let resources = unsafe {
            ThreadResources::new(
                ExecutionContextHandle::from_raw(1),
                StackHandle::NONE,
                TlsHandle::NONE,
                AddressSpaceToken::NONE,
            )
        };
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), unsafe {
                ThreadSpec::new(SchedulePolicy::default()).with_resources(resources)
            })
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        let permit = prepare_current_exit().unwrap();

        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            crate::ThreadState::Running
        );
        let _ = permit;
    }

    #[test]
    fn preparing_exit_closes_scheduler_activity_until_permit_drops() {
        use crate::{
            ThreadResources,
            runtime::{ExecutionContextHandle, StackHandle, TlsHandle},
        };

        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let resources = unsafe {
            ThreadResources::new(
                ExecutionContextHandle::from_raw(1),
                StackHandle::NONE,
                TlsHandle::NONE,
                AddressSpaceToken::NONE,
            )
        };
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), unsafe {
                ThreadSpec::new(SchedulePolicy::default()).with_resources(resources)
            })
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        let permit = prepare_current_exit().unwrap();

        assert!(
            running.core.try_scheduler_activity().is_none(),
            "exit preparation must close new scheduler activity before OS completion publication"
        );
        drop(permit);
        assert!(
            running.core.try_scheduler_activity().is_some(),
            "dropping an uncommitted exit permit must reopen scheduler activity"
        );
    }

    #[test]
    fn exit_commit_separates_transition_from_unrelated_deadline_service() {
        use crate::{
            ThreadResources,
            runtime::{ExecutionContextHandle, StackHandle, TlsHandle},
        };

        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let current_resources = unsafe {
            // SAFETY: the unit runtime treats these unique scalar handles as
            // inert identities for the duration of the switch.
            ThreadResources::new(
                ExecutionContextHandle::from_raw(1),
                StackHandle::from_raw(2),
                TlsHandle::from_raw(3),
                AddressSpaceToken::NONE,
            )
        };
        system
            .install_bootstrap_thread(cpu.as_mut(), unsafe {
                ThreadSpec::new(SchedulePolicy::default()).with_resources(current_resources)
            })
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let next_resources = unsafe {
            // SAFETY: this bundle owns distinct inert handles.
            ThreadResources::new(
                ExecutionContextHandle::from_raw(4),
                StackHandle::from_raw(5),
                TlsHandle::from_raw(6),
                AddressSpaceToken::NONE,
            )
        };
        let next = system
            .create_thread(unsafe {
                ThreadSpec::new(SchedulePolicy::fifo(RtPriority::new(1).unwrap()))
                    .with_resources(next_resources)
            })
            .unwrap();
        system.make_ready(next.id()).unwrap();
        system.enqueue(cpu.as_mut(), next.id()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let unrelated =
            publish_unrelated_expired_deadline(system.as_ref().get_ref(), cpu.as_mut(), 10);
        test_runtime::set_monotonic_ns(10);
        test_runtime::set_scheduler_ns(10);
        let permit = prepare_current_exit().unwrap();
        let _context_switch = test_runtime::allow_context_switch();
        test_runtime::reset_monotonic_reads();
        test_runtime::reset_scheduler_reads();

        let exit =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| commit_current_exit(permit)));

        assert!(
            exit.is_err(),
            "the unit runtime must reject returning to an exited context"
        );
        assert_eq!(
            test_runtime::monotonic_reads(),
            1,
            "exit commit must read the physical clock only when publishing the clockevent"
        );
        assert_eq!(
            test_runtime::scheduler_reads(),
            1,
            "exit commit owns the only rq transaction; ordinary validation and switch tail use \
             the staged placement handoff"
        );
        let mut expired = [ExpiredTaskDeadline::EMPTY; 1];
        assert_eq!(cpu.as_mut().take_expired_task_deadlines(&mut expired), 1);
        assert_eq!(
            expired[0].thread(),
            Some(unrelated.id()),
            "exit commit must not consume another thread's deadline work"
        );
    }

    #[test]
    fn current_cpu_reference_keeps_its_irq_pin_alive() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::reset_irq_state();

        let current = runtime_current_cpu().unwrap();

        assert_eq!(current.owner(), CpuId::new(0));
        assert_eq!(
            test_runtime::active_irq_guards(),
            1,
            "the CPU-local reference must retain its migration pin"
        );
        drop(current);
        assert_eq!(test_runtime::active_irq_guards(), 0);
    }

    #[test]
    fn beginning_a_park_uses_one_owner_transaction() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::reset_cpu_handle_reads();

        let CurrentParkStart::Prepared(park) = begin_current_park().unwrap() else {
            panic!("a fresh current thread must prepare its first park")
        };

        let owner_claims = test_runtime::cpu_owner_claims();
        park.cancel().unwrap();
        assert_eq!(
            owner_claims, 1,
            "current identity capture and Parking publication must share one owner transaction"
        );
    }

    #[test]
    fn park_deadline_arm_uses_one_owner_transaction() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let CurrentParkStart::Prepared(mut park) = begin_current_park().unwrap() else {
            panic!("a fresh current thread must prepare its first park")
        };
        test_runtime::reset_cpu_handle_reads();

        park.arm_deadline(deadline(10)).unwrap();

        let owner_claims = test_runtime::cpu_owner_claims();
        park.cancel().unwrap();
        assert_eq!(
            owner_claims, 1,
            "deadline owner validation and heap insertion must share one owner transaction"
        );
    }

    #[test]
    fn overdue_unactivated_soft_timer_keeps_a_physical_clockevent() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let CurrentParkStart::Prepared(mut park) = begin_current_park().unwrap() else {
            panic!("a fresh current thread must prepare its first park")
        };
        park.arm_deadline(deadline(10)).unwrap();

        let mut irq = RuntimeIrqGuard::enter();
        let mut owner = runtime_current_cpu_mut(&mut irq).unwrap();
        let update = owner
            .as_mut()
            .next_scheduler_deadline_update(
                instant(10),
                crate::SchedulerDeadlineDerivationSource::ClockEvent,
            )
            .unwrap();
        assert_eq!(
            update.deadline(),
            Some(deadline(10)),
            "an overdue queue head remains a physical deadline until IRQ transfers it to ktimers"
        );
        drop(owner);
        drop(irq);
        park.cancel().unwrap();
    }

    #[test]
    fn pi_wait_preparation_uses_one_owner_transaction() {
        use super::pi::{PiParkAttempt, prepare_pi_park_attempt};
        use crate::{PiMutexAcquire, PiMutexCore};

        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let owner = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        system.make_ready(owner.id()).unwrap();
        system.enqueue(cpu.as_mut(), owner.id()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let lock = PiMutexCore::new();
        assert_eq!(
            // SAFETY: this facade test explicitly installs a non-current
            // modeled owner before registering the current waiter.
            unsafe { lock.try_acquire_for_thread(owner.id()) }.unwrap(),
            PiMutexAcquire::Acquired
        );
        let PiMutexLockResult::Waiting(token) = system
            .pi_mutex_lock_slow(
                lock.mutex_ref().unwrap(),
                running.id(),
                running.id().as_u64(),
            )
            .unwrap()
        else {
            panic!("the current thread must enter the PI wait slow path")
        };
        test_runtime::reset_cpu_handle_reads();

        let PiParkAttempt::Prepared(mut ticket) = prepare_pi_park_attempt(&system, &token).unwrap()
        else {
            panic!("an unselected PI waiter must prepare one park transaction")
        };

        let owner_claims = test_runtime::cpu_owner_claims();
        cancel_current_park(&mut ticket).unwrap();
        system.pi_wait_cancel(token).unwrap();
        assert_eq!(
            owner_claims, 1,
            "PI current validation, policy drain, and park publication must share one owner \
             transaction"
        );
    }

    #[test]
    fn runtime_owner_handle_preserves_mutable_provenance_after_a_shared_query() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        let current = runtime_current_cpu().unwrap();
        assert_eq!(current.owner(), CpuId::new(0));
        drop(current);

        assert!(matches!(
            schedule_current_cpu().unwrap(),
            SchedulerOutcome::Quiescent
        ));
    }

    #[test]
    fn idle_wait_clears_polling_before_the_runtime_sleep_commit() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let idle = system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, crate::FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let ParkPrepare::Prepared(mut ticket) = system.prepare_park(cpu.as_mut()).unwrap() else {
            panic!("the bootstrap thread must enter the park transaction")
        };
        let ParkCommit::Blocked(decision) = system.commit_park(cpu.as_mut(), &mut ticket).unwrap()
        else {
            panic!("the isolated fixture cannot race with a notification")
        };
        assert_eq!(decision.next(), idle.id());
        system.complete_context_switch(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::configure_idle_wait(true);

        idle_current_cpu_once().unwrap();

        assert_eq!(
            test_runtime::idle_wait_observation(),
            (1, false),
            "a producer after the final recheck must send a physical wake edge"
        );
        assert!(
            !cpu.is_idle_polling(),
            "returning from the virtual wait must clear polling publication"
        );
        assert!(
            cpu.needs_reschedule(),
            "work published in the final recheck window must remain sticky"
        );

        idle_current_cpu_once().unwrap();
        assert_eq!(
            test_runtime::idle_wait_observation().0,
            1,
            "pending work must prevent a second idle wait"
        );
    }

    #[test]
    fn runtime_hooks_read_current_publications_without_reentering_the_cpu_owner() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let bootstrap = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let mut irq = RuntimeIrqGuard::enter();
        let mut owner = runtime_current_cpu_mut(&mut irq).unwrap();
        let owner_pin = owner.as_mut();

        assert_eq!(
            current_thread_handle().unwrap().id(),
            bootstrap.id(),
            "a current-context publication must remain readable while the runqueue owner is \
             borrowed"
        );

        test_runtime::reenter_current_thread_from_next_hook();
        let _now = task_runtime::monotonic_now();
        assert_eq!(
            test_runtime::take_hook_reentry_error(),
            None,
            "current identity is a lock-free remote publication, not owner-only state"
        );

        test_runtime::reenter_needs_reschedule_from_next_hook();
        let update = crate::runtime::SchedulerDeadlineUpdate::try_new(
            1,
            crate::runtime::MonotonicDeadline::from_nanos(1),
        )
        .unwrap();
        task_runtime::publish_scheduler_deadline(update);
        assert_eq!(
            test_runtime::take_hook_reentry_error(),
            None,
            "the sticky reschedule word is independent from mutable owner state"
        );

        test_runtime::reenter_current_thread_from_next_hook();
        let _status = task_runtime::notify_scheduler_cpu(RuntimeCpuId::new(0));
        assert_eq!(
            test_runtime::take_hook_reentry_error(),
            None,
            "runtime callbacks may inspect the generation-bearing remote identity"
        );
        assert_eq!(owner_pin.as_ref().get_ref().owner(), CpuId::new(0));
    }

    #[test]
    fn pinned_reschedule_query_does_not_resolve_the_current_cpu_as_remote() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::reset_cpu_handle_reads();

        // SAFETY: this single-threaded fixture cannot migrate during the
        // current-CPU reschedule observation.
        assert!(!unsafe { current_needs_reschedule_pinned() }.unwrap());
        assert_eq!(
            test_runtime::cpu_handle_reads(),
            (0, 0),
            "a pinned current-CPU query must not enter the generic remote-CPU lookup"
        );
    }

    #[test]
    fn current_thread_identity_uses_task_current_without_pinning_a_cpu() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let bootstrap = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::reset_cpu_handle_reads();
        test_runtime::reset_preempt_guard_entries();

        assert_eq!(current_thread_id().unwrap(), bootstrap.id());

        assert_eq!(
            test_runtime::current_cpu_remote_handle_reads(),
            0,
            "local current identity must come from the runtime context, not the remote rq endpoint"
        );
        assert_eq!(
            test_runtime::cpu_owner_claims(),
            0,
            "a read-only current identity must not enter the mutable CPU owner gate"
        );
        assert_eq!(
            test_runtime::preempt_guard_entries(),
            0,
            "task identity must remain stable across migration without a CPU pin"
        );
    }

    #[test]
    fn public_wake_owns_one_preemption_lifetime() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::reset_preempt_guard_entries();

        assert_eq!(running.wake_handle().wake(), crate::WakeResult::Notified);

        assert_eq!(
            test_runtime::preempt_guard_entries(),
            1,
            "one wake transaction must own one migration-prevention lifetime"
        );
    }

    #[test]
    fn uncontended_pi_mutex_does_not_enter_a_cpu_preemption_scope() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let mutex = crate::Mutex::new(());
        test_runtime::reset_preempt_guard_entries();

        drop(mutex.lock());

        assert_eq!(
            test_runtime::preempt_guard_entries(),
            0,
            "Linux-style PI owner acquisition must use task identity without pinning a CPU"
        );
    }

    #[test]
    fn current_thread_handle_uses_task_current_without_pinning_a_cpu() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let bootstrap = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::reset_cpu_handle_reads();
        test_runtime::reset_preempt_guard_entries();

        assert_eq!(current_thread_handle().unwrap().id(), bootstrap.id());

        assert_eq!(
            test_runtime::current_cpu_remote_handle_reads(),
            0,
            "a current handle must come from its runtime context, not the remote rq endpoint"
        );
        assert_eq!(
            test_runtime::cpu_owner_claims(),
            0,
            "a read-only current handle must not enter the mutable CPU owner gate"
        );
        assert_eq!(
            test_runtime::preempt_guard_entries(),
            0,
            "the scheduler retains the current task owner across migration"
        );
    }

    #[test]
    fn current_reschedule_query_uses_one_migration_pin_without_claiming_the_cpu_owner() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::reset_cpu_handle_reads();
        test_runtime::reset_preempt_guard_entries();

        assert!(!current_cpu_needs_resched().unwrap());

        assert_eq!(
            test_runtime::cpu_owner_claims(),
            0,
            "a read-only reschedule query must not enter the mutable CPU owner gate"
        );
        assert_eq!(
            test_runtime::preempt_guard_entries(),
            1,
            "an unpinned reschedule query must use one migration pin"
        );
    }

    #[test]
    fn irq_pin_captures_runtime_cpu_handles_once() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::reset_cpu_handle_reads();

        let mut irq = RuntimeIrqGuard::enter();
        {
            let owner = runtime_current_cpu_mut(&mut irq).unwrap();
            assert_eq!(owner.owner(), CpuId::new(0));
        }
        {
            let owner = runtime_current_cpu_mut(&mut irq).unwrap();
            assert_eq!(owner.owner(), CpuId::new(0));
        }

        assert_eq!(
            test_runtime::cpu_handle_reads(),
            (1, 0),
            "one migration pin must capture its owner snapshot once without a generic remote \
             lookup"
        );
    }

    #[test]
    fn online_owner_operations_require_an_outer_cpu_pin() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let affinity = CpuSet::all(1);

        // Model an interrupt landing when the innermost scheduler lock restores
        // IRQs. A safe public API must reject this unpinned owner access before
        // that exit can recursively enter the scheduler over a live CpuLocal
        // borrow.
        test_runtime::configure_irq_exit_schedule_reentry(1);
        let result = system.set_current_affinity(cpu.as_mut(), affinity.clone());
        test_runtime::configure_irq_exit_schedule_reentry(0);

        assert_eq!(result, Err(TaskError::UnsafeContext));
        let _irq = RuntimeIrqGuard::enter();
        assert!(!system.set_current_affinity(cpu.as_mut(), affinity).unwrap());
    }

    struct InstalledTaskHandles;

    impl InstalledTaskHandles {
        fn new(system: Pin<&TaskSystem>, cpu: Pin<&mut CpuLocal>) -> Self {
            test_runtime::install_task_handles(
                (system.get_ref() as *const TaskSystem).expose_provenance(),
                // SAFETY: the fixture publishes this pointer only while the
                // owner CPU object is pinned and scheduler access is serialized.
                (unsafe { Pin::get_unchecked_mut(cpu) } as *mut CpuLocal).expose_provenance(),
            );
            Self
        }
    }

    impl Drop for InstalledTaskHandles {
        fn drop(&mut self) {
            test_runtime::clear_task_handles();
        }
    }
}
