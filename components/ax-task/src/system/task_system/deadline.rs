//! Deadline diagnostics, deferred callbacks, and owner timer service.

use super::*;

impl TaskSystem {
    /// Returns Deadline budget and PI rescue state for diagnostics and ABI glue.
    pub fn deadline_runtime(&self, thread: ThreadId) -> Result<DeadlineRuntimeSnapshot, TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        let sched = record.sched.lock();
        let deadline = sched
            .base_deadline
            .or(match sched.entity {
                SchedulingEntity::Deadline(deadline) => Some(deadline),
                _ => None,
            })
            .ok_or(TaskError::InvalidConfiguration)?;
        Ok(DeadlineRuntimeSnapshot {
            remaining_runtime_ns: deadline.remaining_runtime_ns(),
            misses: deadline.misses(),
            overruns: deadline.overruns(),
            pi_critical_rescue: sched.pi_critical_rescue,
            donor: sched.deadline_donor,
        })
    }

    /// Returns the thread's GRUB activity, zero-lag, and runqueue ownership.
    pub fn deadline_activity(
        &self,
        thread: ThreadId,
    ) -> Result<DeadlineActivitySnapshot, TaskError> {
        let state = self.state.lock();
        let record = state.thread_record(thread)?;
        let sched = record.sched.lock();
        if !matches!(sched.active_base_policy, SchedulePolicy::Deadline(_)) {
            return Err(TaskError::InvalidConfiguration);
        }
        Ok(DeadlineActivitySnapshot {
            activity: sched.deadline_activity,
            bandwidth_cpu: sched.deadline_bandwidth_cpu,
            zero_lag_ns: sched.deadline_zero_lag_ns,
        })
    }

    /// Runs a bounded, allocation-free batch of deferred Deadline callbacks.
    ///
    /// Timer IRQ only publishes pending state. This task-context operation drops
    /// the registry lock before invoking any OS extension callback. Callback
    /// collection retains one existing thread-core reference at a time instead
    /// of allocating temporary storage in a scheduler-adjacent safe point.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::UnsafeContext`] without consuming an event in hard
    /// IRQ context, and [`TaskError::ThreadBusy`] when another task-work
    /// consumer is already active.
    pub fn dispatch_deadline_overruns(&self, limit: usize) -> Result<usize, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let _consumer = self.task_work.try_claim_consumer()?;
        self.dispatch_deadline_overruns_inner(limit)
            .map(|(_, dispatched)| dispatched)
    }

    pub(super) fn dispatch_deadline_overruns_inner(
        &self,
        limit: usize,
    ) -> Result<(usize, usize), TaskError> {
        const MAX_DISPATCH_BATCH: usize = 64;

        let mut processed = 0;
        let mut dispatched = 0;
        while processed < limit.min(MAX_DISPATCH_BATCH) {
            let claimed = {
                let mut state = self.state.lock();
                state.claim_pending_deadline_overrun()
            };
            let Some(callback) = claimed else {
                break;
            };
            processed += 1;
            let Some((extension, thread)) = callback else {
                continue;
            };
            // SAFETY: the registry's callback claim prevents reaping while the
            // callback runs, and every scheduler lock was released above.
            unsafe {
                (extension.ops().on_deadline_overrun)(extension.data(), thread);
            }
            self.state.lock().finish_deadline_callback(thread)?;
            dispatched += 1;
        }
        Ok((processed, dispatched))
    }

    pub(super) fn service_deadline_timers(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        let (start, examined) = cpu.as_mut().fields_mut().begin_deadline_scan_batch();
        if examined == 0 {
            cpu.as_mut().refresh_scheduler_deadline(now_ns);
            return Ok(());
        }
        let owner = cpu.owner();
        for offset in 0..examined {
            let index = (start + offset) % cpu.deadline_members.len();
            let core = Arc::clone(&cpu.deadline_members[index]);
            let mut update_queued = None;
            let mut replenish = false;
            {
                let mut sched = core.sched().lock();
                if sched.deadline_bandwidth_cpu != Some(owner) {
                    return Err(TaskError::CpuOwnerMismatch {
                        expected: sched.deadline_bandwidth_cpu.map_or(u32::MAX, CpuId::as_u32),
                        actual: owner.as_u32(),
                    });
                }
                if sched.deadline_cbs_borrower.is_some() {
                    // The remote PI owner holds the only mutable copy of this
                    // CBS entity in its CurrentDispatch. Its CPU owns the
                    // corresponding execution-budget clockevent until the
                    // baton is committed back below a scheduler safe point.
                    // Re-arming the donor's stale copy would give two CPUs
                    // timer ownership and, once overdue, create a resolution-
                    // rate interrupt loop without advancing CBS state.
                    continue;
                }
                if sched.deadline_activity == DeadlineActivity::ActiveNonContending {
                    if now_ns >= sched.deadline_zero_lag_ns {
                        cpu.as_mut()
                            .fields_mut()
                            .deactivate_deadline_bandwidth(sched.deadline_bandwidth_scaled)?;
                        sched.deadline_activity = DeadlineActivity::Inactive;
                        sched.deadline_zero_lag_ns = 0;
                    } else {
                        cpu.arm_deferred_scheduler_deadline(sched.deadline_zero_lag_ns);
                    }
                }
                let Some(mut deadline) = sched.base_deadline else {
                    continue;
                };
                let missed = deadline.observe_time(now_ns);
                let replenish_due =
                    deadline.is_throttled() && now_ns >= deadline.next_scheduler_event_ns();
                let next_event_ns = deadline.next_scheduler_event_ns();
                if !replenish_due && next_event_ns > now_ns {
                    cpu.arm_deferred_scheduler_deadline(next_event_ns);
                }
                if replenish_due {
                    deadline.replenish(now_ns);
                    sched.base_deadline = Some(deadline);
                    sched.base_entity = SchedulingEntity::Deadline(deadline);
                    if !sched.is_pi_boosted() {
                        sched.entity = sched.base_entity;
                        core.publish_effective_schedule(sched.policy, sched.entity);
                    }
                    if deadline.is_throttled() {
                        cpu.arm_deferred_scheduler_deadline(deadline.next_scheduler_event_ns());
                        continue;
                    }
                    if sched.deadline_replenish_pending {
                        sched.deadline_replenish_pending = false;
                        match sched.lifecycle.state() {
                            ThreadState::Blocked => {
                                sched.transition(&core, ThreadState::Waking)?;
                                sched.transition(&core, ThreadState::Ready)?;
                            }
                            ThreadState::Waking => sched.transition(&core, ThreadState::Ready)?,
                            ThreadState::Ready => {}
                            _ => return Err(TaskError::InvalidConfiguration),
                        }
                        replenish = true;
                    } else if !sched.is_pi_boosted() && sched.placement.queued_cpu() == Some(owner)
                    {
                        update_queued = Some(SchedulingEntity::Deadline(deadline));
                    }
                } else if missed {
                    sched.base_deadline = Some(deadline);
                    sched.base_entity = SchedulingEntity::Deadline(deadline);
                    if !sched.is_pi_boosted() {
                        sched.entity = sched.base_entity;
                        if sched.placement.queued_cpu() == Some(owner) {
                            update_queued = Some(SchedulingEntity::Deadline(deadline));
                        }
                    }
                }
            }
            if let Some(entity) = update_queued
                && !cpu
                    .as_mut()
                    .fields_mut()
                    .run_queue
                    .update_deadline_entity(core.id(), entity)
            {
                return Err(TaskError::InvalidConfiguration);
            }
            if replenish {
                self.enqueue_owner_thread(cpu.as_mut(), core, now_ns, EnqueueReason::Replenished)?;
            }
        }
        if cpu
            .as_mut()
            .fields_mut()
            .finish_deadline_scan_batch(examined)
        {
            cpu.request_scheduler_work();
        }
        cpu.as_mut().refresh_scheduler_deadline(now_ns);
        Ok(())
    }

    /// Returns a monotonic sample suitable for work scheduled after this
    /// scheduler operation completes.
    ///
    /// Runtime accounting deliberately uses one entry snapshot throughout a
    /// scheduling decision. Deadline publication has different semantics: its
    /// relative intervals start when the scheduler returns work to task
    /// context. Like Linux hrtimer interrupt reprogramming, resample after
    /// potentially expensive callbacks or balancing and never move backwards
    /// from the caller's coherent accounting snapshot.
    pub(super) fn scheduler_completion_now_ns(entry_now_ns: u64) -> u64 {
        task_runtime::monotonic_ns().max(entry_now_ns)
    }

    pub(super) fn program_local_timer(
        mut cpu: Pin<&mut CpuLocal>,
        entry_now_ns: u64,
    ) -> Result<(), TaskError> {
        let completion_now_ns = Self::scheduler_completion_now_ns(entry_now_ns);
        cpu.as_mut().refresh_scheduler_deadline(completion_now_ns);
        let resolution_ns = task_runtime::timer_resolution_ns();
        let update = cpu
            .as_mut()
            .next_task_deadline_update(completion_now_ns, resolution_ns)?;
        ensure_runtime_success(task_runtime::publish_task_deadline(update))
    }
}
