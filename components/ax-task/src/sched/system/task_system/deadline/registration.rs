//! Registration under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    pub(super) fn prepare_owner_deadline_timer(
        queue: &TaskDeadlineQueue,
        node: &TaskDeadlineNode,
        registration: Option<&TaskDeadlineRegistration>,
        deadline: Option<MonotonicDeadline>,
        kind: TaskDeadlineKind,
    ) -> Result<OwnerDeadlineTimerPlan, TaskError> {
        if registration.is_some_and(|registration| {
            Some(registration.deadline()) == deadline && registration.kind() == kind
        }) {
            return Ok(OwnerDeadlineTimerPlan::Unchanged);
        }
        let Some(deadline) = deadline else {
            return Ok(if registration.is_some() {
                OwnerDeadlineTimerPlan::Cancel
            } else {
                OwnerDeadlineTimerPlan::Unchanged
            });
        };
        queue
            .prepare_arm(node, deadline, kind)
            .map(OwnerDeadlineTimerPlan::Arm)
            .map_err(Self::task_deadline_error)
    }

    pub(super) fn commit_owner_deadline_timer(
        queue: &mut TaskDeadlineQueue,
        registration: &mut Option<TaskDeadlineRegistration>,
        plan: OwnerDeadlineTimerPlan,
    ) {
        match plan {
            OwnerDeadlineTimerPlan::Unchanged => {}
            OwnerDeadlineTimerPlan::Cancel => {
                if let Some(previous) = registration.take() {
                    // Expiration may already have moved the entry into the
                    // safe-point buffer. The registration is terminal either
                    // way; a later token makes the buffered copy stale.
                    let _removed = queue.cancel(&previous);
                }
            }
            OwnerDeadlineTimerPlan::Arm(plan) => {
                *registration = Some(queue.commit_arm(plan));
            }
        }
    }

    pub(in crate::sched::system::task_system) fn refresh_owner_deadline_timers_locked(
        &self,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        mut cpu: Pin<&mut CpuLocal>,
    ) {
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin(self, &remote);
        let scheduler_now_ns = transaction.clock().wall().as_nanos();
        let enqueue = self.refresh_owner_deadline_timers_in_rq(
            core,
            sched,
            cpu.as_mut(),
            scheduler_now_ns,
            &mut transaction,
        );
        transaction.commit();
        if let Some(preempts_current) = enqueue {
            self.finish_owner_enqueue(
                cpu,
                EnqueueReason::Replenished,
                preempts_current.then_some(RescheduleKind::Immediate),
                false,
                None,
                None,
            );
        }
    }

    pub(in crate::sched::system::task_system) fn refresh_owner_deadline_timers_in_rq(
        &self,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        mut cpu: Pin<&mut CpuLocal>,
        scheduler_now_ns: u64,
        run_queue: &mut OwnerRqTxn<'_>,
    ) -> Option<bool> {
        let owner = cpu.owner();
        if sched.deadline.bandwidth.reservation_owner() != Some(owner)
            && sched.deadline.cbs_timer.is_none()
            && sched.deadline.zero_lag_timer.is_none()
        {
            // Linux keeps CBS and inactive timers in the Deadline class. A
            // non-DL schedule-out or enqueue has no Deadline timer state to
            // refresh; retained registrations still pass through so their
            // old owner can cancel them before releasing rq ownership.
            return None;
        }
        let monotonic_now = task_runtime::monotonic_now();
        let mut owner_enqueue = None;

        loop {
            let owns_bandwidth = sched.deadline.bandwidth.reservation_owner() == Some(owner);
            let owner_entity = if let Some(active) = core.sched().active_option(sched) {
                active.base_entity().clone()
            } else {
                run_queue
                    .base_scheduling_entity(core.id())
                    .unwrap_or_else(|| {
                        task_runtime::fatal_invariant(0x444c_1101, core.id().as_u64() as usize)
                    })
            };
            let cbs_scheduler_deadline = owns_bandwidth
                .then_some(())
                .and(owner_entity.deadline())
                .and_then(DeadlineEntity::next_scheduler_event_ns);
            let zero_lag_scheduler_deadline = owns_bandwidth
                .then(|| sched.deadline.bandwidth.zero_lag())
                .flatten()
                .map(SchedulerTimestamp::as_nanos);
            let cbs_event = cbs_scheduler_deadline
                .map(|deadline| scheduler_clock_event(scheduler_now_ns, monotonic_now, deadline));
            let zero_lag_event = zero_lag_scheduler_deadline
                .map(|deadline| scheduler_clock_event(scheduler_now_ns, monotonic_now, deadline));
            let cbs_due = matches!(cbs_event, Some(SchedulerClockEvent::Due));
            let zero_lag_due = matches!(zero_lag_event, Some(SchedulerClockEvent::Due));
            let cbs_deadline = match cbs_event {
                Some(SchedulerClockEvent::Future(deadline)) => Some(deadline),
                Some(SchedulerClockEvent::Due) | None => None,
            };
            let zero_lag_deadline = match zero_lag_event {
                Some(SchedulerClockEvent::Future(deadline)) => Some(deadline),
                Some(SchedulerClockEvent::Due) | None => None,
            };
            {
                let mut deadline_base = cpu
                    .remote()
                    .lock_deadline_activity(DeadlineBaseGuardSource::Registration);
                let cbs_plan = Self::prepare_owner_deadline_timer(
                    &deadline_base.queue,
                    core.deadline_cbs_timer(),
                    sched.deadline.cbs_timer.as_ref(),
                    cbs_deadline,
                    TaskDeadlineKind::DeadlineCbs,
                )
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x444c_0006, core.id().as_u64() as usize)
                });
                let zero_lag_plan = Self::prepare_owner_deadline_timer(
                    &deadline_base.queue,
                    core.deadline_zero_lag_timer(),
                    sched.deadline.zero_lag_timer.as_ref(),
                    zero_lag_deadline,
                    TaskDeadlineKind::DeadlineZeroLag,
                )
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x444c_0007, core.id().as_u64() as usize)
                });
                Self::commit_owner_deadline_timer(
                    &mut deadline_base.queue,
                    &mut sched.deadline.cbs_timer,
                    cbs_plan,
                );
                Self::commit_owner_deadline_timer(
                    &mut deadline_base.queue,
                    &mut sched.deadline.zero_lag_timer,
                    zero_lag_plan,
                );
            }

            if !cbs_due && !zero_lag_due {
                return owner_enqueue;
            }
            let reconcile = OwnerDeadlineReconcile {
                core,
                sched,
                cpu: cpu.as_mut(),
                due: OwnerDeadlineDue {
                    scheduler_now_ns,
                    cbs_expired: cbs_due,
                    zero_lag_reached: zero_lag_due,
                },
            };
            if let Some(preempts_current) =
                self.reconcile_due_owner_deadline_locked(reconcile, run_queue)
            {
                owner_enqueue = Some(owner_enqueue.unwrap_or(false) || preempts_current);
            }
        }
    }

    pub(super) fn reconcile_due_owner_deadline_locked(
        &self,
        reconcile: OwnerDeadlineReconcile<'_>,
        run_queue: &mut OwnerRqTxn<'_>,
    ) -> Option<bool> {
        let OwnerDeadlineReconcile {
            core,
            sched,
            cpu,
            due,
        } = reconcile;
        let mut replenish = false;

        if due.cbs_expired {
            let rq_throttled = run_queue.is_deadline_throttled_member(core.id());
            let base_entity = if let Some(active) = core.sched().active_option(sched) {
                active.base_entity().clone()
            } else {
                run_queue
                    .base_scheduling_entity(core.id())
                    .unwrap_or_else(|| {
                        task_runtime::fatal_invariant(0x444c_1102, core.id().as_u64() as usize)
                    })
            };
            let Some(deadline) = base_entity.deadline() else {
                task_runtime::fatal_invariant(0x444c_1103, core.id().as_u64() as usize);
            };
            let replenish_due = deadline.is_throttled()
                && deadline
                    .next_scheduler_event_ns()
                    .is_some_and(|event| scheduler_time_reached(due.scheduler_now_ns, event));
            if replenish_due {
                deadline.replenish(due.scheduler_now_ns);
                let updated = SchedulingEntity::Deadline(deadline.clone());
                if let Some(mut active) = core.sched().active_option(sched) {
                    active.replace_base_entity(updated);
                } else {
                    let updated_in_rq = if rq_throttled && !deadline.is_throttled() {
                        replenish = true;
                        run_queue
                            .replenish_throttled_deadline(core.id(), updated.clone())
                            .is_ok()
                    } else {
                        run_queue.update_base_deadline_entity(core.id(), updated.clone())
                    };
                    if !updated_in_rq {
                        task_runtime::fatal_invariant(0x444c_1104, core.id().as_u64() as usize);
                    }
                    if core.effective_policy_snapshot() == sched.policy.base {
                        core.publish_effective_schedule(sched.policy.base, &updated);
                    }
                }
            }
        }

        if due.zero_lag_reached
            && sched.deadline.bandwidth.zero_lag().is_some_and(|zero_lag| {
                zero_lag.is_reached_by(SchedulerTimestamp::from_nanos(due.scheduler_now_ns))
            })
        {
            run_queue.deactivate_deadline_bandwidth(sched.deadline.bandwidth.reservation_scaled());
            sched.deadline.bandwidth.deactivate();
        }

        if replenish {
            if sched.lifecycle.state() != ThreadState::Running
                || sched.placement.queued_cpu() != Some(cpu.owner())
                || sched.placement.on_cpu().is_some()
            {
                task_runtime::fatal_invariant(0x444c_1108, core.id().as_u64() as usize);
            }
            let entity = run_queue.scheduling_entity(core.id()).unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x444c_1105, core.id().as_u64() as usize)
            });
            let preempts_current = run_queue
                .wakeup_preempt(core.id(), sched.policy.base, &entity, 0)
                .requests_reschedule();
            core.set_wake_cpu_hint(cpu.owner());
            return Some(preempts_current);
        }
        None
    }

    pub(in crate::sched::system::task_system) fn cancel_owner_deadline_timers_locked(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        remote: &CpuRemote,
    ) {
        let mut deadline_base =
            remote.lock_deadline_activity(DeadlineBaseGuardSource::Registration);
        let cbs_plan = Self::prepare_owner_deadline_timer(
            &deadline_base.queue,
            core.deadline_cbs_timer(),
            sched.deadline.cbs_timer.as_ref(),
            None,
            TaskDeadlineKind::DeadlineCbs,
        )
        .unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x444c_0008, core.id().as_u64() as usize)
        });
        let zero_lag_plan = Self::prepare_owner_deadline_timer(
            &deadline_base.queue,
            core.deadline_zero_lag_timer(),
            sched.deadline.zero_lag_timer.as_ref(),
            None,
            TaskDeadlineKind::DeadlineZeroLag,
        )
        .unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x444c_0009, core.id().as_u64() as usize)
        });
        Self::commit_owner_deadline_timer(
            &mut deadline_base.queue,
            &mut sched.deadline.cbs_timer,
            cbs_plan,
        );
        Self::commit_owner_deadline_timer(
            &mut deadline_base.queue,
            &mut sched.deadline.zero_lag_timer,
            zero_lag_plan,
        );
    }

    pub(super) fn registration_matches(
        registration: &TaskDeadlineRegistration,
        event: ExpiredTaskDeadline,
    ) -> bool {
        event.thread() == Some(registration.thread())
            && event.token() == registration.token()
            && event.deadline() == Some(registration.deadline())
            && event.kind() == Some(registration.kind())
    }

    pub(super) fn take_expired_registration(
        registration: &mut Option<TaskDeadlineRegistration>,
        event: ExpiredTaskDeadline,
    ) -> bool {
        if registration
            .as_ref()
            .is_some_and(|registration| Self::registration_matches(registration, event))
        {
            registration.take();
            true
        } else {
            false
        }
    }
}
