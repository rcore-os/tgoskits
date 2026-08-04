//! Affinity updates and owner-to-owner placement delivery.

use super::*;

#[cfg(test)]
std::thread_local! {
    static DRAIN_MIGRATION_CPUS_BEFORE_PUBLICATION: core::cell::Cell<bool> = const {
        core::cell::Cell::new(false)
    };
}

#[cfg(test)]
pub(super) fn drain_next_migration_cpus_before_publication() {
    DRAIN_MIGRATION_CPUS_BEFORE_PUBLICATION.set(true);
}

#[cfg(test)]
pub(super) fn inject_migration_publication_race(system: &TaskSystem, source: CpuId, target: CpuId) {
    if !DRAIN_MIGRATION_CPUS_BEFORE_PUBLICATION.replace(false) {
        return;
    }
    for cpu in [target, source] {
        let remote = &system.cpu_remotes[cpu.as_usize()];
        if remote.try_deactivate() {
            assert!(remote.try_begin_draining());
        }
    }
}

/// Move-only owner-control carrier reserved before physical placement changes.
///
/// Holding the CPU publication lease prevents hotplug from crossing the
/// `Queued/Detached -> Migrating` transaction. Holding the thread delivery
/// lease gives `Migrating` a concrete lifetime owner before the source
/// runqueue entry is detached, matching Linux's `TASK_ON_RQ_MIGRATING`
/// invariant.
pub(super) struct PreparedOwnerMigration<'remote> {
    publication: Option<CpuRemotePublication<'remote>>,
    core: Option<Arc<ThreadCore>>,
    source: CpuId,
    inbox_cpu: CpuId,
}

impl PreparedOwnerMigration<'_> {
    pub(super) fn commit(mut self) {
        let publication = self
            .publication
            .take()
            .expect("prepared migration must retain its CPU publication lease");
        let core = self
            .core
            .take()
            .expect("prepared migration must retain its thread delivery lease");
        let thread = core.id();
        let pointer = Arc::into_raw(core);
        let node = unsafe {
            // SAFETY: the transferred Arc count keeps the embedded node pinned
            // until one owner drain reconstructs and releases the reference.
            Pin::new_unchecked((*pointer).migration_node())
        };
        let message = InboxMessage::migration_with_payload(
            thread,
            self.source,
            self.inbox_cpu,
            thread.generation() as u64,
            pointer.expose_provenance(),
        );
        match publication.publish_owner_control(node, message) {
            PublishResult::Published => {}
            PublishResult::AlreadyPending => unsafe {
                // SAFETY: a coalesced publication did not consume this Arc.
                // The older carrier remains responsible for observing the
                // latest generation-bearing placement state.
                let retained = Arc::from_raw(pointer);
                retained.cancel_scheduler_inbox_delivery();
                drop(retained);
            },
            PublishResult::WrongKind => unsafe {
                // SAFETY: the typed carrier and embedded migration node have
                // matching kinds, so rejection is an internal invariant.
                let retained = Arc::from_raw(pointer);
                retained.cancel_scheduler_inbox_delivery();
                drop(retained);
                task_runtime::fatal_invariant(0x4d49_4701, thread.as_u64() as usize);
            },
        }
    }
}

impl Drop for PreparedOwnerMigration<'_> {
    fn drop(&mut self) {
        if let Some(core) = self.core.take() {
            core.cancel_scheduler_inbox_delivery();
        }
    }
}

impl TaskSystem {
    pub(super) fn complete_affinity_if_satisfied_locked(
        core: &Arc<ThreadCore>,
        sched: &ThreadSchedState,
    ) -> bool {
        if sched.lifecycle.state() == ThreadState::Exited
            || sched.placement.migration_target().is_some()
        {
            return false;
        }
        let placement_is_allowed = [
            sched.placement.queued_cpu(),
            sched.placement.running_cpu(),
            sched.placement.on_cpu(),
        ]
        .into_iter()
        .flatten()
        .all(|cpu| sched.placement.affinity.contains(cpu));
        placement_is_allowed
            && core.publish_affinity_completion(sched.placement.affinity_generation)
    }

    pub(super) fn prepare_owner_migration(
        &self,
        core: &Arc<ThreadCore>,
        source: CpuId,
        target: CpuId,
    ) -> Result<PreparedOwnerMigration<'_>, TaskError> {
        let target_remote = self
            .cpu_remotes
            .get(target.as_usize())
            .ok_or(TaskError::InvalidCpu(target.as_u32()))?;
        let source_remote = self
            .cpu_remotes
            .get(source.as_usize())
            .ok_or(TaskError::InvalidCpu(source.as_u32()))?;
        let (inbox_cpu, publication) = target_remote
            .begin_publication()
            .map_or_else(
                || {
                    source_remote
                        .begin_owner_delivery()
                        .map(|publication| (source, publication))
                },
                |publication| Some((target, publication)),
            )
            .ok_or(TaskError::CpuOffline(source.as_u32()))?;
        if !core.reserve_scheduler_inbox_delivery() {
            return Err(TaskError::NotReady);
        }
        Ok(PreparedOwnerMigration {
            publication: Some(publication),
            core: Some(Arc::clone(core)),
            source,
            inbox_cpu,
        })
    }

    pub(super) fn deliver_owner_migration(
        &self,
        core: &Arc<ThreadCore>,
        source: CpuId,
        target: CpuId,
    ) -> Result<(), TaskError> {
        self.prepare_owner_migration(core, source, target)?.commit();
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
        if remote.publish_owner_control(node, message) != PublishResult::Published {
            // SAFETY: rejected/coalesced publication did not consume this
            // attempt's retained reference.
            unsafe { Arc::decrement_strong_count(pointer) };
            core.cancel_scheduler_inbox_delivery();
        }
        Ok(())
    }

    pub(super) fn publish_owner_deadline_refresh(
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
        // SAFETY: this count is transferred to the dedicated refresh node and
        // consumed by exactly one owner-side inbox drain.
        unsafe { Arc::increment_strong_count(pointer) };
        // SAFETY: the transferred Arc count pins the embedded refresh node.
        let node = unsafe { Pin::new_unchecked((*pointer).deadline_refresh_node()) };
        let message = InboxMessage::deadline_refresh_with_payload(
            core.id(),
            owner,
            generation,
            pointer.expose_provenance(),
        );
        if remote.publish_owner_control(node, message) != PublishResult::Published {
            // SAFETY: rejected/coalesced publication retained no extra count.
            unsafe { Arc::decrement_strong_count(pointer) };
            core.cancel_scheduler_inbox_delivery();
        }
        Ok(())
    }

    pub(super) fn publish_owner_deadline_refresh_reserved(
        &self,
        core: &Arc<ThreadCore>,
        owner: CpuId,
        generation: u64,
        publication: CpuRemotePublication<'_>,
    ) {
        if !core.reserve_scheduler_inbox_delivery() {
            return;
        }
        let pointer = Arc::as_ptr(core);
        // SAFETY: this count is transferred to the dedicated refresh node and
        // consumed by exactly one owner-side inbox drain.
        unsafe { Arc::increment_strong_count(pointer) };
        // SAFETY: the transferred Arc count pins the embedded refresh node.
        let node = unsafe { Pin::new_unchecked((*pointer).deadline_refresh_node()) };
        let message = InboxMessage::deadline_refresh_with_payload(
            core.id(),
            owner,
            generation,
            pointer.expose_provenance(),
        );
        if publication.publish_owner_control(node, message) != PublishResult::Published {
            // SAFETY: rejected/coalesced publication retained no extra count.
            unsafe { Arc::decrement_strong_count(pointer) };
            core.cancel_scheduler_inbox_delivery();
        }
    }

    pub(super) fn publish_owner_policy_reserved(
        &self,
        core: &Arc<ThreadCore>,
        owner: CpuId,
        generation: u64,
        publication: CpuRemotePublication<'_>,
    ) {
        if !core.reserve_scheduler_inbox_delivery() {
            return;
        }
        let pointer = Arc::as_ptr(core);
        // SAFETY: this count is transferred to the embedded inbox node and
        // consumed by the owner drain.
        unsafe { Arc::increment_strong_count(pointer) };
        // SAFETY: the transferred Arc count keeps the embedded node pinned.
        let node = unsafe { Pin::new_unchecked((*pointer).policy_update_node()) };
        let message = InboxMessage::policy_update_with_payload(
            core.id(),
            owner,
            generation,
            pointer.expose_provenance(),
        );
        if publication.publish_owner_control(node, message) != PublishResult::Published {
            // SAFETY: rejected/coalesced publication did not consume this
            // attempt's retained reference.
            unsafe { Arc::decrement_strong_count(pointer) };
            core.cancel_scheduler_inbox_delivery();
        }
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
        if remote.publish_owner_control(node, message) != PublishResult::Published {
            // SAFETY: rejected/coalesced publication did not consume this
            // attempt's retained reference.
            unsafe { Arc::decrement_strong_count(pointer) };
            core.cancel_scheduler_inbox_delivery();
        }
        Ok(())
    }

    /// Publishes one affinity generation and returns its completion owner.
    pub fn request_affinity(
        &self,
        thread: ThreadId,
        affinity: CpuSet,
    ) -> Result<ThreadAffinityChange, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        validate_affinity(&affinity, self.config.cpu_count())?;
        let state = self.state.lock();
        let root_domain = self.root_domain.lock();
        let record = state.thread_record(thread)?;
        let core = Arc::clone(&record.core);
        let mut sched = record.sched.lock();
        if sched.lifecycle.state() == ThreadState::Exited {
            return Err(TaskError::NotReady);
        }
        let is_deadline = matches!(sched.policy.applied, SchedulePolicy::Deadline(_))
            || matches!(sched.policy.requested, SchedulePolicy::Deadline(_));
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
        let generation = sched
            .placement
            .affinity_generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        sched.placement.affinity_generation = generation;
        sched.placement.affinity = affinity;
        // The affinity mask is task metadata, but physical placement belongs
        // to one runqueue owner. A remote writer only publishes a reconciliation
        // request; it never rewrites Queued/Running or the independent
        // switch-tail `on_cpu` publication in place.
        let owner = sched
            .placement
            .running_cpu()
            .or(sched.placement.queued_cpu())
            .or(sched.placement.on_cpu())
            .or(sched.placement.migration_target())
            .or(sched.deadline.bandwidth_cpu);
        let target = owner
            .filter(|owner| sched.placement.affinity.contains(*owner))
            .unwrap_or(target);
        core.set_wake_cpu_hint(target);
        let completed = Self::complete_affinity_if_satisfied_locked(&core, &sched);
        drop(sched);
        let publication = owner.map_or(Ok(()), |owner| {
            state.publish_affinity_update(&core, owner, target)
        });
        drop(root_domain);
        drop(state);
        if completed {
            core.notify_affinity_waiters();
        }
        publication?;
        Ok(ThreadAffinityChange::new(core, generation))
    }

    /// Changes thread affinity after validating Deadline root-domain coverage.
    pub fn set_affinity(&self, thread: ThreadId, affinity: CpuSet) -> Result<(), TaskError> {
        self.request_affinity(thread, affinity).map(drop)
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
        let is_deadline = matches!(sched.policy.applied, SchedulePolicy::Deadline(_))
            || matches!(sched.policy.requested, SchedulePolicy::Deadline(_));
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
        let generation = sched
            .placement
            .affinity_generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        sched.placement.affinity_generation = generation;
        sched.placement.affinity = affinity;
        sched
            .placement
            .set_migration_target(must_migrate.then_some(target))?;
        record
            .core
            .set_wake_cpu_hint(if must_migrate { target } else { owner });
        let completed = Self::complete_affinity_if_satisfied_locked(&record.core, &sched);
        let core = Arc::clone(&record.core);
        drop(sched);
        drop(root_domain);
        drop(state);
        if completed {
            core.notify_affinity_waiters();
        }
        if must_migrate {
            cpu.request_reschedule();
        }
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        Ok(must_migrate)
    }
}
