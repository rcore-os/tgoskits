//! Park run queue under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Implements Linux's ordinary Fair/FIFO/RR `__schedule()` block transition.
    ///
    /// Linux serializes normal scheduling state with `rq->lock`: Fair current
    /// owns an unlinked entity while FIFO/RR current remains linked in its rq
    /// class node. Task-control writers retain the `task lock -> rq` order;
    /// this path never acquires the task lock in reverse. Instead, a move-only
    /// marker makes task-lock readers wait while rq membership, placement, and
    /// the detached-or-delayed entity owner are published as one transition.
    /// Deadline bandwidth, migration, and special classes use the full path.
    pub(super) fn try_commit_park_in_rq(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        token: &mut ParkTicket,
        remote: &CpuRemote,
        previous_core: &Arc<ThreadCore>,
        initial_request: crate::sched::system::cpu::SchedulerRequestClaim,
        rq_entry: OwnerRqEntry,
    ) -> Result<Option<ParkCommit>, TaskError> {
        let owner = cpu.owner();
        let placement = previous_core.sched().placement();
        // SAFETY: propagated from `commit_park_owner`'s selected entry
        // contract. The returned transaction does not outlive this helper.
        let mut transaction = unsafe { rq_entry.begin(self, remote) };
        let linked_current = transaction.is_linked_current(previous_core.id());
        let current_core_matches = transaction
            .current_core_ref()
            .is_some_and(|current| core::ptr::eq(current, previous_core.as_ref()));
        let park_class = transaction.current().and_then(|current| {
            (current.thread() == token.thread()
                && current_core_matches
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

        let previous_fifo = transaction.current().is_some_and(|current| {
            matches!(current.schedule_policy(), SchedulePolicy::Fifo { .. })
        });
        transaction.adopt_scheduler_request(initial_request);
        let scheduler_request = transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let now_ns = transaction.clock().wall().as_nanos();

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
        let previous_urgency = transaction.current_scheduling_urgency().unwrap_or_else(|| {
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
            self.finish_owner_dispatch_commit(dispatch_commit);
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
        let previous_runtime_owner = match park_class {
            RqOnlyParkClass::Realtime => {
                // End the rq-current borrow while its RT node is still linked.
                // The caller retains previous_core through handoff construction;
                // unlink therefore need not clone a temporary current owner and
                // metadata only to discard them before selecting the next task.
                let outgoing = transaction.take_current().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x504b_1117, previous_core.id().as_u64() as usize)
                });
                if outgoing.thread() != previous_core.id() || outgoing.into_active().is_some() {
                    task_runtime::fatal_invariant(
                        0x504b_1117,
                        previous_core.id().as_u64() as usize,
                    );
                }
                let QueuedThread {
                    active,
                    core: runtime_owner,
                    ..
                } = transaction.deactivate_task(previous_core.id());
                placement.block_current(owner);
                // Publish the detached owner only after `on_rq = NONE`.
                publication.finish(active);
                runtime_owner
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
                Arc::clone(previous_core)
            }
        };

        cpu.finish_park_preemption(false);
        if transaction.current().is_some() {
            task_runtime::fatal_invariant(0x504b_1117, previous_core.id().as_u64() as usize);
        }
        transaction.merge_scheduler_request(SchedulerRequestScope::All);

        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, None);
        let OwnerNext {
            core: next_core,
            policy: next_policy_ref,
            urgency: next_urgency,
        } = next;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x504b_1118, next_core.as_ref().id().as_u64() as usize)
        });
        let handoff = Self::prepare_switch_handoff(
            Some(token.thread()),
            Some(PreviousSwitchOwnership::retained(previous_runtime_owner)),
            next_core,
            next_policy_ref,
            PreviousSwitchDisposition::Live,
            None,
        );

        // FIFO has no per-task hrtick. Blocking changes rq membership, but
        // another FIFO dispatch retains the same shared timer heads, just as
        // the existing FIFO yield selection does. Retain the shared Fair
        // balance timer only when blocking cannot change its runnable-work
        // predicate: no Fair tasks, or more than the new FIFO current remain.
        let scheduler_deadline = if previous_fifo
            && matches!(next_policy_ref.get(), SchedulePolicy::Fifo { .. })
            && (!transaction.has_fair() || transaction.nr_running() > 1)
        {
            OwnerSchedulerDeadline::Unchanged
        } else {
            OwnerSchedulerDeadline::Reevaluate(
                transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref()),
            )
        };

        self.commit_owner_switch_selection(cpu.as_mut(), transaction, handoff, true);

        self.finish_owner_dispatch_commit(dispatch_commit);
        self.finish_owner_selection(
            cpu.as_mut(),
            Some(previous_endpoint.thread()),
            next_endpoint.thread(),
            Some(previous_urgency),
            next_urgency,
            scheduler_deadline,
        );
        let decision = Self::owner_switch_plan(
            Some(previous_endpoint),
            next_endpoint,
            SwitchReason::Blocked,
            now_ns,
        );

        token.mark_resolved();
        Ok(Some(ParkCommit::Blocked(decision)))
    }
}
