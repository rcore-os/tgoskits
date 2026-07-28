//! Generation-checked thread registry and deferred teardown state.

use super::*;

#[cfg(test)]
static PI_DONOR_RECORD_VISITS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
pub(super) fn reset_pi_donor_record_visits() {
    PI_DONOR_RECORD_VISITS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
pub(super) fn pi_donor_record_visits() -> usize {
    PI_DONOR_RECORD_VISITS.load(Ordering::Relaxed)
}

#[derive(Debug)]
pub(super) struct TaskSystemState {
    pub(super) cpus: Vec<CpuRegistration>,
    pub(super) slots: Vec<ThreadSlot>,
    pub(super) free_slots: Vec<u32>,
    pub(super) pending_resource_releases: Vec<PendingResourceRelease>,
    pub(super) task_work_class_cursor: DeferredTaskWorkClass,
    pub(super) thread_release_first: bool,
    pub(super) deadline_callback_cursor: usize,
    pub(super) exit_callback_cursor: usize,
    pub(super) reap_cursor: usize,
    pub(super) deadline_admission: DeadlineAdmission,
}

/// Proof that every fallible read required by one PI-chain recomputation
/// succeeded while the task-system registry lock was held.
#[derive(Clone, Copy, Debug)]
pub(super) struct PiRecomputeProof {
    start: ThreadId,
}

impl PiRecomputeProof {
    pub(super) const fn start(self) -> ThreadId {
        self.start
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PiWaiterCursor {
    owner: ThreadId,
    previous: Option<ThreadId>,
    next: Option<ThreadId>,
    remaining: usize,
}

impl TaskSystemState {
    pub(super) fn claim_pending_deadline_overrun(
        &mut self,
    ) -> Option<Option<(ThreadExtensionView, ThreadId)>> {
        let slot_count = self.slots.len();
        if slot_count == 0 {
            return None;
        }
        let start = self.deadline_callback_cursor % slot_count;
        for offset in 0..slot_count {
            let index = (start + offset) % slot_count;
            let Some(record) = self.slots[index].record.as_mut() else {
                continue;
            };
            let mut sched = record.sched.lock();
            if sched.deadline_overrun_events == 0 || record.deadline_callback_claimed {
                continue;
            }
            sched.deadline_overrun_events -= 1;
            self.deadline_callback_cursor = (index + 1) % slot_count;
            return Some(record.extension.as_ref().map(|extension| {
                record.deadline_callback_claimed = true;
                (extension.as_view(), record.core.id())
            }));
        }
        None
    }

    pub(super) fn reserve_deadline(
        &mut self,
        policy: SchedulePolicy,
        affinity: &CpuSet,
        online: &CpuSet,
    ) -> Result<u128, TaskError> {
        match policy {
            SchedulePolicy::Deadline(deadline) => {
                if !affinity.covers(online) {
                    return Err(TaskError::DeadlineAffinity);
                }
                self.deadline_admission.reserve(deadline)
            }
            _ => Ok(0),
        }
    }

    pub(super) fn deadline_reservation_for(
        &self,
        policy: SchedulePolicy,
        affinity: &CpuSet,
        online: &CpuSet,
    ) -> Result<u128, TaskError> {
        match policy {
            SchedulePolicy::Deadline(deadline) => {
                if !affinity.covers(online) {
                    return Err(TaskError::DeadlineAffinity);
                }
                Ok(DeadlineAdmission::utilization(deadline))
            }
            _ => Ok(0),
        }
    }

    pub(super) fn allocate_thread_slot(&mut self) -> Result<(u32, u32), TaskError> {
        if let Some(slot) = self.free_slots.pop() {
            Ok((slot, self.slots[slot as usize].generation))
        } else {
            let slot =
                u32::try_from(self.slots.len()).map_err(|_| TaskError::InvalidConfiguration)?;
            self.slots.push(ThreadSlot {
                generation: 1,
                record: None,
            });
            Ok((slot, 1))
        }
    }

    pub(super) fn thread_record(&self, thread: ThreadId) -> Result<&ThreadRecord, TaskError> {
        let slot = self
            .slots
            .get(thread.slot() as usize)
            .ok_or(TaskError::StaleThreadId)?;
        if slot.generation != thread.generation() {
            return Err(TaskError::StaleThreadId);
        }
        slot.record.as_ref().ok_or(TaskError::StaleThreadId)
    }

    pub(super) fn thread_record_mut(
        &mut self,
        thread: ThreadId,
    ) -> Result<&mut ThreadRecord, TaskError> {
        let slot = self
            .slots
            .get_mut(thread.slot() as usize)
            .ok_or(TaskError::StaleThreadId)?;
        if slot.generation != thread.generation() {
            return Err(TaskError::StaleThreadId);
        }
        slot.record.as_mut().ok_or(TaskError::StaleThreadId)
    }

    pub(super) fn pi_waiter_cursor(&self, owner: ThreadId) -> Result<PiWaiterCursor, TaskError> {
        Ok(PiWaiterCursor {
            owner,
            previous: None,
            next: self.thread_record(owner)?.pi_waiter_head,
            remaining: self.slots.len(),
        })
    }

    pub(super) fn next_pi_waiter(
        &self,
        cursor: &mut PiWaiterCursor,
    ) -> Result<Option<(ThreadId, PiWaitRegistration)>, TaskError> {
        let Some(waiter) = cursor.next else {
            return Ok(None);
        };
        if cursor.remaining == 0 {
            return Err(TaskError::PiCycle);
        }
        #[cfg(test)]
        PI_DONOR_RECORD_VISITS.fetch_add(1, Ordering::Relaxed);
        let registration = self
            .thread_record(waiter)?
            .blocked_on
            .ok_or(TaskError::InvalidPiState)?;
        if registration.owner != cursor.owner || registration.owner_prev != cursor.previous {
            return Err(TaskError::InvalidPiState);
        }
        cursor.previous = Some(waiter);
        cursor.next = registration.owner_next;
        cursor.remaining -= 1;
        Ok(Some((waiter, registration)))
    }

    pub(super) fn cpu_registration(&self, cpu: CpuId) -> Result<&CpuRegistration, TaskError> {
        self.cpus
            .get(cpu.as_usize())
            .ok_or(TaskError::InvalidCpu(cpu.as_u32()))
    }

    pub(super) fn cpu_registration_mut(
        &mut self,
        cpu: CpuId,
    ) -> Result<&mut CpuRegistration, TaskError> {
        self.cpus
            .get_mut(cpu.as_usize())
            .ok_or(TaskError::InvalidCpu(cpu.as_u32()))
    }

    pub(super) fn ensure_cpu_online(&self, cpu: &CpuLocal) -> Result<(), TaskError> {
        let registration = self.cpu_registration(cpu.owner())?;
        if registration.online && cpu.is_online() {
            Ok(())
        } else {
            Err(TaskError::CpuOffline(cpu.owner().as_u32()))
        }
    }

    pub(super) fn online_cpu_count(&self) -> usize {
        self.cpus.iter().filter(|cpu| cpu.online).count()
    }

    pub(super) fn release_deadline_reservation_on_exit(
        &mut self,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        let held = {
            let record = self.thread_record(thread)?;
            let mut sched = record.sched.lock();
            let held = sched
                .active_deadline_reservation
                .max(sched.desired_deadline_reservation);
            sched.active_deadline_reservation = 0;
            sched.desired_deadline_reservation = 0;
            held
        };
        self.deadline_admission.release(u128::from(held));
        Ok(())
    }

    pub(super) fn remove_exited_thread(
        &mut self,
        thread: ThreadId,
    ) -> Result<ThreadRecord, TaskError> {
        self.remove_exited_thread_with_lease_count(thread, 0, None)
    }

    pub(super) fn remove_unpublished_thread_with_handle(
        &mut self,
        handle: &ThreadHandle,
    ) -> Result<ThreadRecord, TaskError> {
        let thread = handle.id();
        let slot_index = thread.slot() as usize;
        let slot = self
            .slots
            .get_mut(slot_index)
            .ok_or(TaskError::StaleThreadId)?;
        if slot.generation != thread.generation() {
            return Err(TaskError::StaleThreadId);
        }
        let record = slot.record.as_ref().ok_or(TaskError::StaleThreadId)?;
        if !core::ptr::eq(Arc::as_ptr(&record.core), Arc::as_ptr(&handle.core)) {
            return Err(TaskError::StaleThreadId);
        }
        let held = {
            let sched = record.sched.lock();
            if sched.lifecycle.state() == ThreadState::Exited
                || sched.placement.queued_cpu().is_some()
                || sched.placement.running_cpu().is_some()
                || sched.placement.on_cpu().is_some()
                || sched.placement.migration_target().is_some()
                || sched.deadline_bandwidth_cpu.is_some()
                || sched.deadline_cleanup_pending
                || sched.deadline_cbs_borrower.is_some()
                || record.blocked_on.is_some()
                || record.exit_callback_pending
                || record.exit_callback_claimed
                || record.deadline_callback_claimed
                || record.core.scheduler_inbox_delivery_count() != 0
                || record.core.sleep_timer_cpu().is_some()
                || record.core.external_lease_count() != 1
            {
                return Err(TaskError::ThreadBusy);
            }
            sched
                .active_deadline_reservation
                .max(sched.desired_deadline_reservation)
        };
        let record = slot.record.take().ok_or(TaskError::StaleThreadId)?;
        self.deadline_admission.release(u128::from(held));
        if advance_thread_slot_generation(slot) {
            self.free_slots.push(thread.slot());
        }
        Ok(record)
    }

    pub(super) fn remove_exited_thread_with_lease_count(
        &mut self,
        thread: ThreadId,
        expected_external_leases: usize,
        expected_core: Option<*const ThreadCore>,
    ) -> Result<ThreadRecord, TaskError> {
        let slot_index = thread.slot() as usize;
        let slot = self
            .slots
            .get_mut(slot_index)
            .ok_or(TaskError::StaleThreadId)?;
        if slot.generation != thread.generation() {
            return Err(TaskError::StaleThreadId);
        }
        let record = slot.record.as_ref().ok_or(TaskError::StaleThreadId)?;
        if !record.core.try_claim_reap() {
            return Err(TaskError::ThreadBusy);
        }
        let validation = (|| {
            let sched = record.sched.lock();
            if sched.lifecycle.state() != ThreadState::Exited {
                return Err(TaskError::NotExited);
            }
            // Exit closes the scheduler activity gate before publishing
            // `Exited`, so no producer can increment the delivery count after
            // the Acquire observation of zero. A non-zero count owns both one
            // raw inbox Arc and access to scheduler-owned thread state.
            if sched.placement.on_cpu().is_some()
                || sched.placement.migration_target().is_some()
                || sched.deadline_bandwidth_cpu.is_some()
                || sched.deadline_cleanup_pending
                || sched.deadline_cbs_borrower.is_some()
                || sched.deadline_overrun_events != 0
                || record.deadline_callback_claimed
                || record.exit_callback_pending
                || record.exit_callback_claimed
                || record.core.scheduler_inbox_delivery_count() != 0
            {
                return Err(TaskError::ThreadBusy);
            }
            if record.core.sleep_timer_cpu().is_some() {
                // The owner CPU's timer heap still contains a raw pointer to the
                // embedded node. Expiry/cancel must physically detach it before
                // this Arc allocation can be released.
                return Err(TaskError::ThreadBusy);
            }
            if expected_core.is_some_and(|core| !core::ptr::eq(core, Arc::as_ptr(&record.core))) {
                return Err(TaskError::StaleThreadId);
            }
            if record.core.external_lease_count() != expected_external_leases {
                return Err(TaskError::ThreadBusy);
            }
            Ok(())
        })();
        if let Err(error) = validation {
            record.core.cancel_reap_claim();
            return Err(error);
        }
        let record = slot.record.take().ok_or(TaskError::StaleThreadId)?;
        let held = {
            let sched = record.sched.lock();
            sched
                .active_deadline_reservation
                .max(sched.desired_deadline_reservation)
        };
        self.deadline_admission.release(u128::from(held));
        if advance_thread_slot_generation(slot) {
            self.free_slots.push(thread.slot());
        }
        Ok(record)
    }

    pub(super) fn remove_exited_thread_with_handle(
        &mut self,
        handle: &ThreadHandle,
    ) -> Result<ThreadRecord, TaskError> {
        self.remove_exited_thread_with_lease_count(handle.id(), 1, Some(Arc::as_ptr(&handle.core)))
    }

    pub(super) fn take_unreferenced_exited(&mut self) -> Result<Option<ThreadRecord>, TaskError> {
        let slot_count = self.slots.len();
        if slot_count == 0 {
            return Ok(None);
        }
        let start = self.reap_cursor % slot_count;
        for offset in 0..slot_count {
            let index = (start + offset) % slot_count;
            let thread = {
                let slot = &self.slots[index];
                let Some(record) = slot.record.as_ref() else {
                    continue;
                };
                let sched = record.sched.lock();
                if sched.lifecycle.state() != ThreadState::Exited
                    || sched.placement.on_cpu().is_some()
                    || sched.placement.migration_target().is_some()
                    || sched.deadline_bandwidth_cpu.is_some()
                    || sched.deadline_cleanup_pending
                    || sched.deadline_cbs_borrower.is_some()
                    || sched.deadline_overrun_events != 0
                    || record.deadline_callback_claimed
                    || record.exit_callback_pending
                    || record.exit_callback_claimed
                    || record.core.scheduler_inbox_delivery_count() != 0
                    || record.core.sleep_timer_cpu().is_some()
                {
                    continue;
                }
                let slot_index = u32::try_from(index)
                    .expect("thread registry slot must fit the ThreadId representation");
                ThreadId::from_parts(slot_index, slot.generation)
            };
            match self.remove_exited_thread_with_lease_count(thread, 0, None) {
                Ok(record) => {
                    self.reap_cursor = (index + 1) % slot_count;
                    return Ok(Some(record));
                }
                Err(TaskError::ThreadBusy) => continue,
                Err(error) => return Err(error),
            }
        }
        self.reap_cursor = (start + 1) % slot_count;
        Ok(None)
    }

    pub(super) fn claim_pending_exit_callback(
        &mut self,
    ) -> Result<Option<(ThreadExtensionView, ThreadId)>, TaskError> {
        let slot_count = self.slots.len();
        if slot_count == 0 {
            return Ok(None);
        }
        let start = self.exit_callback_cursor % slot_count;
        for offset in 0..slot_count {
            let index = (start + offset) % slot_count;
            let slot = &mut self.slots[index];
            let Some(record) = slot.record.as_mut() else {
                continue;
            };
            let sched = record.sched.lock();
            if sched.lifecycle.state() != ThreadState::Exited
                || sched.placement.on_cpu().is_some()
                || sched.deadline_overrun_events != 0
                || record.deadline_callback_claimed
                || !record.exit_callback_pending
                || record.exit_callback_claimed
            {
                continue;
            }
            let extension = record
                .extension
                .as_ref()
                .ok_or(TaskError::InvalidConfiguration)?
                .as_view();
            record.exit_callback_claimed = true;
            let slot_index = u32::try_from(index).map_err(|_| TaskError::InvalidConfiguration)?;
            self.exit_callback_cursor = (index + 1) % slot_count;
            return Ok(Some((
                extension,
                ThreadId::from_parts(slot_index, slot.generation),
            )));
        }
        self.exit_callback_cursor = (start + 1) % slot_count;
        Ok(None)
    }

    pub(super) fn finish_exit_callback(&mut self, thread: ThreadId) -> Result<(), TaskError> {
        let record = self.thread_record_mut(thread)?;
        let sched = record.sched.lock();
        if sched.lifecycle.state() != ThreadState::Exited
            || sched.placement.on_cpu().is_some()
            || !record.exit_callback_pending
            || !record.exit_callback_claimed
        {
            return Err(TaskError::InvalidConfiguration);
        }
        record.exit_callback_pending = false;
        record.exit_callback_claimed = false;
        Ok(())
    }

    pub(super) fn finish_deadline_callback(&mut self, thread: ThreadId) -> Result<(), TaskError> {
        let record = self.thread_record_mut(thread)?;
        if !record.deadline_callback_claimed {
            return Err(TaskError::InvalidConfiguration);
        }
        record.deadline_callback_claimed = false;
        Ok(())
    }

    pub(super) fn ensure_pi_acyclic(
        &self,
        waiter: ThreadId,
        mut owner: ThreadId,
    ) -> Result<(), TaskError> {
        for _ in 0..self.slots.len().saturating_add(1) {
            if owner == waiter {
                return Err(TaskError::PiCycle);
            }
            let Some(registration) = self.thread_record(owner)?.blocked_on else {
                return Ok(());
            };
            owner = registration.owner;
        }
        Err(TaskError::PiCycle)
    }

    pub(super) fn select_allowed_cpu(&self, affinity: &CpuSet) -> Option<CpuId> {
        self.cpus
            .iter()
            .enumerate()
            .filter(|(index, registration)| {
                registration.online && affinity.contains(CpuId::new(*index as u32))
            })
            .filter_map(|(index, registration)| {
                let cpu = CpuId::new(index as u32);
                registration
                    .remote
                    .is_online()
                    .then_some(cpu)
                    .and_then(|cpu| {
                        registration
                            .remote
                            .try_runnable_summary()
                            .map(|runnable| (runnable, cpu))
                    })
            })
            .min_by_key(|(load, cpu)| (*load, cpu.as_u32()))
            .map(|(_, cpu)| cpu)
    }

    pub(super) fn publish_affinity_update(
        &self,
        core: &Arc<ThreadCore>,
        owner: CpuId,
        target: CpuId,
    ) -> Result<(), TaskError> {
        let cpu_local = self
            .cpu_remote(owner)
            .ok_or(TaskError::CpuOffline(owner.as_u32()))?;
        if !core.reserve_scheduler_inbox_delivery() {
            return Ok(());
        }
        let pointer = Arc::as_ptr(core);
        // SAFETY: the retained count is transferred to the intrusive affinity
        // reconciliation request and consumed by one owner drain.
        unsafe { Arc::increment_strong_count(pointer) };
        // SAFETY: the retained Arc count pins this dedicated control node.
        let node = unsafe { Pin::new_unchecked((*pointer).affinity_update_node()) };
        let message = InboxMessage::affinity_update_with_payload(
            core.id(),
            owner,
            target,
            pointer.expose_provenance(),
        );
        let result = cpu_local.publish_policy_update(node, message);
        if result != PublishResult::Published {
            // SAFETY: a rejected/coalesced publication did not consume this
            // attempt's retained reference.
            unsafe { Arc::decrement_strong_count(pointer) };
            core.cancel_scheduler_inbox_delivery();
        }
        Ok(())
    }

    pub(super) fn publish_migration_to(
        &self,
        core: &Arc<ThreadCore>,
        inbox_cpu: CpuId,
        source: CpuId,
        target: CpuId,
    ) -> Result<(), TaskError> {
        let cpu_local = self
            .cpu_remote(inbox_cpu)
            .ok_or(TaskError::CpuOffline(inbox_cpu.as_u32()))?;
        if !core.reserve_scheduler_inbox_delivery() {
            return Ok(());
        }
        let pointer = Arc::as_ptr(core);
        // SAFETY: the retained count is transferred to the intrusive inbox
        // message and released by exactly one owner drain.
        unsafe { Arc::increment_strong_count(pointer) };
        // SAFETY: Arc allocation addresses are stable and the retained count
        // keeps the embedded migration node alive while queued.
        let node = unsafe { Pin::new_unchecked((*pointer).migration_node()) };
        let message = InboxMessage::migration_with_payload(
            core.id(),
            source,
            target,
            core.id().generation() as u64,
            pointer.expose_provenance(),
        );
        let result = cpu_local.publish_migration(node, message);
        if result != PublishResult::Published {
            // SAFETY: a rejected/coalesced publication did not consume this
            // attempt's retained reference.
            unsafe { Arc::decrement_strong_count(pointer) };
            core.cancel_scheduler_inbox_delivery();
        }
        Ok(())
    }

    pub(super) fn request_owner_reschedule(&self, owner: ThreadId) {
        if let Ok(record) = self.thread_record(owner) {
            let (cpu, generation) = {
                let sched = record.sched.lock();
                (
                    sched
                        .placement
                        .running_cpu()
                        .or(sched.placement.queued_cpu())
                        .or(sched.deadline_bandwidth_cpu),
                    sched.policy_generation,
                )
            };
            let Some(cpu) = cpu else {
                return;
            };
            let core = Arc::as_ptr(&record.core);
            let Some(cpu_local) = self.cpu_remote(cpu) else {
                return;
            };
            if !record.core.reserve_scheduler_inbox_delivery() {
                return;
            }
            // SAFETY: this retained Arc count is transferred to the embedded
            // policy-update node and released by the owner drain.
            unsafe { Arc::increment_strong_count(core) };
            // SAFETY: the retained Arc count keeps this embedded node pinned.
            let node = unsafe { Pin::new_unchecked((*core).policy_update_node()) };
            let message = InboxMessage::policy_update_with_payload(
                owner,
                cpu,
                generation,
                core.expose_provenance(),
            );
            let result = cpu_local.publish_policy_update(node, message);
            if result != PublishResult::Published {
                // SAFETY: rejected/coalesced publication did not consume the
                // retained count allocated for this attempt.
                unsafe { Arc::decrement_strong_count(core) };
                record.core.cancel_scheduler_inbox_delivery();
            }
        }
    }

    pub(super) fn validate_pi_donor(&self, waiter: ThreadId) -> Result<(), TaskError> {
        let record = self.thread_record(waiter)?;
        let (policy, donor) = {
            let sched = record.sched.lock();
            (sched.policy, sched.pi_donor.unwrap_or(waiter))
        };
        if matches!(policy, SchedulePolicy::Deadline(_))
            && self
                .thread_record(donor)?
                .sched
                .lock()
                .base_deadline
                .is_none()
        {
            return Err(TaskError::InvalidPiState);
        }
        Ok(())
    }

    pub(super) fn prepare_pi_recompute_chain(
        &self,
        start: ThreadId,
    ) -> Result<PiRecomputeProof, TaskError> {
        let mut current = start;
        for _ in 0..=self.slots.len() {
            let record = self.thread_record(current)?;
            let (blocked_on, dispatch_generation) = {
                let sched = record.sched.lock();
                (record.blocked_on, sched.dispatch_generation)
            };
            if dispatch_generation == u64::MAX {
                return Err(TaskError::InvalidConfiguration);
            }
            let mut waiter_count = 0;
            let mut cursor = self.pi_waiter_cursor(current)?;
            while let Some((waiter, _)) = self.next_pi_waiter(&mut cursor)? {
                self.validate_pi_donor(waiter)?;
                waiter_count += 1;
            }
            if waiter_count != self.thread_record(current)?.sched.lock().blocked_pi_waiters {
                return Err(TaskError::InvalidPiState);
            }
            let Some(registration) = blocked_on else {
                return Ok(PiRecomputeProof { start });
            };
            current = registration.owner;
        }
        Err(TaskError::PiCycle)
    }

    pub(super) fn apply_pi_recompute_chain(&mut self, proof: PiRecomputeProof, fair_slice_ns: u64) {
        let mut current = proof.start();
        for _ in 0..=self.slots.len() {
            let (
                current_core,
                base,
                base_entity,
                blocked_on,
                previous_policy,
                previous_entity,
                previous_pi_donor,
                previous_deadline_donor,
                blocked_pi_waiters,
                previous_pi_critical_rescue,
                previous_dispatch_generation,
            ) = {
                let record = self
                    .thread_record(current)
                    .expect("prepared PI chain must retain every thread record");
                let sched = record.sched.lock();
                let base_entity = sched
                    .base_deadline
                    .filter(|_| matches!(sched.active_base_policy, SchedulePolicy::Deadline(_)))
                    .map(SchedulingEntity::Deadline)
                    .unwrap_or(sched.base_entity);
                (
                    Arc::clone(&record.core),
                    sched.active_base_policy,
                    base_entity,
                    record.blocked_on,
                    sched.policy,
                    sched.entity,
                    sched.pi_donor,
                    sched.deadline_donor,
                    sched.blocked_pi_waiters,
                    sched.pi_critical_rescue,
                    sched.dispatch_generation,
                )
            };
            let mut effective = base;
            let mut effective_entity = base_entity;
            let mut effective_urgency = base_entity.scheduling_urgency(base);
            let mut pi_donor = None;
            let mut deadline_donor = None;
            let mut cursor = self
                .pi_waiter_cursor(current)
                .expect("prepared PI owner must retain its waiter list");
            while let Some((waiter, _)) = self
                .next_pi_waiter(&mut cursor)
                .expect("prepared PI waiter list must remain linked")
            {
                let donor_record = self
                    .thread_record(waiter)
                    .expect("prepared PI waiter must retain its thread record");
                let (donor_policy, donor) = {
                    let sched = donor_record.sched.lock();
                    (sched.policy, sched.pi_donor.unwrap_or(waiter))
                };
                let donor_entity = if matches!(donor_policy, SchedulePolicy::Deadline(_)) {
                    self.thread_record(donor)
                        .expect("prepared PI donor must retain its thread record")
                        .sched
                        .lock()
                        .base_deadline
                        .map(SchedulingEntity::Deadline)
                        .expect("prepared Deadline PI donor must retain its entity")
                } else if previous_pi_donor == Some(donor)
                    && previous_policy == donor_policy
                    && previous_entity.matches_policy(donor_policy)
                {
                    previous_entity
                } else {
                    let virtual_time = base_entity.fair().map_or(0, |fair| fair.vruntime());
                    SchedulingEntity::new(donor_policy, fair_slice_ns, virtual_time)
                };
                let donor_urgency = donor_entity.scheduling_urgency(donor_policy);
                if donor_urgency < effective_urgency {
                    effective = donor_policy;
                    effective_entity = donor_entity;
                    effective_urgency = donor_urgency;
                    pi_donor = Some(donor);
                    deadline_donor =
                        matches!(donor_policy, SchedulePolicy::Deadline(_)).then_some(donor);
                }
            }
            let changed = previous_policy != effective
                || previous_pi_donor != pi_donor
                || previous_deadline_donor != deadline_donor;
            let deadline_donor_core = deadline_donor.map(|donor| {
                Arc::downgrade(
                    &self
                        .thread_record(donor)
                        .expect("prepared PI donor must retain its thread record")
                        .core,
                )
            });
            let should_rescue = blocked_pi_waiters != 0
                && effective_entity
                    .deadline()
                    .is_some_and(|deadline| deadline.remaining_runtime_ns() == 0);
            let rescue_changed = should_rescue != previous_pi_critical_rescue;
            let next_dispatch_generation = if changed || rescue_changed {
                Some(
                    previous_dispatch_generation
                        .checked_add(1)
                        .expect("prepared PI dispatch generation must not overflow"),
                )
            } else {
                None
            };
            let (rescue_changed, policy, entity) = {
                let mut sched = current_core.sched().lock();
                if changed {
                    sched.policy = effective;
                    sched.pi_donor = pi_donor;
                    sched.deadline_donor = deadline_donor;
                    sched.deadline_donor_core = deadline_donor_core;
                    sched.entity = effective_entity;
                }
                if rescue_changed {
                    sched.pi_critical_rescue = should_rescue;
                    if should_rescue {
                        sched.entity.enter_pi_critical_rescue();
                    } else {
                        sched.entity.leave_pi_critical_rescue();
                    }
                    if !sched.is_pi_boosted() {
                        sched.base_entity = sched.entity;
                        if let SchedulingEntity::Deadline(deadline) = sched.entity {
                            sched.base_deadline = Some(deadline);
                        }
                    }
                }
                if let Some(generation) = next_dispatch_generation {
                    sched.dispatch_generation = generation;
                }
                (rescue_changed, sched.policy, sched.entity)
            };
            if changed || rescue_changed {
                current_core.publish_effective_schedule(policy, entity);
                self.request_owner_reschedule(current);
            }
            let Some(registration) = blocked_on else {
                return;
            };
            current = registration.owner;
        }
        unreachable!("prepared PI chain must be acyclic");
    }

    pub(super) fn cpu_remote(&self, cpu: CpuId) -> Option<&CpuRemote> {
        let registration = self.cpu_registration(cpu).ok()?;
        if !registration.online || !registration.remote.is_online() {
            return None;
        }
        Some(registration.remote.as_ref())
    }
}

#[derive(Debug)]
pub(super) struct CpuRegistration {
    pub(super) online: bool,
    pub(super) remote: Arc<CpuRemote>,
}

#[derive(Debug)]
pub(super) struct ThreadSlot {
    pub(super) generation: u32,
    pub(super) record: Option<ThreadRecord>,
}

#[derive(Debug)]
pub(super) struct ThreadRecord {
    pub(super) core: Arc<ThreadCore>,
    pub(super) sched: Arc<ThreadSchedCell>,
    // Keep the fallback field drop order aligned with the normal reaper.
    pub(super) resources: ThreadResources,
    pub(super) extension: Option<ThreadExtension>,
    pub(super) blocked_on: Option<PiWaitRegistration>,
    pub(super) pi_waiter_head: Option<ThreadId>,
    pub(super) exit_callback_pending: bool,
    pub(super) exit_callback_claimed: bool,
    pub(super) deadline_callback_claimed: bool,
}

#[derive(Debug)]
pub(super) struct DetachedThreadRecord {
    resources: ThreadResources,
    extension: Option<ThreadExtension>,
}

impl DetachedThreadRecord {
    pub(super) const fn new(
        resources: ThreadResources,
        extension: Option<ThreadExtension>,
    ) -> Self {
        Self {
            resources,
            extension,
        }
    }

    pub(super) fn into_owned_parts(self) -> (Option<ThreadExtension>, ThreadResources) {
        let Self {
            resources,
            extension,
        } = self;
        (extension, resources)
    }

    pub(super) fn try_release_resources(&mut self) -> Result<(), TaskError> {
        self.resources.try_release()
    }

    pub(super) fn finish_release(mut self) {
        drop(self.extension.take());
    }
}

#[derive(Debug)]
pub(super) enum PendingResourceRelease {
    /// A reaped thread whose extension must outlive its runtime context.
    Thread(ThreadRecord),
    /// A construction transaction that failed before registry publication.
    Detached(DetachedThreadRecord),
}

impl PendingResourceRelease {
    pub(super) fn resources_mut(&mut self) -> &mut ThreadResources {
        match self {
            Self::Thread(record) => &mut record.resources,
            Self::Detached(record) => &mut record.resources,
        }
    }

    pub(super) fn finish(self) {
        match self {
            Self::Thread(mut record) => drop(record.extension.take()),
            Self::Detached(record) => record.finish_release(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PiWaitRegistration {
    pub(super) lock: PiLockId,
    pub(super) owner: ThreadId,
    pub(super) generation: u64,
    pub(super) owner_prev: Option<ThreadId>,
    pub(super) owner_next: Option<ThreadId>,
}

impl ThreadRecord {
    pub(super) fn has_live_pi_edges(&self) -> bool {
        self.blocked_on.is_some()
            || self.pi_waiter_head.is_some()
            || self.sched.lock().blocked_pi_waiters != 0
    }
}
