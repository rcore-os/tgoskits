//! Park, current-thread exit, and physical switch-tail completion.

use super::*;
use crate::ParkPublication;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RqOnlyParkClass {
    Fair,
    Realtime,
}

fn classify_rq_only_park_class(
    policy: SchedulePolicy,
    linked_current: bool,
    rt_quota_exempt: bool,
) -> Option<RqOnlyParkClass> {
    match policy {
        SchedulePolicy::Fair { .. } if !linked_current => Some(RqOnlyParkClass::Fair),
        SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
            if linked_current && !rt_quota_exempt =>
        {
            Some(RqOnlyParkClass::Realtime)
        }
        _ => None,
    }
}

pub(crate) struct CurrentExitPermit {
    scheduler_exit: OwnedThreadSchedulerExit,
    current_core: Arc<ThreadCore>,
}

impl CurrentExitPermit {
    pub(crate) fn thread(&self) -> ThreadId {
        self.current_core.id()
    }

    fn current_core(&self) -> &Arc<ThreadCore> {
        &self.current_core
    }

    fn seal(&mut self) {
        self.scheduler_exit.seal();
    }
}

impl TaskSystem {
    /// Publishes `PARKING` after consuming a wake-before-park notification.
    pub fn prepare_park(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
    ) -> Result<ParkPrepare, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let core = current.runtime_core_arc();
        let placement = core.sched().placement();
        if placement.queued_cpu() != Some(cpu.owner()) || placement.on_cpu() != Some(cpu.owner()) {
            return Err(TaskError::StaleThreadId);
        }
        self.prepare_current_park(core)
    }

    /// Publishes the current task's wait state before its later schedule pass.
    ///
    /// The runtime's current-thread publication is the architecture-context
    /// identity, like Linux `current`. Resumed and fresh task contexts complete
    /// switch tail before calling task code, so this state publication neither
    /// reclaims `CpuLocal` nor repeats switch-tail completion.
    pub(crate) fn prepare_current_park(
        &self,
        current: &ThreadCore,
    ) -> Result<ParkPrepare, TaskError> {
        let core = current;
        let placement = core.sched().placement();
        let queued_cpu = placement.queued_cpu();
        if core.state() != ThreadState::Running
            || queued_cpu.is_none()
            || placement.on_cpu() != queued_cpu
        {
            return Err(TaskError::StaleThreadId);
        }
        if core.take_park_notification() {
            return Ok(ParkPrepare::Notified);
        }
        let generation = core.next_park_generation()?;
        core.transition_state(ThreadState::Parking)?;
        Ok(ParkPrepare::Prepared(ParkTicket::new(
            core.id(),
            generation,
        )))
    }

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

    fn commit_park_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: &Arc<ThreadCore>,
        token: &mut ParkTicket,
        rq_entry: OwnerRqEntry,
    ) -> Result<ParkCommit, TaskError> {
        if token.is_resolved() || current.id() != token.thread() {
            return Err(TaskError::StaleThreadId);
        }
        self.ensure_owner_cpu_context(&cpu)?;
        let remote = Arc::clone(cpu.remote());
        if let Some(registration) = token.deadline()
            && let Some(event) = cpu.as_mut().take_buffered_expiration(registration)
        {
            self.service_expired_park_deadline(event)?;
        }
        let initial_request = remote.claim_scheduler_request(SchedulerRequestScope::All);
        self.drain_owner_work(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let previous_core_hint = Arc::clone(current);

        if matches!(
            previous_core_hint.effective_policy_snapshot(),
            SchedulePolicy::Fair { .. }
                | SchedulePolicy::Fifo { .. }
                | SchedulePolicy::RoundRobin { .. }
        ) && !previous_core_hint
            .sched()
            .placement()
            .has_pending_migration()
            && let Some(commit) = self.try_commit_park_in_rq(
                cpu.as_mut(),
                token,
                &remote,
                &previous_core_hint,
                initial_request,
                rq_entry,
            )?
        {
            return Ok(commit);
        }

        // SAFETY: propagated from the selected entry contract.
        let mut previous_sched = unsafe { rq_entry.lock_thread_sched(previous_core_hint.sched()) };
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, &remote) };

        transaction.adopt_scheduler_request(initial_request);
        let scheduler_request = transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let clock = transaction.clock();
        let now_ns = clock.wall().as_nanos();

        if transaction.current_thread() != Some(token.thread()) {
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::StaleThreadId);
        }
        let Some(previous_core) = transaction.current_core() else {
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::NoRunnableThread);
        };
        if !Arc::ptr_eq(&previous_core, &previous_core_hint) {
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
            self.finish_owner_dispatch_commit(
                cpu.as_mut(),
                dispatch_commit,
                clock.wall().as_nanos(),
            );
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
        let next_core = next.core;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_1107, next_core.id().as_u64() as usize)
        });
        Self::stage_switch_handoff(
            cpu.as_mut(),
            Some(token.thread()),
            Some(Arc::clone(&previous_core)),
            Arc::clone(&next_core),
            None,
        );

        let deadline_rq_observation =
            transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());

        self.commit_owner_switch_selection(
            cpu.as_mut(),
            transaction,
            !dispatch_commit.has_deferred_task_lock_work(),
        );

        drop(previous_sched);
        let decision = Self::owner_switch_plan(
            Some(&previous_core),
            Some(previous_endpoint),
            &next_core,
            next_endpoint,
            SwitchReason::Blocked,
            now_ns,
        );
        self.finish_owner_dispatch_commit(cpu.as_mut(), dispatch_commit, clock.wall().as_nanos());
        let decision = self.finish_owner_selection(cpu.as_mut(), decision, deadline_rq_observation);

        token.mark_resolved();
        Ok(ParkCommit::Blocked(decision))
    }

    /// Implements Linux's ordinary Fair/FIFO/RR `__schedule()` block transition.
    ///
    /// Linux serializes normal scheduling state with `rq->lock`: Fair current
    /// owns an unlinked entity while FIFO/RR current remains linked in its rq
    /// class node. Task-control writers retain the `task lock -> rq` order;
    /// this path never acquires the task lock in reverse. Instead, a move-only
    /// marker makes task-lock readers wait while rq membership, placement, and
    /// the detached-or-delayed entity owner are published as one transition.
    /// Deadline bandwidth, migration, and special classes use the full path.
    fn try_commit_park_in_rq(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        token: &mut ParkTicket,
        remote: &Arc<CpuRemote>,
        previous_core: &Arc<ThreadCore>,
        initial_request: crate::system::cpu::SchedulerRequestClaim,
        rq_entry: OwnerRqEntry,
    ) -> Result<Option<ParkCommit>, TaskError> {
        let owner = cpu.owner();
        let placement = previous_core.sched().placement();
        // SAFETY: propagated from `commit_park_owner`'s selected entry
        // contract. The returned transaction does not outlive this helper.
        let mut transaction = unsafe { rq_entry.begin(self, remote) };
        let linked_current = transaction.is_linked_current(previous_core.id());
        let park_class = transaction.current().and_then(|current| {
            (current.thread() == token.thread()
                && Arc::ptr_eq(current.runtime_core_arc(), previous_core)
                && !current.is_dedicated_idle()
                && current.metadata().deadline_bandwidth_scaled == 0)
                .then(|| {
                    classify_rq_only_park_class(
                        current.schedule_policy(),
                        linked_current,
                        current.rt_quota_exempt(),
                    )
                })
                .flatten()
        });
        let eligible = park_class.is_some()
            && previous_core.state() == ThreadState::Parking
            && placement.queued_cpu() == Some(owner)
            && placement.on_cpu() == Some(owner)
            && !placement.has_pending_migration();
        if !eligible {
            transaction.commit();
            return Ok(None);
        }

        transaction.adopt_scheduler_request(initial_request);
        let scheduler_request = transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let clock = transaction.clock();
        let now_ns = clock.wall().as_nanos();

        if previous_core.park_generation() != token.generation() {
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::StaleThreadId);
        }
        if previous_core.take_park_notification() {
            previous_core
                .transition_state(ThreadState::Running)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x504b_1111, previous_core.id().as_u64() as usize)
                });
            cpu.restore_claimed_park_preemption(scheduler_request);
            transaction.commit_and_finish_scheduler_request();
            token.mark_resolved();
            return Ok(Some(ParkCommit::Notified));
        }

        cpu.defer_park_preemption(scheduler_request);
        let dispatch_commit = self.settle_owner_current_dispatch_in_rq(&mut transaction);

        if dispatch_commit.has_deferred_task_lock_work() {
            task_runtime::fatal_invariant(0x504b_1112, previous_core.id().as_u64() as usize);
        }
        let previous_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_1113, previous_core.id().as_u64() as usize)
        });

        let publication = previous_core
            .sched()
            .begin_active_publication()
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x504b_1119, previous_core.id().as_u64() as usize)
            });

        if previous_core
            .publish_blocked_from_parking()
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x504b_1114, previous_core.id().as_u64() as usize)
            })
            == ParkPublication::Notified
        {
            drop(publication);
            transaction.commit_and_finish_scheduler_request();
            self.finish_owner_dispatch_commit(
                cpu.as_mut(),
                dispatch_commit,
                clock.wall().as_nanos(),
            );
            cpu.finish_park_preemption(true);
            token.mark_resolved();
            return Ok(Some(ParkCommit::Notified));
        }

        if previous_core.state() != ThreadState::Blocked
            || placement.queued_cpu() != Some(owner)
            || placement.on_cpu() != Some(owner)
        {
            task_runtime::fatal_invariant(0x504b_1115, previous_core.id().as_u64() as usize);
        }

        let park_class = park_class.unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_111a, previous_core.id().as_u64() as usize)
        });
        match park_class {
            RqOnlyParkClass::Realtime => {
                let active = transaction
                    .deactivate_task(previous_core.id())
                    .into_active();
                placement.block_current(owner);
                // Publish the detached owner only after `on_rq = NONE`.
                publication.finish(active);
            }
            RqOnlyParkClass::Fair => {
                let timing_granularity_ns = self.config.timing_granularity_ns();
                let delayed = transaction
                    .delay_dequeue_unlinked_current(
                        previous_core.id(),
                        timing_granularity_ns,
                        false,
                    )
                    .is_some();
                if delayed {
                    // Linux keeps an ineligible Fair sleeper on-rq until pick
                    // or wake completes its delayed dequeue. Release the
                    // publication marker only after that rq owner is visible.
                    placement.delay_dequeue_current(owner);
                    publication.finish_rq_owned();
                } else {
                    transaction.deactivate_unlinked_current(previous_core.id());
                    let mut active = transaction
                        .take_current()
                        .and_then(CurrentDispatch::into_active)
                        .unwrap_or_else(|| {
                            task_runtime::fatal_invariant(
                                0x504b_111b,
                                previous_core.id().as_u64() as usize,
                            )
                        });
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
                    placement.block_current(owner);
                    publication.finish(active);
                }
            }
        }

        cpu.finish_park_preemption(false);
        let outgoing = transaction.take_current();
        match (park_class, outgoing) {
            (RqOnlyParkClass::Realtime, Some(outgoing)) => {
                if outgoing.thread() != previous_core.id() || outgoing.into_active().is_some() {
                    task_runtime::fatal_invariant(
                        0x504b_1117,
                        previous_core.id().as_u64() as usize,
                    );
                }
            }
            (RqOnlyParkClass::Fair, None) => {}
            _ => {
                task_runtime::fatal_invariant(0x504b_1117, previous_core.id().as_u64() as usize);
            }
        }
        transaction.merge_scheduler_request(SchedulerRequestScope::All);

        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, None);
        let next_core = next.core;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_1118, next_core.id().as_u64() as usize)
        });
        Self::stage_switch_handoff(
            cpu.as_mut(),
            Some(token.thread()),
            Some(Arc::clone(previous_core)),
            Arc::clone(&next_core),
            None,
        );

        let deadline_rq_observation =
            transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());

        self.commit_owner_switch_selection(cpu.as_mut(), transaction, true);

        let decision = Self::owner_switch_plan(
            Some(previous_core),
            Some(previous_endpoint),
            &next_core,
            next_endpoint,
            SwitchReason::Blocked,
            now_ns,
        );
        self.finish_owner_dispatch_commit(cpu.as_mut(), dispatch_commit, clock.wall().as_nanos());
        let decision = self.finish_owner_selection(cpu.as_mut(), decision, deadline_rq_observation);

        token.mark_resolved();
        Ok(Some(ParkCommit::Blocked(decision)))
    }

    /// Cancels a prepared park because an independent grant won the race.
    pub fn cancel_park(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
        token: &mut ParkTicket,
    ) -> Result<(), TaskError> {
        self.cancel_current_park(cpu, current.runtime_core_arc(), token)
    }

    pub(crate) fn cancel_current_park(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadCore,
        token: &mut ParkTicket,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if token.is_resolved() || current.id() != token.thread() {
            return Err(TaskError::StaleThreadId);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        let core = current;
        if core.park_generation() != token.generation() {
            return Err(TaskError::StaleThreadId);
        }
        let placement = core.sched().placement();
        if core.state() != ThreadState::Parking
            || placement.queued_cpu() != Some(cpu.owner())
            || placement.on_cpu() != Some(cpu.owner())
        {
            return Err(TaskError::StaleThreadId);
        }
        core.transition_state(ThreadState::Running)?;
        cpu.finish_park_preemption(true);
        token.mark_resolved();
        Ok(())
    }

    /// Validates all fallible current-thread exit prerequisites without
    /// publishing the thread as exited.
    pub(crate) fn prepare_current_exit(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
    ) -> Result<CurrentExitPermit, TaskError> {
        self.prepare_current_exit_inner(cpu, current, true)
    }

    pub(super) fn prepare_current_exit_inner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
        require_runtime_context: bool,
    ) -> Result<CurrentExitPermit, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut())?;
        let current_id = current.id();
        if cpu.remote().idle_thread() == Some(current_id) {
            return Err(TaskError::InvalidConfiguration);
        }
        let current_core = Arc::clone(current.runtime_core_arc());
        // Close before taking registry or thread-state locks. An activity that
        // won before this edge may need either lock to finish, just as Linux
        // takes p->pi_lock before rq/task-state validation rather than waiting
        // for a reader while holding rq.
        let scheduler_exit = current_core
            .close_owned_scheduler_activity()
            .ok_or(TaskError::ThreadBusy)?;
        let state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let record = state.thread_record(current_id)?;
        if !Arc::ptr_eq(&record.core, &current_core) {
            return Err(TaskError::StaleThreadId);
        }
        let sched = record.sched.lock();
        let placement = record.sched.placement();
        let lifecycle = sched.lifecycle.state();
        if lifecycle != ThreadState::Running {
            return Err(TaskError::InvalidTransition {
                from: lifecycle,
                to: ThreadState::Exited,
            });
        }
        if sched.pi.blocked_on.is_some() || !sched.pi.donors.is_empty() {
            return Err(TaskError::InvalidPiState);
        }
        if placement.queued_cpu() != Some(cpu.owner()) || placement.on_cpu() != Some(cpu.owner()) {
            return Err(TaskError::ThreadBusy);
        }
        if require_runtime_context && record.resources.context().is_none() {
            return Err(TaskError::InvalidRuntimeHandle);
        }
        record.callbacks.validate_prepare_exit()?;
        Ok(CurrentExitPermit {
            scheduler_exit,
            current_core,
        })
    }

    /// Atomically prepares and commits current-thread exit.
    ///
    /// Runtime integrations that publish OS completion between those phases
    /// use the crate-private prepared form instead.
    pub fn exit_current(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: ThreadHandle,
    ) -> Result<ScheduleDecision, TaskError> {
        // Pure scheduler users may model a transition without installing an
        // architecture context. The runtime facade uses the stricter prepared
        // form before publishing OS-visible completion.
        let permit = self.prepare_current_exit_inner(cpu.as_mut(), &current, false)?;
        // The architecture current entry no longer needs a lookup lease once
        // the permit pins its core. Release it before publishing Exited so its
        // eventual lease drop cannot manufacture pre-switch-tail reap work.
        drop(current);
        self.commit_current_exit_after_owner_drain(cpu, permit)
    }

    /// Commits a prepared current-thread exit and selects a replacement.
    /// Commits a prepared exit while the runtime owns the IRQ-off scheduler baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must remain active until this function returns.
    pub(crate) unsafe fn commit_prepared_current_exit(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        permit: CurrentExitPermit,
    ) -> Result<ScheduleDecision, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        // SAFETY: propagated from this method's scheduler-frame contract.
        unsafe { self.complete_context_switch_in_scheduler_frame(cpu.as_mut())? };
        self.drain_owner_work(cpu.as_mut())?;
        self.commit_current_exit_owner(cpu, permit, OwnerRqEntry::SchedulerFrame)
    }

    /// Commits the non-returning half of current exit after owner work drained.
    ///
    /// The move-only permit has already closed new scheduler activity. A
    /// message whose delivery reservation predates that close remains an
    /// in-flight late delivery and pins registry resources until its owner
    /// drains it as an exited no-op.
    pub(super) fn commit_current_exit_after_owner_drain(
        &self,
        cpu: Pin<&mut CpuLocal>,
        permit: CurrentExitPermit,
    ) -> Result<ScheduleDecision, TaskError> {
        self.commit_current_exit_owner(cpu, permit, OwnerRqEntry::IrqSave)
    }

    fn commit_current_exit_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        mut permit: CurrentExitPermit,
        rq_entry: OwnerRqEntry,
    ) -> Result<ScheduleDecision, TaskError> {
        let exiting = permit.thread();
        let exited_core = Arc::clone(permit.current_core());
        {
            let state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            let record = state.thread_record(exiting)?;
            if !Arc::ptr_eq(&record.core, &exited_core) {
                return Err(TaskError::StaleThreadId);
            }
            if record.has_live_pi_edges() {
                return Err(TaskError::InvalidPiState);
            }
            record.callbacks.validate_prepare_exit()?;
        }

        let remote = Arc::clone(cpu.remote());
        let initial_request = remote.claim_scheduler_request(SchedulerRequestScope::All);
        // SAFETY: propagated from the selected entry contract.
        let mut exited_sched = unsafe { rq_entry.lock_thread_sched(exited_core.sched()) };
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, &remote) };
        let clock = transaction.clock();
        let now_ns = clock.wall().as_nanos();
        if transaction.current_thread() != Some(exiting)
            || transaction
                .current_core()
                .is_none_or(|core| !Arc::ptr_eq(&core, &exited_core))
        {
            transaction.adopt_scheduler_request(initial_request);
            transaction.commit_and_finish_scheduler_request();
            return Err(TaskError::StaleThreadId);
        }
        transaction.adopt_scheduler_request(initial_request);
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let dispatch_commit = self.settle_owner_current_dispatch_in_rq(&mut transaction);
        // Exit necessarily selects a replacement, so accounting requests from
        // the outgoing task are consumed by this decision.
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let previous_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x4558_0007, exiting.as_u64() as usize)
        });
        let held_reservation = {
            let placement = exited_core.sched().placement();
            let sched = &mut *exited_sched;
            if sched.lifecycle.state() != ThreadState::Running
                || placement.queued_cpu() != Some(cpu.owner())
                || placement.on_cpu() != Some(cpu.owner())
            {
                task_runtime::fatal_invariant(0x4558_1101, exiting.as_u64() as usize);
            }
            Self::detach_owner_deadline_bandwidth_in_rq(
                &exited_core,
                sched,
                cpu.remote(),
                &mut transaction,
            );
            if transaction.is_linked_current(exiting) {
                transaction.deactivate_task(exiting);
            } else {
                transaction.deactivate_unlinked_current(exiting);
            }
            if sched.transition(&exited_core, ThreadState::Exited).is_err() {
                task_runtime::fatal_invariant(0x4558_0001, exiting.as_u64() as usize);
            }
            // Exit removes rq ownership immediately. The outgoing execution
            // claim remains in `on_cpu` until the per-CPU switch handoff tail
            // releases it, exactly like Linux `do_task_dead()` followed by
            // `finish_task_switch()`.
            placement.block_current(cpu.owner());
            permit.seal();
            let held = sched.held_deadline_reservation();
            sched.deadline.bandwidth.replace_detached_reservation(0);
            sched.policy.discard_pending_update();
            held
        };
        transaction.take_current();
        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, None);
        let next_core = next.core;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x4558_0008, exiting.as_u64() as usize)
        });
        Self::stage_switch_handoff(
            cpu.as_mut(),
            Some(exiting),
            Some(Arc::clone(&exited_core)),
            Arc::clone(&next_core),
            None,
        );
        self.validate_owner_runtime_switch_out(cpu.as_ref().get_ref(), &transaction);
        let deadline_rq_observation =
            transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        transaction.commit_and_finish_scheduler_request();
        drop(exited_sched);
        let decision = Self::owner_switch_plan(
            Some(&exited_core),
            Some(previous_endpoint),
            &next_core,
            next_endpoint,
            SwitchReason::Exited,
            now_ns,
        );
        self.finish_owner_dispatch_commit(cpu.as_mut(), dispatch_commit, clock.wall().as_nanos());

        {
            let mut state = self.state.lock();
            let record = state.thread_record_mut(exiting).unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x4558_0002, exiting.as_u64() as usize)
            });
            if record
                .callbacks
                .prepare_exit(record.extension.is_some())
                .is_err()
            {
                task_runtime::fatal_invariant(0x4558_0003, exiting.as_u64() as usize);
            }
            state.queue_exited_thread(exiting);
        }
        self.root_domain.lock().release_deadline(held_reservation);
        exited_core.notify_affinity_waiters();
        drop(permit);
        let decision = self.finish_owner_selection(cpu.as_mut(), decision, deadline_rq_observation);
        Ok(decision)
    }

    /// Completes the physical switch-out handoff in the newly active context.
    ///
    /// This second phase clears `on_cpu` only after architecture execution has
    /// left the previous stack. Deferred migration publication and exit hooks
    /// therefore cannot make a context runnable or reapable too early.
    #[doc(hidden)]
    pub fn complete_context_switch(
        &self,
        cpu: Pin<&mut CpuLocal>,
    ) -> Result<SwitchInCompletion, TaskError> {
        // SAFETY: the irqsave entry establishes its own IRQ ownership.
        unsafe { self.complete_context_switch_owner(cpu, OwnerRqEntry::IrqSave) }
    }

    /// Completes switch tail below the runtime's IRQ-off scheduler baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must remain active until this function returns.
    pub(crate) unsafe fn complete_context_switch_in_scheduler_frame(
        &self,
        cpu: Pin<&mut CpuLocal>,
    ) -> Result<SwitchInCompletion, TaskError> {
        // SAFETY: forwarded from this method's scheduler-frame contract.
        unsafe { self.complete_context_switch_owner(cpu, OwnerRqEntry::SchedulerFrame) }
    }

    /// Completes switch tail under the selected IRQ ownership protocol.
    ///
    /// # Safety
    ///
    /// `SchedulerFrame` requires an active IRQ-off runtime scheduler baton.
    pub(super) unsafe fn complete_context_switch_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        rq_entry: OwnerRqEntry,
    ) -> Result<SwitchInCompletion, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let Some(initial_handoff) = cpu.as_ref().get_ref().switch_handoff() else {
            return Ok(SwitchInCompletion::NONE);
        };
        let owner = cpu.owner();
        let previous_core = Arc::clone(initial_handoff.previous());
        let incoming = Arc::clone(initial_handoff.incoming());
        let migration_target = initial_handoff.migration_target();
        let runtime_tail_finished = initial_handoff.runtime_tail_is_finished();
        let rq_baton_retained = initial_handoff.has_rq_baton();
        if previous_core.id() == incoming.id()
            || previous_core.sched().placement().on_cpu() != Some(owner)
            || incoming.sched().placement().queued_cpu() != Some(owner)
            || incoming.sched().placement().on_cpu() != Some(owner)
            || (migration_target.is_some() && rq_baton_retained)
        {
            return Err(TaskError::InvalidConfiguration);
        }
        if !runtime_tail_finished {
            let reclaim_ready = task_runtime::finish_context_switch_tail();
            if cpu
                .as_mut()
                .finish_switch_runtime_tail(previous_core.id(), migration_target, reclaim_ready)
                .is_err()
            {
                task_runtime::fatal_invariant(0x5357_0001, previous_core.id().as_u64() as usize);
            }
        }
        let handoff = cpu
            .as_ref()
            .get_ref()
            .switch_handoff()
            .ok_or(TaskError::InvalidConfiguration)?;
        let previous = handoff.previous().id();
        if !Arc::ptr_eq(handoff.previous(), &previous_core)
            || !Arc::ptr_eq(handoff.incoming(), &incoming)
            || incoming.id() == previous
        {
            return Err(TaskError::InvalidConfiguration);
        }
        let (migration_target, previous_exited, affinity_completed) = if migration_target.is_some()
        {
            let placement = previous_core.sched().placement();

            // SAFETY: propagated from this method's selected entry contract.
            let mut sched = unsafe { rq_entry.lock_thread_sched(handoff.previous().sched()) };
            let remote = Arc::clone(cpu.remote());
            // SAFETY: propagated from this method's selected entry contract.
            let mut transaction = unsafe { rq_entry.begin(self, &remote) };

            let validation = self.validate_switch_handoff_state(
                owner,
                transaction.deadline_bandwidth(),
                handoff,
                placement,
                &sched,
            );
            let (migration_target, previous_exited) = match validation {
                Ok(validated) => validated,
                Err(error) => {
                    transaction.commit();
                    return Err(error);
                }
            };
            if migration_target.is_some() && sched.deadline.bandwidth.reservation_owner().is_some()
            {
                Self::detach_owner_deadline_bandwidth_in_rq(
                    &previous_core,
                    &mut sched,
                    &remote,
                    &mut transaction,
                );
            }
            // Linux `finish_task_switch()` clears `prev->on_cpu` before
            // releasing `rq->lock`; wake, migration, and reaping therefore
            // cannot observe a released execution claim with stale rq state.
            placement.finish_task(owner);
            transaction.commit();
            if let Some(target) = migration_target {
                previous_core.set_wake_cpu_hint(target);
            }
            let affinity_completed =
                Self::complete_affinity_if_satisfied_locked(&previous_core, &sched);
            (migration_target, previous_exited, affinity_completed)
        } else {
            // Linux `finish_task_switch()` runs `finish_task(prev)` — the
            // release-store of `prev->on_cpu` — while still holding
            // `rq->lock`, and only then `finish_lock_switch()` drops it.
            // Publishing the release inside the owner rq transaction keeps a
            // concurrent owner transaction (policy update classification and
            // re-link, wake, affinity reconcile) from observing `on_cpu`
            // flipping mid-transaction. Like Linux, ordinary switch tail does
            // not reopen `p->pi_lock`: remote affinity changes are serialized
            // through the rq owner's inbox, while current-task changes request
            // migration and rescheduling before reaching this tail.
            let previous_exited = if rq_baton_retained {
                let previous_exited = previous_core.state() == ThreadState::Exited;

                previous_core.sched().placement().finish_task(owner);
                if cpu.as_mut().finish_switch_rq_baton(previous_core.id()) != Ok(true) {
                    task_runtime::fatal_invariant(
                        0x5357_0005,
                        previous_core.id().as_u64() as usize,
                    );
                }
                previous_exited
            } else {
                let remote = Arc::clone(cpu.remote());
                // SAFETY: propagated from this method's selected entry contract.
                let transaction = unsafe { rq_entry.begin(self, &remote) };
                let previous_exited = previous_core.state() == ThreadState::Exited;

                previous_core.sched().placement().finish_task(owner);
                transaction.commit();
                previous_exited
            };

            (None, previous_exited, false)
        };

        if affinity_completed {
            previous_core.notify_affinity_waiters();
        }
        let consumed = cpu.as_mut().take_switch_handoff().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5357_0003, previous.as_u64() as usize)
        });
        if consumed.previous().id() != previous
            || consumed.incoming().id() != incoming.id()
            || consumed.migration_target() != migration_target
        {
            task_runtime::fatal_invariant(0x5357_0004, previous.as_u64() as usize);
        }
        let completed = consumed.into_runtime_finished().unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x5357_0004, previous.as_u64() as usize)
        });
        if !Arc::ptr_eq(&completed.previous, &previous_core)
            || !Arc::ptr_eq(&completed.incoming, &incoming)
        {
            task_runtime::fatal_invariant(0x5357_0004, previous.as_u64() as usize)
        }
        if let Some(migration) = completed.migration {
            migration.commit();
        }
        if completed.reclaim_ready {
            self.publish_resource_release_ready();
        }
        if previous_exited {
            self.task_work.publish();
        }
        let completion = SwitchInCompletion::for_core(&incoming);
        Ok(completion)
    }

    fn validate_switch_handoff_state(
        &self,
        owner: CpuId,
        bandwidth: DeadlineBandwidthSnapshot,
        handoff: &crate::system::cpu::SwitchHandoff,
        placement: &crate::system::thread_sched::SchedulerPlacement,
        sched: &ThreadSchedState,
    ) -> Result<(Option<CpuId>, bool), TaskError> {
        if placement.on_cpu() != Some(owner) {
            return Err(TaskError::InvalidConfiguration);
        }
        let migration_target = match handoff.migration_target() {
            Some(reserved_target) => {
                let target = placement
                    .committed_migration_target()
                    .ok_or(TaskError::InvalidConfiguration)?;
                if target != reserved_target {
                    return Err(TaskError::InvalidConfiguration);
                }
                if sched.lifecycle.state() != ThreadState::Running
                    || placement.queued_cpu().is_some()
                {
                    return Err(TaskError::InvalidConfiguration);
                }
                if let Some(assigned) = sched.deadline.bandwidth.reservation_owner() {
                    if assigned != owner {
                        return Err(TaskError::CpuOwnerMismatch {
                            expected: assigned.as_u32(),
                            actual: owner.as_u32(),
                        });
                    }
                    let reservation_scaled = sched.deadline.bandwidth.reservation_scaled();
                    if bandwidth.this_bw_scaled() < reservation_scaled
                        || (sched.deadline.bandwidth.is_active()
                            && bandwidth.running_bw_scaled() < reservation_scaled)
                    {
                        return Err(TaskError::InvalidConfiguration);
                    }
                }
                Some(target)
            }
            None => None,
        };
        Ok((
            migration_target,
            sched.lifecycle.state() == ThreadState::Exited,
        ))
    }
}
