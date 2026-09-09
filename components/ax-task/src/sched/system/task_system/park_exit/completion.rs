//! Completion under the owning scheduler transaction.

use super::*;

impl TaskSystem {
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
    pub(in crate::sched::system::task_system) unsafe fn complete_context_switch_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        rq_entry: OwnerRqEntry,
    ) -> Result<SwitchInCompletion, TaskError> {
        if rq_entry.requires_owner_context_validation() {
            self.ensure_owner_cpu_context(&cpu)?;
        }
        let Some(initial_handoff) = cpu.as_ref().get_ref().switch_handoff() else {
            return Ok(SwitchInCompletion::NONE);
        };
        let owner = cpu.owner();
        // SAFETY: this owner-only switch tail retains the pinned CPU-local
        // capability until every derived rq transaction is complete.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let previous_id = initial_handoff.previous().id();
        let migration_target = initial_handoff.migration_target();
        debug_assert_ne!(previous_id, initial_handoff.incoming().id());
        debug_assert_eq!(
            initial_handoff.previous().sched().placement().on_cpu(),
            Some(owner)
        );
        debug_assert!(!(migration_target.is_some() && initial_handoff.has_rq_baton()));
        debug_assert_eq!(
            initial_handoff.previous_exited(),
            initial_handoff.previous().state() == ThreadState::Exited
        );

        // Migration completion needs scheduler and deadline state that is only
        // consistently observable under the owner rq transaction. Validate it
        // before publishing the architecture runtime tail; once that tail is
        // complete, the same checks are commit invariants rather than a
        // recoverable retry boundary.
        if migration_target.is_some() {
            let previous_core =
                Arc::clone(initial_handoff.retained_previous().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5357_0007, previous_id.as_u64() as usize)
                }));
            let placement = previous_core.sched().placement();
            // SAFETY: propagated from this method's selected entry contract.
            let sched = unsafe { rq_entry.lock_thread_sched(initial_handoff.previous().sched()) };
            // SAFETY: propagated from this method's selected entry contract.
            let transaction = unsafe { rq_entry.begin(self, remote) };
            let validation = self.validate_switch_handoff_state(
                owner,
                transaction.deadline_bandwidth(),
                initial_handoff,
                placement,
                &sched,
            );
            transaction.commit();
            validation?;
        }
        let reclaim_ready = task_runtime::finish_context_switch_tail();
        #[cfg(feature = "qperf-metrics")]
        let qperf_owner_runtime_publish_finished_ns = task_runtime::monotonic_now().as_nanos();
        if migration_target.is_none() {
            // Linux's ordinary `finish_task_switch()` consumes only the local
            // previous-task and rq-lock state. Keep that state in its
            // CPU-local slot so the hot path never moves the much wider remote
            // migration reservation through the scheduler stack.
            let handoff = cpu.as_mut().switch_handoff_mut().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5357_0003, previous_id.as_u64() as usize)
            });
            let rq_baton = handoff.take_local_rq_baton().unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5357_0006, previous_id.as_u64() as usize)
            });
            handoff.previous().sched().placement().finish_task(owner);
            if let Some(baton) = rq_baton {
                if baton.finish(owner).is_err() {
                    task_runtime::fatal_invariant(0x5357_0005, previous_id.as_u64() as usize);
                }
            } else {
                // SAFETY: propagated from this method's selected entry contract.
                let transaction = unsafe { rq_entry.begin(self, remote) };
                transaction.commit();
            }
            let incoming = handoff.incoming_ref();
            let incoming_policy = handoff.incoming_policy();
            let incoming_runtime_ns = handoff.incoming_runtime_ns();
            let previous_exited = handoff.previous_exited();
            let trace_wake = handoff.take_trace_wake();
            #[cfg(feature = "qperf-metrics")]
            let qperf_owner_finish_prev_finished_ns = task_runtime::monotonic_now().as_nanos();
            #[cfg(feature = "qperf-metrics")]
            crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
                24,
                qperf_owner_runtime_publish_finished_ns,
                qperf_owner_finish_prev_finished_ns,
            );
            cpu.as_mut().clear_switch_handoff().unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5357_0004, previous_id.as_u64() as usize)
            });
            #[cfg(feature = "qperf-metrics")]
            let qperf_owner_consume_handoff_finished_ns = task_runtime::monotonic_now().as_nanos();
            #[cfg(feature = "qperf-metrics")]
            crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
                25,
                qperf_owner_finish_prev_finished_ns,
                qperf_owner_consume_handoff_finished_ns,
            );
            if reclaim_ready {
                self.publish_resource_release_ready();
            }
            if previous_exited {
                self.task_work.publish();
            }
            #[cfg(feature = "qperf-metrics")]
            crate::diagnostics::counters::qperf_record_switch_phase_owner_tail(
                qperf_owner_runtime_publish_finished_ns,
                task_runtime::monotonic_now().as_nanos(),
            );
            return Ok(SwitchInCompletion::for_core(
                incoming.as_ref(),
                incoming_policy,
                incoming_runtime_ns,
            )
            .with_trace_wake(trace_wake));
        }
        // The architecture switch is now irreversible. Move the one owner
        // token out of the CPU-local slot and consume that exact state through
        // the migration tail; no post-switch path may rediscover or revalidate
        // a second mutable identity.
        let handoff = cpu.as_mut().take_switch_handoff().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5357_0003, previous_id.as_u64() as usize)
        });
        let affinity_completed = {
            let previous_core = Arc::clone(handoff.retained_previous().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5357_0007, previous_id.as_u64() as usize)
            }));
            let placement = previous_core.sched().placement();

            // SAFETY: propagated from this method's selected entry contract.
            let mut sched = unsafe { rq_entry.lock_thread_sched(previous_core.sched()) };
            // SAFETY: propagated from this method's selected entry contract.
            let mut transaction = unsafe { rq_entry.begin(self, remote) };

            let validation = self.validate_switch_handoff_state(
                owner,
                transaction.deadline_bandwidth(),
                &handoff,
                placement,
                &sched,
            );
            let migration_target = match validation {
                Ok(validated) => validated,
                Err(_) => {
                    transaction.commit();
                    task_runtime::fatal_invariant(0x5357_0007, previous_core.id().as_u64() as usize)
                }
            };
            if migration_target.is_some() && sched.deadline.bandwidth.reservation_owner().is_some()
            {
                Self::detach_owner_deadline_bandwidth_in_rq(
                    &previous_core,
                    &mut sched,
                    remote,
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
            Self::complete_affinity_if_satisfied_locked(&previous_core, &sched)
        };
        #[cfg(feature = "qperf-metrics")]
        let qperf_owner_finish_prev_finished_ns = task_runtime::monotonic_now().as_nanos();
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
            24,
            qperf_owner_runtime_publish_finished_ns,
            qperf_owner_finish_prev_finished_ns,
        );

        if affinity_completed {
            handoff.previous().notify_affinity_waiters();
        }
        let completed = handoff
            .complete_migration(reclaim_ready)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5357_0004, previous_id.as_u64() as usize)
            });
        #[cfg(feature = "qperf-metrics")]
        let qperf_owner_consume_handoff_finished_ns = task_runtime::monotonic_now().as_nanos();
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
            25,
            qperf_owner_finish_prev_finished_ns,
            qperf_owner_consume_handoff_finished_ns,
        );
        completed.migration.commit();
        if completed.reclaim_ready {
            self.publish_resource_release_ready();
        }
        if completed.previous_exited {
            self.task_work.publish();
        }
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::qperf_record_switch_phase_owner_tail(
            qperf_owner_runtime_publish_finished_ns,
            task_runtime::monotonic_now().as_nanos(),
        );
        let completion = SwitchInCompletion::for_core(
            completed.incoming.as_ref(),
            completed.incoming_policy.get(),
            completed.incoming_runtime_ns,
        )
        .with_trace_wake(completed.trace_wake);
        Ok(completion)
    }

    pub(super) fn validate_switch_handoff_state(
        &self,
        owner: CpuId,
        bandwidth: DeadlineBandwidthSnapshot,
        handoff: &crate::sched::system::cpu::SwitchHandoff,
        placement: &crate::sched::system::thread_sched::SchedulerPlacement,
        sched: &ThreadSchedState,
    ) -> Result<Option<CpuId>, TaskError> {
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
        Ok(migration_target)
    }
}
