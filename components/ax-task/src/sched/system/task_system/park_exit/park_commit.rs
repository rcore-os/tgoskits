//! Park commit under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Rechecks a prepared park and either cancels it or commits schedule-out.
    pub fn commit_park(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
        token: &mut ParkTicket,
    ) -> Result<ParkCommit, TaskError> {
        self.commit_park_owner(
            cpu,
            current.runtime_core_arc(),
            token,
            OwnerRqEntry::IrqSave,
        )
    }

    /// Commits park while the runtime owns the IRQ-off scheduler baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must remain active until this function returns.
    pub(crate) unsafe fn commit_park_in_scheduler_frame(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &Arc<ThreadCore>,
        token: &mut ParkTicket,
    ) -> Result<ParkCommit, TaskError> {
        self.commit_park_owner(cpu, current, token, OwnerRqEntry::SchedulerFrame)
    }

    pub(super) fn commit_park_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: &Arc<ThreadCore>,
        token: &mut ParkTicket,
        rq_entry: OwnerRqEntry,
    ) -> Result<ParkCommit, TaskError> {
        if token.is_resolved() || current.id() != token.thread() {
            return Err(TaskError::StaleThreadId);
        }
        if rq_entry.requires_owner_context_validation() {
            self.ensure_owner_cpu_context(&cpu)?;
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint for the complete park transaction.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        if let Some(registration) = token.deadline()
            && registration.may_enter_soft_expiry_buffer()
            && let Some(event) = cpu.as_mut().take_buffered_expiration(registration)
        {
            self.service_expired_park_deadline(event)?;
        }
        let initial_request = remote.claim_scheduler_request(SchedulerRequestScope::All);
        self.drain_owner_work(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;

        if matches!(
            current.effective_policy_snapshot(),
            SchedulePolicy::Fair { .. }
                | SchedulePolicy::Fifo { .. }
                | SchedulePolicy::RoundRobin { .. }
        ) && !current.sched().placement().has_pending_migration()
            && let Some(commit) = self.try_commit_park_in_rq(
                cpu.as_mut(),
                token,
                remote,
                current,
                initial_request,
                rq_entry,
            )?
        {
            return Ok(commit);
        }

        // SAFETY: propagated from the selected entry contract.
        let mut previous_sched = unsafe { rq_entry.lock_thread_sched(current.sched()) };
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, remote) };

        transaction.adopt_scheduler_request(initial_request);
        let scheduler_request = transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let now_ns = transaction.clock().wall().as_nanos();

        if transaction.current_thread() != Some(token.thread()) {
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::StaleThreadId);
        }
        let Some(previous_core) = transaction.current_core() else {
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::NoRunnableThread);
        };
        if !Arc::ptr_eq(&previous_core, current) {
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::InvalidConfiguration);
        }
        let generation = previous_core.park_generation();
        if generation != token.generation() {
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::StaleThreadId);
        }
        let notified = previous_core.take_park_notification();
        if notified {
            previous_sched
                .transition(&previous_core, ThreadState::Running)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x504b_1101, previous_core.id().as_u64() as usize)
                });
            cpu.restore_claimed_park_preemption(scheduler_request);
            transaction.commit_and_finish_scheduler_request();
            token.mark_resolved();
            return Ok(ParkCommit::Notified);
        }
        cpu.defer_park_preemption(scheduler_request);
        let dispatch_commit = self.settle_owner_current_dispatch_in_rq(&mut transaction);

        let previous_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_1102, previous_core.id().as_u64() as usize)
        });
        let previous_urgency = transaction.current_scheduling_urgency().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_1102, previous_core.id().as_u64() as usize)
        });
        let resumed = {
            let placement = previous_core.sched().placement();
            let sched = &mut *previous_sched;
            // Lifecycle and wake publication share one atomic word. A wake
            // that observes Parking sets PARK_NOTIFIED in that word; this CAS
            // either consumes it and restores Running or uniquely publishes
            // Blocked before a later waker enters the task-lock activation
            // path.
            if previous_core
                .publish_blocked_from_parking()
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x504b_1103, previous_core.id().as_u64() as usize)
                })
                == ParkPublication::Notified
            {
                true
            } else {
                if sched.lifecycle.state() != ThreadState::Blocked
                    || placement.queued_cpu() != Some(cpu.owner())
                    || placement.on_cpu() != Some(cpu.owner())
                {
                    task_runtime::fatal_invariant(
                        0x504b_1104,
                        previous_core.id().as_u64() as usize,
                    );
                }
                // Timer replacement is the final recoverable preparation.
                // A wake cannot cross this point while the thread lock is
                // held; all following rq and placement changes are one owner
                // commit and cannot return a partial block.

                let force_delayed = false;
                let timing_granularity_ns = self.config.timing_granularity_ns();
                let delayed = !transaction.is_linked_current(previous_core.id())
                    && transaction
                        .delay_dequeue_unlinked_current(
                            previous_core.id(),
                            timing_granularity_ns,
                            force_delayed,
                        )
                        .is_some();
                if delayed {
                    placement.delay_dequeue_current(cpu.owner());
                } else {
                    let active = if transaction.is_linked_current(previous_core.id()) {
                        transaction
                            .deactivate_task(previous_core.id())
                            .into_active()
                    } else {
                        transaction.deactivate_unlinked_current(previous_core.id());
                        transaction
                            .take_current()
                            .and_then(CurrentDispatch::into_active)
                            .unwrap_or_else(|| {
                                task_runtime::fatal_invariant(
                                    0x504b_1105,
                                    previous_core.id().as_u64() as usize,
                                )
                            })
                    };
                    previous_core.sched().install_active(sched, active);
                }
                self.mark_owner_deadline_non_contending_in_rq(
                    &previous_core,
                    sched,
                    cpu.as_mut(),
                    now_ns,
                    &mut transaction,
                );
                if !delayed {
                    let mut active = previous_core.sched().active(sched);
                    if let Some(fair) = active.base_entity().fair() {
                        let virtual_time = transaction.virtual_time();
                        let rq_max_slice_ns = transaction
                            .max_fair_service_request_ns()
                            .unwrap_or(fair.service_request_ns())
                            .max(fair.service_request_ns());
                        active.base_entity_mut().capture_fair_sleep_lag(
                            virtual_time,
                            rq_max_slice_ns,
                            timing_granularity_ns,
                        );
                    }
                }
                if !delayed {
                    placement.block_current(cpu.owner());
                }
                false
            }
        };
        if resumed {
            transaction.commit_and_finish_scheduler_request();
            drop(previous_sched);
            self.finish_owner_dispatch_commit(dispatch_commit);
            cpu.finish_park_preemption(true);
            token.mark_resolved();
            return Ok(ParkCommit::Notified);
        }

        cpu.finish_park_preemption(false);
        transaction.take_current();
        // This branch commits a real switch, so the request generated while
        // settling the outgoing dispatch belongs to this decision. The
        // resumed branch above deliberately leaves it for the next pass.
        transaction.merge_scheduler_request(SchedulerRequestScope::All);

        let next = self.pick_owner_next_in_rq(
            cpu.as_mut(),
            &mut transaction,
            Some((&previous_core, &mut previous_sched)),
        );
        let OwnerNext {
            core: next_core,
            policy: next_policy_ref,
            urgency: next_urgency,
        } = next;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_1107, next_core.as_ref().id().as_u64() as usize)
        });
        let handoff = Self::prepare_switch_handoff(
            Some(token.thread()),
            Some(PreviousSwitchOwnership::retained(previous_core)),
            next_core,
            next_policy_ref,
            PreviousSwitchDisposition::Live,
            None,
        );

        let deadline_rq_observation =
            transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());

        self.commit_owner_switch_selection(
            cpu.as_mut(),
            transaction,
            handoff,
            !dispatch_commit.has_deferred_task_lock_work(),
        );

        drop(previous_sched);
        self.finish_owner_dispatch_commit(dispatch_commit);
        self.finish_owner_selection(
            cpu.as_mut(),
            Some(previous_endpoint.thread()),
            next_endpoint.thread(),
            Some(previous_urgency),
            next_urgency,
            OwnerSchedulerDeadline::Reevaluate(deadline_rq_observation),
        );
        let decision = Self::owner_switch_plan(
            Some(previous_endpoint),
            next_endpoint,
            SwitchReason::Blocked,
            now_ns,
        );

        token.mark_resolved();
        Ok(ParkCommit::Blocked(decision))
    }
}
