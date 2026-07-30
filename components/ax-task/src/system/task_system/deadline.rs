//! Deadline diagnostics, deferred callbacks, and owner timer service.

use super::*;

impl TaskSystem {
    fn task_deadline_error(error: TaskDeadlineError) -> TaskError {
        match error {
            TaskDeadlineError::Capacity => TaskError::TimerCapacity,
            TaskDeadlineError::InvalidDeadline
            | TaskDeadlineError::GenerationExhausted
            | TaskDeadlineError::KindMismatch => TaskError::InvalidConfiguration,
        }
    }

    fn replace_owner_deadline_timer(
        mut cpu: Pin<&mut CpuLocal>,
        node: &TaskDeadlineNode,
        registration: &mut Option<TaskDeadlineRegistration>,
        deadline_ns: Option<u64>,
        kind: TaskDeadlineKind,
    ) -> Result<(), TaskError> {
        let deadline_ns = deadline_ns.filter(|deadline| *deadline != 0 && *deadline != u64::MAX);
        if registration.as_ref().is_some_and(|registration| {
            Some(registration.deadline_ns()) == deadline_ns && registration.kind() == kind
        }) {
            return Ok(());
        }
        if let Some(previous) = registration.take() {
            // Expiration may already have moved the value-owned entry into the
            // safe-point buffer. A later token makes that buffered copy stale;
            // physical cancellation is required only while it remains queued.
            let _removed = cpu.as_mut().task_deadlines().cancel(&previous);
        }
        let Some(deadline_ns) = deadline_ns else {
            return Ok(());
        };
        *registration = Some(
            cpu.as_mut()
                .task_deadlines()
                .arm(node, deadline_ns, kind)
                .map_err(Self::task_deadline_error)?,
        );
        Ok(())
    }

    pub(super) fn refresh_owner_deadline_timers_locked(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let owns_bandwidth = sched.deadline_bandwidth_cpu == Some(owner);
        let cbs_deadline_ns = owns_bandwidth
            .then_some(())
            .filter(|_| sched.deadline_cbs_borrower.is_none())
            .and(sched.base_deadline)
            .map(DeadlineEntity::next_scheduler_event_ns);
        Self::replace_owner_deadline_timer(
            cpu.as_mut(),
            core.deadline_cbs_timer(),
            &mut sched.deadline_cbs_timer,
            cbs_deadline_ns,
            TaskDeadlineKind::DeadlineCbs,
        )?;

        let zero_lag_ns = (owns_bandwidth
            && sched.deadline_activity == DeadlineActivity::ActiveNonContending)
            .then_some(sched.deadline_zero_lag_ns);
        Self::replace_owner_deadline_timer(
            cpu,
            core.deadline_zero_lag_timer(),
            &mut sched.deadline_zero_lag_timer,
            zero_lag_ns,
            TaskDeadlineKind::DeadlineZeroLag,
        )
    }

    pub(super) fn cancel_owner_deadline_timers_locked(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<(), TaskError> {
        Self::replace_owner_deadline_timer(
            cpu.as_mut(),
            core.deadline_cbs_timer(),
            &mut sched.deadline_cbs_timer,
            None,
            TaskDeadlineKind::DeadlineCbs,
        )?;
        Self::replace_owner_deadline_timer(
            cpu,
            core.deadline_zero_lag_timer(),
            &mut sched.deadline_zero_lag_timer,
            None,
            TaskDeadlineKind::DeadlineZeroLag,
        )
    }

    fn registration_matches(
        registration: &TaskDeadlineRegistration,
        event: ExpiredTaskDeadline,
    ) -> bool {
        event.thread() == Some(registration.thread())
            && event.token() == registration.token()
            && event.deadline_ns() == registration.deadline_ns()
            && event.kind() == Some(registration.kind())
    }

    fn take_expired_registration(
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
        let budget = cpu.batch_limit();
        let batch =
            cpu.as_mut()
                .expire_task_deadlines(now_ns, task_runtime::timer_resolution_ns(), budget);
        if cpu.as_mut().begin_deadline_work() {
            let mut processed = 0;
            let service = (|| {
                while processed < budget {
                    let Some(event) = cpu.as_mut().take_expired_scheduler_deadline() else {
                        break;
                    };
                    self.service_expired_scheduler_deadline(cpu.as_mut(), event, now_ns)?;
                    processed += 1;
                }
                Ok(())
            })();
            if let Err(error) = service {
                cpu.as_mut().finish_deadline_work(true);
                return Err(error);
            }
            let pending = batch.pending() || cpu.has_expired_task_deadlines();
            cpu.as_mut().finish_deadline_work(pending);
        }
        cpu.as_mut().refresh_scheduler_deadline(now_ns);
        Ok(())
    }

    fn service_expired_scheduler_deadline(
        &self,
        cpu: Pin<&mut CpuLocal>,
        event: ExpiredTaskDeadline,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        let Some(thread) = event.thread() else {
            return Ok(());
        };
        let core = {
            let state = self.state.lock();
            match state.thread_record(thread) {
                Ok(record) => Arc::clone(&record.core),
                Err(TaskError::StaleThreadId) => return Ok(()),
                Err(error) => return Err(error),
            }
        };
        match event.kind() {
            Some(TaskDeadlineKind::DeadlineCbs) => {
                self.service_expired_deadline_cbs(cpu, core, event, now_ns)
            }
            Some(TaskDeadlineKind::DeadlineZeroLag) => {
                Self::service_expired_deadline_zero_lag(cpu, core, event)
            }
            Some(TaskDeadlineKind::ParkTimeout { .. }) | None => Ok(()),
        }
    }

    fn service_expired_deadline_cbs(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        event: ExpiredTaskDeadline,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut update_queued = None;
        let mut replenish = false;
        {
            let mut sched = core.sched().lock();
            if !Self::take_expired_registration(&mut sched.deadline_cbs_timer, event) {
                return Ok(());
            }
            if sched.deadline_bandwidth_cpu != Some(owner) {
                return Err(TaskError::CpuOwnerMismatch {
                    expected: sched.deadline_bandwidth_cpu.map_or(u32::MAX, CpuId::as_u32),
                    actual: owner.as_u32(),
                });
            }
            if sched.deadline_cbs_borrower.is_some() {
                // The borrower owns the mutable CBS copy. Its baton-return
                // message will refresh this exact donor rather than scanning
                // the CPU reservation set.
                return Ok(());
            }
            let Some(mut deadline) = sched.base_deadline else {
                return Ok(());
            };
            let missed = deadline.observe_time(now_ns);
            let replenish_due =
                deadline.is_throttled() && now_ns >= deadline.next_scheduler_event_ns();
            if replenish_due {
                deadline.replenish(now_ns);
                sched.base_deadline = Some(deadline);
                sched.base_entity = SchedulingEntity::Deadline(deadline);
                if !sched.is_pi_boosted() {
                    sched.entity = sched.base_entity;
                    core.publish_effective_schedule(sched.policy, sched.entity);
                }
                if !deadline.is_throttled() {
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
                }
            } else if missed {
                sched.base_deadline = Some(deadline);
                sched.base_entity = SchedulingEntity::Deadline(deadline);
                if !sched.is_pi_boosted() {
                    sched.entity = sched.base_entity;
                    core.publish_effective_schedule(sched.policy, sched.entity);
                    if sched.placement.queued_cpu() == Some(owner) {
                        update_queued = Some(SchedulingEntity::Deadline(deadline));
                    }
                }
            }
            Self::refresh_owner_deadline_timers_locked(&core, &mut sched, cpu.as_mut())?;
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
            self.enqueue_owner_thread(cpu, core, now_ns, EnqueueReason::Replenished)?;
        }
        Ok(())
    }

    fn service_expired_deadline_zero_lag(
        mut cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        event: ExpiredTaskDeadline,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        if !Self::take_expired_registration(&mut sched.deadline_zero_lag_timer, event) {
            return Ok(());
        }
        if sched.deadline_bandwidth_cpu != Some(owner) {
            return Err(TaskError::CpuOwnerMismatch {
                expected: sched.deadline_bandwidth_cpu.map_or(u32::MAX, CpuId::as_u32),
                actual: owner.as_u32(),
            });
        }
        if sched.deadline_activity == DeadlineActivity::ActiveNonContending
            && event.deadline_ns() >= sched.deadline_zero_lag_ns
        {
            cpu.as_mut()
                .fields_mut()
                .deactivate_deadline_bandwidth(sched.deadline_bandwidth_scaled)?;
            sched.deadline_activity = DeadlineActivity::Inactive;
            sched.deadline_zero_lag_ns = 0;
        }
        Self::refresh_owner_deadline_timers_locked(&core, &mut sched, cpu)
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
