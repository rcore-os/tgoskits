//! Generation-checked thread registry and deferred teardown state.

use super::*;

#[derive(Debug)]
pub(super) struct TaskSystemState {
    pub(super) cpus: Vec<CpuRegistration>,
    pub(super) slots: Vec<ThreadSlot>,
    pub(super) free_slots: Vec<u32>,
    pub(super) pending_address_space_reclaims: Vec<crate::runtime::AddressSpaceToken>,
    pub(super) task_work_class_cursor: DeferredTaskWorkClass,
    pub(super) address_space_reclaim_first: bool,
    pub(super) exited_work: ExitedThreadWork,
}

pub(super) enum DeadlineCallbackClaim {
    NoCallback {
        has_more: bool,
    },
    Callback {
        extension: ThreadExtensionView,
        thread: ThreadId,
    },
}

impl TaskSystemState {
    pub(super) fn claim_pending_deadline_overrun(
        &mut self,
        thread: ThreadId,
    ) -> Result<DeadlineCallbackClaim, TaskError> {
        let record = self.thread_record_mut(thread)?;
        let mut sched = record.sched.lock();
        if sched.deadline.overrun_events == 0 {
            return Err(TaskError::InvalidConfiguration);
        }
        sched.deadline.overrun_events -= 1;
        let has_more = sched.deadline.overrun_events != 0;
        let callback = record.extension.as_ref().map(ThreadExtension::as_view);
        if let Some(extension) = callback {
            record.callbacks.claim_deadline()?;
            Ok(DeadlineCallbackClaim::Callback { extension, thread })
        } else {
            Ok(DeadlineCallbackClaim::NoCallback { has_more })
        }
    }

    pub(super) fn allocate_thread_slot(
        &mut self,
        thread_capacity: usize,
    ) -> Result<(u32, u32), TaskError> {
        if let Some(slot) = self.free_slots.pop() {
            let reusable = &self.slots[slot as usize];
            assert!(reusable.record.is_none());
            assert_eq!(reusable.pending_deadline_reservation, 0);
            Ok((slot, reusable.generation))
        } else {
            if self.slots.len() == thread_capacity {
                return Err(TaskError::ThreadCapacity);
            }
            let slot =
                u32::try_from(self.slots.len()).map_err(|_| TaskError::InvalidConfiguration)?;
            let required_capacity = self.slots.len().saturating_add(1);
            self.exited_work.reserve_slot_capacity(required_capacity);
            self.slots.push(ThreadSlot {
                generation: 1,
                record: None,
                pending_deadline_reservation: 0,
            });
            Ok((slot, 1))
        }
    }

    pub(super) fn deadline_bandwidth_rebuild(
        &self,
        online_cpus: usize,
    ) -> Result<DeadlineBandwidthRebuild, TaskError> {
        let online_cpus =
            u32::try_from(online_cpus).map_err(|_| TaskError::InvalidConfiguration)?;
        let divisor = core::num::NonZeroU64::new(u64::from(online_cpus));
        let mut reserved_scaled = 0_u64;
        let mut distributed_scaled = 0_u64;
        for slot in &self.slots {
            let held = if let Some(record) = &slot.record {
                let sched = record.sched.lock();
                sched.held_deadline_reservation()
            } else {
                slot.pending_deadline_reservation
            };
            reserved_scaled = reserved_scaled
                .checked_add(held)
                .ok_or(TaskError::InvalidConfiguration)?;
            if let Some(divisor) = divisor {
                distributed_scaled = distributed_scaled
                    .checked_add(held / divisor)
                    .ok_or(TaskError::InvalidConfiguration)?;
            } else if held != 0 {
                return Err(TaskError::DeadlineAdmission);
            }
        }
        Ok(DeadlineBandwidthRebuild {
            online_cpus,
            reserved_scaled,
            distributed_scaled,
        })
    }

    /// Publishes one generation-bearing exit candidate without allocating.
    pub(super) fn queue_exited_thread(&mut self, thread: ThreadId) {
        self.exited_work.publish(thread, self.slots.len());
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

    pub(super) fn cpu_registration(&self, cpu: CpuId) -> Result<&CpuRegistration, TaskError> {
        self.cpus
            .get(cpu.as_usize())
            .ok_or(TaskError::InvalidCpu(cpu.as_u32()))
    }

    pub(super) fn ensure_cpu_online(&self, cpu: &CpuLocal) -> Result<(), TaskError> {
        let registration = self.cpu_registration(cpu.owner())?;
        if Arc::ptr_eq(&registration.remote, cpu.remote()) && cpu.is_online() {
            Ok(())
        } else {
            Err(TaskError::CpuOffline(cpu.owner().as_u32()))
        }
    }

    pub(super) fn release_deadline_reservation_on_exit(
        &mut self,
        thread: ThreadId,
    ) -> Result<u64, TaskError> {
        let held = {
            let record = self.thread_record(thread)?;
            let mut sched = record.sched.lock();
            let held = sched.held_deadline_reservation();
            sched.deadline.bandwidth.replace_detached_reservation(0);
            sched.policy.discard_pending_update();
            held
        };
        Ok(held)
    }

    pub(super) fn remove_exited_thread(
        &mut self,
        thread: ThreadId,
    ) -> Result<(ThreadRecord, u64), TaskError> {
        self.remove_exited_thread_with_lease_count(thread, 0, None)
    }

    pub(super) fn remove_unpublished_thread_with_handle(
        &mut self,
        handle: &ThreadHandle,
    ) -> Result<(ThreadRecord, u64), TaskError> {
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
                || sched.placement.on_cpu().is_some()
                || sched.placement.has_pending_migration()
                || sched.deadline.bandwidth.reservation_owner().is_some()
                || sched.pi.blocked_on.is_some()
                || record.callbacks.blocks_reap()
                || record.core.scheduler_inbox_delivery_count() != 0
                || record.core.sleep_timer_cpu().is_some()
                || record.core.external_lease_count() != 1
            {
                return Err(TaskError::ThreadBusy);
            }
            sched.held_deadline_reservation()
        };
        let record = slot.record.take().ok_or(TaskError::StaleThreadId)?;
        if advance_thread_slot_generation(slot) {
            self.free_slots.push(thread.slot());
        }
        Ok((record, held))
    }

    pub(super) fn remove_exited_thread_with_lease_count(
        &mut self,
        thread: ThreadId,
        expected_external_leases: usize,
        expected_core: Option<*const ThreadCore>,
    ) -> Result<(ThreadRecord, u64), TaskError> {
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
                || sched.placement.has_pending_migration()
                || sched.deadline.bandwidth.reservation_owner().is_some()
                || sched.deadline.overrun_events != 0
                || record.callbacks.blocks_reap()
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
            sched.held_deadline_reservation()
        };
        if advance_thread_slot_generation(slot) {
            self.free_slots.push(thread.slot());
        }
        self.exited_work.remove(thread);
        Ok((record, held))
    }

    pub(super) fn remove_exited_thread_with_handle(
        &mut self,
        handle: &ThreadHandle,
    ) -> Result<(ThreadRecord, u64), TaskError> {
        self.remove_exited_thread_with_lease_count(handle.id(), 1, Some(Arc::as_ptr(&handle.core)))
    }

    pub(super) fn take_unreferenced_exited(
        &mut self,
    ) -> Result<Option<(ThreadRecord, u64)>, TaskError> {
        let candidate_count = self.exited_work.candidate_count();
        for _ in 0..candidate_count {
            let thread = self
                .exited_work
                .next_candidate()
                .expect("exit candidate count must match the queue");
            match self.remove_exited_thread_with_lease_count(thread, 0, None) {
                Ok(record) => return Ok(Some(record)),
                Err(TaskError::ThreadBusy) => {}
                Err(TaskError::StaleThreadId) => self.exited_work.remove(thread),
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }

    pub(super) fn claim_pending_exit_callback(
        &mut self,
    ) -> Result<Option<(ThreadExtensionView, ThreadId)>, TaskError> {
        let candidate_count = self.exited_work.candidate_count();
        for _ in 0..candidate_count {
            let thread = self
                .exited_work
                .next_candidate()
                .expect("exit candidate count must match the queue");
            let extension = match self.thread_record_mut(thread) {
                Ok(record) => {
                    let sched = record.sched.lock();
                    if sched.lifecycle.state() != ThreadState::Exited
                        || sched.placement.on_cpu().is_some()
                        || sched.deadline.overrun_events != 0
                        || record.callbacks.deadline_is_claimed()
                        || !record.callbacks.exit_is_pending()
                    {
                        None
                    } else {
                        let extension = record
                            .extension
                            .as_ref()
                            .ok_or(TaskError::InvalidConfiguration)?
                            .as_view();
                        record.callbacks.claim_exit()?;
                        Some(extension)
                    }
                }
                Err(TaskError::StaleThreadId) => {
                    self.exited_work.remove(thread);
                    continue;
                }
                Err(error) => return Err(error),
            };
            if let Some(extension) = extension {
                return Ok(Some((extension, thread)));
            }
        }
        Ok(None)
    }

    pub(super) fn finish_exit_callback(&mut self, thread: ThreadId) -> Result<(), TaskError> {
        let record = self.thread_record_mut(thread)?;
        let sched = record.sched.lock();
        if sched.lifecycle.state() != ThreadState::Exited || sched.placement.on_cpu().is_some() {
            return Err(TaskError::InvalidConfiguration);
        }
        record.callbacks.finish_exit()
    }

    pub(super) fn finish_deadline_callback(&mut self, thread: ThreadId) -> Result<bool, TaskError> {
        let record = self.thread_record_mut(thread)?;
        let sched = record.sched.lock();
        record.callbacks.finish_deadline()?;
        Ok(sched.deadline.overrun_events != 0)
    }

    pub(super) fn select_initial_fair_cpu(
        &self,
        affinity: &CpuSet,
        preferred: Option<CpuId>,
    ) -> Option<CpuId> {
        self.cpus
            .iter()
            .enumerate()
            .filter_map(|(index, registration)| {
                let cpu = CpuId::new(index as u32);
                if !registration.remote.accepts_placement() || !affinity.contains(cpu) {
                    return None;
                }
                Some((
                    registration.remote.placement_demand(),
                    Some(cpu) != preferred,
                    cpu,
                ))
            })
            .min_by_key(|(load, not_preferred, cpu)| (*load, *not_preferred, cpu.as_u32()))
            .map(|(_, _, cpu)| cpu)
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
        let result = cpu_local.publish_owner_control(node, message);
        if result != PublishResult::Published {
            // SAFETY: a rejected/coalesced publication did not consume this
            // attempt's retained reference.
            unsafe { Arc::decrement_strong_count(pointer) };
            core.cancel_scheduler_inbox_delivery();
        }
        Ok(())
    }

    pub(super) fn cpu_remote(&self, cpu: CpuId) -> Option<&CpuRemote> {
        let registration = self.cpu_registration(cpu).ok()?;
        if !registration.remote.is_online() {
            return None;
        }
        Some(registration.remote.as_ref())
    }
}

#[derive(Debug)]
pub(super) struct CpuRegistration {
    pub(super) remote: Arc<CpuRemote>,
}

#[derive(Debug)]
pub(super) struct ThreadSlot {
    pub(super) generation: u32,
    pub(super) record: Option<ThreadRecord>,
    pub(super) pending_deadline_reservation: u64,
}

#[derive(Debug)]
pub(super) struct ThreadRecord {
    pub(super) core: Arc<ThreadCore>,
    pub(super) sched: Arc<ThreadSchedCell>,
    // Explicit teardown consumes this bundle before dropping the extension.
    pub(super) resources: ThreadResources,
    pub(super) extension: Option<ThreadExtension>,
    pub(super) callbacks: ThreadCallbackState,
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

    pub(super) fn release(mut self) -> crate::runtime::AddressSpaceToken {
        let address_space = self.resources.release();
        drop(self.extension.take());
        address_space
    }
}

impl ThreadRecord {
    pub(super) fn has_live_pi_edges(&self) -> bool {
        let sched = self.sched.lock();
        sched.pi.blocked_on.is_some() || !sched.pi.donors.is_empty()
    }
}
