//! Affinity updates and owner-to-owner placement delivery.

use super::*;

impl TaskSystem {
    pub(super) fn publish_owner_migration(
        &self,
        core: &Arc<ThreadCore>,
        inbox_cpu: CpuId,
        source: CpuId,
        target: CpuId,
    ) -> Result<(), TaskError> {
        let remote = self
            .cpu_remote(inbox_cpu)
            .ok_or(TaskError::CpuOffline(inbox_cpu.as_u32()))?;
        if !core.reserve_scheduler_inbox_delivery() {
            return Ok(());
        }
        let pointer = Arc::as_ptr(core);
        unsafe {
            // The retained count is transferred to the intrusive inbox.
            Arc::increment_strong_count(pointer);
        }
        let node = unsafe {
            // The transferred Arc count keeps the embedded node pinned.
            Pin::new_unchecked((*pointer).migration_node())
        };
        let message = InboxMessage::migration_with_payload(
            core.id(),
            source,
            target,
            core.id().generation() as u64,
            pointer.expose_provenance(),
        );
        if remote.publish_migration(node, message) != PublishResult::Published {
            unsafe {
                // A rejected/coalesced publication did not consume this count.
                Arc::decrement_strong_count(pointer);
            }
            core.cancel_scheduler_inbox_delivery();
        }
        Ok(())
    }

    pub(super) fn publish_owner_policy_retry(
        &self,
        core: &Arc<ThreadCore>,
        owner: CpuId,
        generation: u64,
    ) -> Result<(), TaskError> {
        let remote = self
            .cpu_remote(owner)
            .ok_or(TaskError::CpuOffline(owner.as_u32()))?;
        if !core.reserve_scheduler_inbox_delivery() {
            return Ok(());
        }
        let pointer = Arc::as_ptr(core);
        // SAFETY: this count is transferred to the embedded inbox node and
        // consumed by exactly one later owner drain.
        unsafe { Arc::increment_strong_count(pointer) };
        // SAFETY: the transferred Arc count keeps the embedded node pinned.
        let node = unsafe { Pin::new_unchecked((*pointer).policy_update_node()) };
        let message = InboxMessage::policy_update_with_payload(
            core.id(),
            owner,
            generation,
            pointer.expose_provenance(),
        );
        if remote.publish_policy_update(node, message) != PublishResult::Published {
            // SAFETY: rejected/coalesced publication did not consume this
            // attempt's retained reference.
            unsafe { Arc::decrement_strong_count(pointer) };
            core.cancel_scheduler_inbox_delivery();
        }
        Ok(())
    }

    pub(super) fn publish_owner_affinity_retry(
        &self,
        core: &Arc<ThreadCore>,
        owner: CpuId,
        target: CpuId,
    ) -> Result<(), TaskError> {
        let remote = self
            .cpu_remote(owner)
            .ok_or(TaskError::CpuOffline(owner.as_u32()))?;
        if !core.reserve_scheduler_inbox_delivery() {
            return Ok(());
        }
        let pointer = Arc::as_ptr(core);
        // SAFETY: this count is transferred to the dedicated affinity node and
        // consumed by exactly one later owner drain.
        unsafe { Arc::increment_strong_count(pointer) };
        // SAFETY: the transferred Arc count pins the embedded control node.
        let node = unsafe { Pin::new_unchecked((*pointer).affinity_update_node()) };
        let message = InboxMessage::affinity_update_with_payload(
            core.id(),
            owner,
            target,
            pointer.expose_provenance(),
        );
        if remote.publish_policy_update(node, message) != PublishResult::Published {
            // SAFETY: rejected/coalesced publication did not consume this
            // attempt's retained reference.
            unsafe { Arc::decrement_strong_count(pointer) };
            core.cancel_scheduler_inbox_delivery();
        }
        Ok(())
    }

    /// Changes thread affinity after validating Deadline root-domain coverage.
    pub fn set_affinity(&self, thread: ThreadId, affinity: CpuSet) -> Result<(), TaskError> {
        validate_affinity(&affinity, self.config.cpu_count())?;
        let state = self.state.lock();
        let root_domain = self.root_domain.lock();
        let record = state.thread_record(thread)?;
        let core = Arc::clone(&record.core);
        let mut sched = record.sched.lock();
        if sched.lifecycle.state() == ThreadState::Exited {
            return Err(TaskError::NotReady);
        }
        let is_deadline = matches!(sched.active_base_policy, SchedulePolicy::Deadline(_))
            || matches!(sched.base_policy, SchedulePolicy::Deadline(_));
        if is_deadline && !affinity.covers(&root_domain.online) {
            return Err(TaskError::DeadlineAffinity);
        }
        let timer_cpu = core.sleep_timer_cpu();
        if timer_cpu.is_some_and(|cpu| !affinity.contains(cpu)) {
            return Err(TaskError::ActiveTimerAffinity);
        }
        let target = timer_cpu
            .or_else(|| state.select_allowed_cpu(&affinity))
            .ok_or(TaskError::InvalidConfiguration)?;
        sched.affinity = affinity;
        // The affinity mask is task metadata, but physical placement belongs
        // to one runqueue owner. A remote writer only publishes a reconciliation
        // request; it never rewrites Queued/Running/SwitchingOut in place.
        let owner = sched
            .placement
            .running_cpu()
            .or(sched.placement.queued_cpu())
            .or(sched.placement.on_cpu())
            .or(sched.placement.migration_target())
            .or(sched.deadline_bandwidth_cpu);
        let target = owner
            .filter(|owner| sched.affinity.contains(*owner))
            .unwrap_or(target);
        core.set_target_cpu(target);
        drop(sched);
        if let Some(owner) = owner {
            state.publish_affinity_update(&core, owner, target)?;
        }
        Ok(())
    }

    /// Updates the owner CPU's running thread without publishing a self inbox.
    ///
    /// The caller owns `cpu` in an IRQ-off scheduler-safe window. A `true`
    /// result means the current thread must schedule out before the operation
    /// can return to its caller; switch tail will publish the detached context
    /// to the selected destination CPU.
    pub fn set_current_affinity(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        affinity: CpuSet,
    ) -> Result<bool, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        validate_affinity(&affinity, self.config.cpu_count())?;
        let state = self.state.lock();
        let root_domain = self.root_domain.lock();
        state.ensure_cpu_online(&cpu)?;
        let current = cpu.current().ok_or(TaskError::NoRunnableThread)?;
        let record = state.thread_record(current)?;
        let mut sched = record.sched.lock();
        if sched.placement.running_cpu() != Some(cpu.owner())
            || sched.placement.on_cpu() != Some(cpu.owner())
        {
            return Err(TaskError::InvalidConfiguration);
        }
        let is_deadline = matches!(sched.active_base_policy, SchedulePolicy::Deadline(_))
            || matches!(sched.base_policy, SchedulePolicy::Deadline(_));
        if is_deadline && !affinity.covers(&root_domain.online) {
            return Err(TaskError::DeadlineAffinity);
        }
        let timer_cpu = record.core.sleep_timer_cpu();
        if timer_cpu.is_some_and(|timer_cpu| !affinity.contains(timer_cpu)) {
            return Err(TaskError::ActiveTimerAffinity);
        }
        let target = timer_cpu
            .or_else(|| state.select_allowed_cpu(&affinity))
            .ok_or(TaskError::InvalidConfiguration)?;
        let owner = cpu.owner();
        let must_migrate = !affinity.contains(owner);
        sched.affinity = affinity;
        sched
            .placement
            .set_migration_target(must_migrate.then_some(target))?;
        record
            .core
            .set_target_cpu(if must_migrate { target } else { owner });
        drop(sched);
        if must_migrate {
            cpu.request_reschedule();
        }
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        Ok(must_migrate)
    }
}
