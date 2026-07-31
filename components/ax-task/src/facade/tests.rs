#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{
        CpuId, SchedulePolicy, SwitchReason, ThreadExtension, ThreadExtensionOps, ThreadSpec,
        inbox::{InboxKind, InboxMessage, InboxNode, PublishResult},
        runtime::AddressSpaceHandle,
        test_runtime,
    };

    static PARKING_EXIT_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
    static REENTRANT_EXIT_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
    static REENTRANT_EXIT_CALLBACKS_IN_IRQ_EXIT: AtomicUsize = AtomicUsize::new(0);

    fn owner_snapshot(system: &TaskSystem, cpu: Pin<&CpuLocal>) -> crate::CpuSnapshot {
        let _irq = RuntimeIrqGuard::enter();
        system.snapshot(cpu).unwrap()
    }

    fn publish_unrelated_expired_deadline(
        system: &TaskSystem,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> ThreadHandle {
        let unrelated = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let _registration = cpu
            .as_mut()
            .task_deadlines()
            .arm(
                unrelated.sleep_timer(),
                now_ns,
                TaskDeadlineKind::park_timeout(0),
            )
            .unwrap();
        assert_eq!(on_clock_event(now_ns, 1).unwrap().expired(), 1);
        unrelated
    }

    static ORDERING_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: assert_address_space_installed,
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
    fn kernel_address_space_is_explicitly_installed_before_switch_in() {
        test_runtime::reset_installed_address_space();
        let extension = unsafe {
            // SAFETY: the callback table interprets data only as the expected
            // address-space scalar and owns no external resource.
            ThreadExtension::new(0, &ORDERING_EXTENSION_OPS)
        };

        prepare_next_context(
            AddressSpaceHandle::NONE,
            ThreadId::from_parts(1, 1),
            SchedulePolicy::default(),
            Some(extension.as_view()),
        );

        assert_eq!(test_runtime::installed_address_space(), Some(0));
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
        arm_current_park_deadline(&running, &mut ticket, 0).unwrap();

        assert_eq!(on_clock_event(0, 64).unwrap().expired(), 1);
        let mut irq = RuntimeIrqGuard::enter();
        assert_eq!(
            drain_current_expired_timers(system.as_ref().get_ref(), &mut irq).unwrap(),
            1
        );
        drop(irq);
        {
            let _irq = RuntimeIrqGuard::enter();
            assert_eq!(
                system
                    .drain_remote_wakes(cpu.as_mut(), 0)
                    .unwrap()
                    .drained(),
                1
            );
        }
        assert_eq!(
            system.thread_state(running.id()).unwrap(),
            crate::ThreadState::Parking,
            "timer wake must leave the owner thread to finish its PARKING handshake"
        );
        assert_eq!(owner_snapshot(&system, cpu.as_ref()).runnable(), 0);
        cpu.request_reschedule();
        let mut irq_return_passes = 0;
        while current_cpu_needs_resched().unwrap() {
            assert!(
                schedule_current_cpu().unwrap().parking_deferred(),
                "timer IRQ-return scheduling must defer until the park token commits"
            );
            irq_return_passes += 1;
            assert!(irq_return_passes < 2, "PARKING must not spin at IRQ return");
        }
        assert_eq!(irq_return_passes, 1);
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
        assert_eq!(owner_snapshot(&system, cpu.as_ref()).runnable(), 0);
        assert!(owner_snapshot(&system, cpu.as_ref()).need_resched());
        assert!(!cancel_current_park_deadline(&running, &mut ticket).unwrap());
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

        {
            let mut irq = RuntimeIrqGuard::enter();
            assert_eq!(
                service_scheduler_safe_point_deadlines(system.as_ref().get_ref(), &mut irq)
                    .unwrap(),
                0
            );
        }
        assert_eq!(
            cpu.deadline_expire_passes_for_test(),
            0,
            "an idle scheduler safe point must not enter the timer expiry engine"
        );
    }

    #[test]
    fn scheduler_safe_point_reuses_its_idle_clock_snapshot() {
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
            1,
            "an idle deadline pass and its scheduler decision must share one clock sample"
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
            cpu.as_mut().next_task_deadline_update(0, 1).unwrap()
        };
        assert_eq!(
            task_runtime::publish_task_deadline(initial),
            RuntimeStatus::Success
        );
        assert!(test_runtime::take_task_deadline_update().is_some());

        yield_current_cpu().unwrap();

        assert!(
            test_runtime::take_task_deadline_update().is_none(),
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
        assert_eq!(take_current_expired_task_deadlines(&mut expired).unwrap(), 1);
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

        commit_current_park(&mut ticket).unwrap();

        assert_eq!(
            test_runtime::monotonic_reads(),
            1,
            "park commit must use one owner-transition clock sample"
        );
        let mut expired = [ExpiredTaskDeadline::EMPTY; 1];
        assert_eq!(take_current_expired_task_deadlines(&mut expired).unwrap(), 1);
        assert_eq!(
            expired[0].thread(),
            Some(unrelated.id()),
            "park commit must not consume another thread's deadline work"
        );
    }

    #[test]
    fn saturated_park_deadline_remains_notification_only() {
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

        arm_current_park_deadline(&running, &mut ticket, u64::MAX).unwrap();

        assert!(
            !ticket.has_deadline(),
            "the no-deadline sentinel must not own a queue registration"
        );
        assert!(cpu.as_mut().task_deadlines().is_empty());
        cancel_current_park(&mut ticket).unwrap();
    }

    #[test]
    fn scheduler_safe_point_recovers_overdue_deadline_without_clock_irq() {
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
        arm_current_park_deadline(&running, &mut ticket, 10).unwrap();

        test_runtime::set_monotonic_ns(10);
        assert!(
            schedule_current_cpu().unwrap().parking_deferred(),
            "the owner must retain its PARKING handshake at the recovery safe point"
        );
        assert!(
            running.core.take_park_notification(),
            "a scheduler safe point must recover a due task deadline even when no physical \
             clockevent IRQ was observed"
        );
        assert!(
            !cancel_current_park_deadline(&running, &mut ticket).unwrap(),
            "safe-point recovery must physically consume the expired deadline entry"
        );
        cancel_current_park(&mut ticket).unwrap();
    }

    #[test]
    fn clock_event_publishes_owner_reschedule_before_returning() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let policy =
            SchedulePolicy::round_robin_with_quantum(crate::RtPriority::new(1).unwrap(), 10)
                .unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(policy))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        assert!(!current_cpu_needs_resched().unwrap());
        let outcome = on_clock_event(10, 64).unwrap();

        assert!(outcome.slice_expired());
        assert_eq!(outcome.expired(), 0);
        assert!(outcome.pending());
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
        let _first_registration = cpu
            .as_mut()
            .task_deadlines()
            .arm(first.sleep_timer(), 10, TaskDeadlineKind::park_timeout(0))
            .unwrap();
        let _second_registration = cpu
            .as_mut()
            .task_deadlines()
            .arm(second.sleep_timer(), 10, TaskDeadlineKind::park_timeout(0))
            .unwrap();

        let outcome = on_clock_event(10, 1).unwrap();

        assert_eq!(outcome.expired(), 1);
        assert!(outcome.pending());
        assert!(outcome.update().deferred_work());
        assert!(
            outcome
                .update()
                .deadline()
                .is_none_or(|deadline| deadline.as_nanos() > 11),
            "an expired bounded backlog must be advanced by sticky task work, not by a timer \
             interrupt rearmed at the 1ns hardware resolution"
        );
        assert!(
            owner_snapshot(&system, cpu.as_ref()).need_resched(),
            "the deferred owner pass must remain a scheduler-visible safe point"
        );
    }

    #[test]
    fn park_deadline_owner_mismatch_preserves_ticket_for_retry() {
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
        arm_current_park_deadline(&running, &mut ticket, 10).unwrap();
        let token = ticket
            .deadline()
            .expect("armed park deadline token")
            .token();

        running
            .core
            .register_sleep_timer(CpuId::new(1), token.generation());
        assert_eq!(
            cancel_current_park_deadline(&running, &mut ticket),
            Err(TaskError::CpuOwnerMismatch {
                expected: 1,
                actual: 0,
            })
        );
        assert!(
            ticket.has_deadline(),
            "a retryable owner mismatch must not consume the move-only deadline token"
        );
        assert_eq!(
            cpu.as_mut().task_deadlines().next_deadline_ns(0, 1),
            Some(10),
            "a failed cancellation must leave the physical queue entry intact"
        );

        running
            .core
            .register_sleep_timer(CpuId::new(0), token.generation());
        assert!(cancel_current_park_deadline(&running, &mut ticket).unwrap());
        assert!(!ticket.has_deadline());
        assert_eq!(cpu.as_mut().task_deadlines().next_deadline_ns(0, 1), None);
        cancel_current_park(&mut ticket).unwrap();
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
        arm_current_park_deadline(&running, &mut first, 10).unwrap();
        assert_eq!(on_clock_event(10, 1).unwrap().expired(), 1);
        assert!(!cancel_current_park_deadline(&running, &mut first).unwrap());
        cancel_current_park(&mut first).unwrap();

        let second_permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut second) = prepare_current_park(&second_permit).unwrap()
        else {
            panic!("the next park generation must be independently prepared");
        };
        let _ = second_permit;
        arm_current_park_deadline(&running, &mut second, 100).unwrap();

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
        cpu.as_mut().set_task_deadline_generation_for_test(u64::MAX);

        assert_eq!(
            arm_current_park_deadline(&running, &mut ticket, 10),
            Err(TaskError::InvalidConfiguration)
        );
        assert!(!ticket.has_deadline());
        assert_eq!(running.core.sleep_timer_cpu(), None);
        assert_eq!(cpu.as_mut().task_deadlines().next_deadline_ns(0, 1), None);

        cancel_current_park(&mut ticket).unwrap();
    }

    #[test]
    fn failed_cancel_update_consumes_the_physically_removed_deadline_ticket() {
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
        arm_current_park_deadline(&running, &mut ticket, 10).unwrap();
        cpu.as_mut().set_task_deadline_generation_for_test(u64::MAX);

        assert_eq!(
            cancel_current_park_deadline(&running, &mut ticket),
            Err(TaskError::InvalidConfiguration)
        );
        assert!(
            !ticket.has_deadline(),
            "a physically removed timer must not remain represented by a live ticket"
        );
        assert_eq!(running.core.sleep_timer_cpu(), None);
        assert_eq!(cpu.as_mut().task_deadlines().next_deadline_ns(0, 1), None);

        cancel_current_park(&mut ticket).unwrap();
    }

    #[test]
    fn parking_safe_point_is_bounded_and_does_not_run_task_work() {
        PARKING_EXIT_CALLBACKS.store(0, Ordering::Release);
        let remote_wake_nodes = [
            Box::pin(InboxNode::new(InboxKind::RemoteWake)),
            Box::pin(InboxNode::new(InboxKind::RemoteWake)),
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
        system.enqueue(cpu.as_mut(), exited.id(), 0).unwrap();
        assert_eq!(
            system.schedule(cpu.as_mut(), 0).unwrap().next(),
            exited.id()
        );
        system.complete_context_switch(cpu.as_mut()).unwrap();
        let exit_decision = system.exit_current(cpu.as_mut(), 0).unwrap();
        assert_ne!(exit_decision.next(), exited.id());
        assert_eq!(PARKING_EXIT_CALLBACKS.load(Ordering::Acquire), 0);

        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let permit = acquire_blocking_permit().unwrap();
        let ParkPrepare::Prepared(mut ticket) = prepare_current_park(&permit).unwrap() else {
            panic!("fresh park must publish PARKING");
        };
        let _ = permit;

        for (index, node) in remote_wake_nodes.iter().enumerate() {
            let slot = (index + 1) as u32;
            let message = InboxMessage::remote_wake(ThreadId::from_parts(slot, 1), CpuId::new(0));
            let node = unsafe {
                // The pinned fixture is declared before the task system, so it
                // outlives the CPU inbox even when one bounded batch remains.
                Pin::new_unchecked(&*(node.as_ref().get_ref() as *const InboxNode))
            };
            assert_eq!(
                cpu.remote().publish_remote_wake(node, message),
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
        system.enqueue(cpu.as_mut(), exiting.id(), 0).unwrap();
        assert_eq!(
            system.schedule(cpu.as_mut(), 0).unwrap().next(),
            exiting.id()
        );
        system.complete_context_switch(cpu.as_mut()).unwrap();
        assert_eq!(
            system.exit_current(cpu.as_mut(), 0).unwrap().next(),
            bootstrap.id()
        );
        system.complete_context_switch(cpu.as_mut()).unwrap();

        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        test_runtime::configure_irq_exit_schedule_reentry(1);
        assert!(matches!(
            schedule_current_cpu().unwrap(),
            SchedulerOutcome::Quiescent
        ));

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
    fn scheduler_safe_point_drains_owner_work_after_resched_bit_was_consumed() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        let running = system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());

        assert_eq!(running.wake_handle().wake(), crate::WakeResult::Notified);
        assert!(cpu.has_remote_work());
        assert!(cpu.needs_reschedule());

        // Forced schedule paths used to consume the sticky bit without first
        // draining owner work. Claiming scheduler entry must re-observe the
        // published inbox and preserve a doorbell for the next bounded drain.
        cpu.as_mut().scheduler_enter();
        assert!(cpu.needs_reschedule());

        assert!(matches!(
            schedule_current_cpu().unwrap(),
            SchedulerOutcome::Quiescent
        ));
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
                AddressSpaceHandle::NONE,
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
                AddressSpaceHandle::NONE,
            )
        };
        let next = system
            .create_thread(unsafe {
                ThreadSpec::new(SchedulePolicy::fifo(crate::RtPriority::new(1).unwrap()))
                    .with_resources(next_resources)
            })
            .unwrap();
        system.make_ready(next.id()).unwrap();
        system.enqueue(cpu.as_mut(), next.id(), 0).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let _context_switch = test_runtime::allow_context_switch();
        test_runtime::reset_scheduler_frame_state();
        test_runtime::reset_cpu_handle_reads();

        let decision = schedule_current_cpu().unwrap().decision().unwrap();

        assert!(decision.requires_context_switch());
        assert_eq!(
            test_runtime::cpu_handle_reads(),
            (2, 2),
            "scheduler entry must capture once and switch return must refresh once"
        );
        assert_eq!(
            test_runtime::scheduler_frame_state(),
            (0, 1, 0),
            "one scheduling operation must use exactly one scheduler baton without nested IRQ guards"
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
                AddressSpaceHandle::NONE,
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
                AddressSpaceHandle::NONE,
            )
        };
        let next = system
            .create_thread(unsafe {
                ThreadSpec::new(SchedulePolicy::fifo(crate::RtPriority::new(1).unwrap()))
                    .with_resources(next_resources)
            })
            .unwrap();
        system.make_ready(next.id()).unwrap();
        system.enqueue(cpu.as_mut(), next.id(), 0).unwrap();
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

    unsafe extern "Rust" fn assert_address_space_installed(
        data: usize,
        _thread: ThreadId,
        _policy: SchedulePolicy,
    ) {
        assert_eq!(test_runtime::installed_address_space(), Some(data));
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
                AddressSpaceHandle::NONE,
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
                AddressSpaceHandle::NONE,
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
                AddressSpaceHandle::NONE,
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
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let unrelated =
            publish_unrelated_expired_deadline(system.as_ref().get_ref(), cpu.as_mut(), 10);
        test_runtime::set_monotonic_ns(10);
        let permit = prepare_current_exit().unwrap();
        let _context_switch = test_runtime::allow_context_switch();
        test_runtime::reset_monotonic_reads();

        let exit = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            commit_current_exit(permit)
        }));

        assert!(
            exit.is_err(),
            "the unit runtime must reject returning to an exited context"
        );
        assert_eq!(
            test_runtime::monotonic_reads(),
            2,
            "exit commit needs one transition timestamp and one completion-time clockevent recheck"
        );
        let mut expired = [ExpiredTaskDeadline::EMPTY; 1];
        assert_eq!(take_current_expired_task_deadlines(&mut expired).unwrap(), 1);
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
        assert_eq!(
            system.block_current(cpu.as_mut(), 0).unwrap().next(),
            idle.id()
        );
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
    fn runtime_hooks_reject_reentrant_cpu_owner_queries() {
        let system = Box::pin(TaskSystem::new(crate::TaskSystemConfig::new(1)).unwrap());
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let _runtime_handles = InstalledTaskHandles::new(system.as_ref(), cpu.as_mut());
        let mut irq = RuntimeIrqGuard::enter();
        let mut owner = runtime_current_cpu_mut(&mut irq).unwrap();
        let owner_pin = owner.as_mut();

        assert_eq!(
            current_thread_handle().unwrap_err(),
            TaskError::CpuOwnerBorrowed,
            "a reentrant current-handle query must fail instead of spinning"
        );

        test_runtime::reenter_current_thread_from_next_hook();
        let _now = task_runtime::monotonic_ns();
        assert_eq!(
            test_runtime::take_hook_reentry_error(),
            Some(TaskError::CpuOwnerBorrowed)
        );

        test_runtime::reenter_needs_reschedule_from_next_hook();
        let update = crate::runtime::TaskDeadlineUpdate::try_new(
            1,
            crate::runtime::MonotonicDeadline::from_nanos(1),
            false,
        )
        .unwrap();
        let _status = task_runtime::publish_task_deadline(update);
        assert_eq!(
            test_runtime::take_hook_reentry_error(),
            Some(TaskError::CpuOwnerBorrowed)
        );

        test_runtime::reenter_current_thread_from_next_hook();
        let _status = task_runtime::send_scheduler_ipi(RuntimeCpuId::new(0));
        assert_eq!(
            test_runtime::take_hook_reentry_error(),
            Some(TaskError::CpuOwnerBorrowed)
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
            (1, 1),
            "one migration pin must validate its CPU-local identity once"
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
