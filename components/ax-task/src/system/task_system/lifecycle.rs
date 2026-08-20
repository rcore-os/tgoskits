//! Thread exit callbacks, registry reaping, and resource release.

use super::*;

impl TaskSystem {
    /// Marks a non-queued thread exited and queues its task-context exit hook.
    pub fn mark_exited(&self, thread: ThreadId) -> Result<(), TaskError> {
        let core = {
            let state = self.state.lock();
            Arc::clone(&state.thread_record(thread)?.core)
        };
        let mut scheduler_exit = core
            .close_owned_scheduler_activity()
            .ok_or(TaskError::ThreadBusy)?;
        let exited_core = {
            let mut state = self.state.lock();
            let record = state.thread_record_mut(thread)?;
            if !Arc::ptr_eq(&record.core, &core) {
                return Err(TaskError::StaleThreadId);
            }
            let mut sched = record.sched.lock();
            if sched.placement.queued_cpu().is_some() {
                return Err(TaskError::AlreadyQueued);
            }
            if sched.placement.on_cpu().is_some() {
                return Err(TaskError::ThreadBusy);
            }
            if sched.pi.blocked_on.is_some() || !sched.pi.donors.is_empty() {
                return Err(TaskError::InvalidPiState);
            }
            let lifecycle = sched.lifecycle.state();
            if !crate::thread::transition_is_valid(lifecycle, ThreadState::Exited) {
                return Err(TaskError::InvalidTransition {
                    from: lifecycle,
                    to: ThreadState::Exited,
                });
            }
            record.callbacks.validate_prepare_exit()?;
            if let Some(owner) = sched.deadline.bandwidth.reservation_owner() {
                let remote = self
                    .cpu_remotes
                    .get(owner.as_usize())
                    .ok_or(TaskError::InvalidCpu(owner.as_u32()))?;
                if !remote.is_online() {
                    task_runtime::fatal_invariant(0x444c_1203, core.id().as_u64() as usize);
                }
                let mut transaction = OwnerRqTxn::begin(self, remote);
                Self::detach_owner_deadline_bandwidth_in_rq(
                    &record.core,
                    &mut sched,
                    remote,
                    &mut transaction,
                );
                transaction.commit();
                // A stale physical clockevent is harmless, but the owning CPU
                // must promptly publish the new earliest scheduler deadline
                // instead of waiting for that stale edge to fire.
                remote.request_scheduler_work();
            }
            sched.placement.cancel_remote_handoff_for_exit();
            sched
                .transition(&record.core, ThreadState::Exited)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x4558_000a, core.id().as_u64() as usize)
                });
            scheduler_exit.seal();
            record
                .callbacks
                .prepare_exit(record.extension.is_some())
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x4558_000b, core.id().as_u64() as usize)
                });
            let exited_core = Arc::clone(&record.core);
            drop(sched);
            state.queue_exited_thread(thread);
            let mut root_domain = self.root_domain.lock();
            let released = state
                .release_deadline_reservation_on_exit(thread)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x4558_000c, core.id().as_u64() as usize)
                });
            root_domain.release_deadline(released);
            exited_core
        };
        exited_core.notify_affinity_waiters();
        self.task_work.publish();
        Ok(())
    }

    /// Runs pending exit callbacks from an ordinary task-context safe point.
    ///
    /// Context-switch tail only proves that the exited stack is inactive; its
    /// inherited IRQ and scheduler guards are still live. Calling an OS exit
    /// hook there can acquire a sleepable lock and recursively enter the
    /// scheduler. This bounded pass claims each callback under the registry
    /// lock, invokes it without scheduler locks, and only then makes the record
    /// eligible for reaping.
    pub fn dispatch_exit_callbacks(&self, limit: usize) -> Result<usize, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let _consumer = self.task_work.try_claim_consumer()?;
        self.dispatch_exit_callbacks_inner(limit)
    }

    pub(super) fn dispatch_exit_callbacks_inner(&self, limit: usize) -> Result<usize, TaskError> {
        let mut dispatched = 0;
        while dispatched < limit {
            let callback = {
                let mut state = self.state.lock();
                state.claim_pending_exit_callback()?
            };
            let Some((extension, thread)) = callback else {
                break;
            };
            // SAFETY: the registry record keeps the claimed extension live,
            // and ThreadExtension construction validated this callback table.
            unsafe { (extension.ops().on_exit)(extension.data(), thread) };
            self.state.lock().finish_exit_callback(thread)?;
            dispatched += 1;
        }
        Ok(dispatched)
    }

    /// Removes an exited registry record and makes its slot reusable.
    pub fn reap_thread(&self, thread: ThreadId) -> Result<(), TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let record = {
            let mut state = self.state.lock();
            let mut root_domain = self.root_domain.lock();
            let (record, released) = state.remove_exited_thread(thread)?;
            root_domain.release_deadline(released);
            record
        };
        self.release_thread_record(record);
        Ok(())
    }

    /// Atomically removes an exited thread while consuming its owning handle.
    ///
    /// Keeping `handle` alive until registry removal prevents the detached
    /// reaper on another CPU from winning between a handle drop and an ID-based
    /// reap. Retryable failures return the same handle to the caller.
    pub fn reap_thread_handle(&self, handle: ThreadHandle) -> Result<(), OwnedThreadReapError> {
        if task_runtime::in_hard_irq() {
            return Err(OwnedThreadReapError::new(TaskError::UnsafeContext, handle));
        }
        let record = {
            let mut state = self.state.lock();
            let mut root_domain = self.root_domain.lock();
            match state.remove_exited_thread_with_handle(&handle) {
                Ok((record, released)) => {
                    root_domain.release_deadline(released);
                    record
                }
                Err(error) => return Err(OwnedThreadReapError::new(error, handle)),
            }
        };
        drop(handle);
        self.release_thread_record(record);
        Ok(())
    }

    /// Reaps exited records for which no external strong handle remains.
    ///
    /// This bounded task-context pass is the detached-thread reaper. Joinable
    /// threads remain registered because their [`ThreadHandle`] contributes a
    /// strong reference. Late IRQ wake handles likewise delay resource release
    /// until their final reference reaches the task-context reaper.
    pub fn reap_unreferenced_exited(&self, limit: usize) -> Result<usize, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let _consumer = self.task_work.try_claim_consumer()?;
        self.reap_unreferenced_exited_inner(limit)
    }

    pub(super) fn reap_unreferenced_exited_inner(&self, limit: usize) -> Result<usize, TaskError> {
        let mut reaped = 0;
        while reaped < limit {
            let removed = {
                let mut state = self.state.lock();
                let mut root_domain = self.root_domain.lock();
                let removed = state.take_unreferenced_exited()?;
                if let Some((_, released)) = &removed {
                    root_domain.release_deadline(*released);
                }
                removed
            };
            let Some((record, _released)) = removed else {
                break;
            };
            self.release_thread_record(record);
            reaped += 1;
        }
        Ok(reaped)
    }

    pub(super) fn release_thread_record(&self, mut record: ThreadRecord) {
        let address_space = record.resources.release();
        drop(record.extension.take());
        self.release_address_space_token(address_space);
    }

    pub(super) fn release_unpublished_thread(&self, record: DetachedThreadRecord) {
        let address_space = record.release();
        self.release_address_space_token(address_space);
    }

    /// Releases a construction transaction that failed before thread registry
    /// publication.
    ///
    /// Thread-private destruction is a one-way ownership transfer. Only the
    /// independent active-mm token may outlive this call through the runtime's
    /// explicit last-CPU readiness edge.
    pub fn release_unpublished_resources(&self, resources: ThreadResources) {
        self.release_unpublished_thread(DetachedThreadRecord::new(resources, None))
    }

    pub(crate) fn release_address_space_token(
        &self,
        address_space: crate::runtime::AddressSpaceToken,
    ) {
        if address_space.is_none() {
            return;
        }
        let handle = address_space.handle();
        match task_runtime::destroy_address_space(handle) {
            AddressSpaceDestroyOutcome::Released => return,
            AddressSpaceDestroyOutcome::Active => {}
        }
        self.state
            .lock()
            .pending_address_space_reclaims
            .push(address_space);
        match task_runtime::arm_address_space_reclaim(handle) {
            AddressSpaceReclaimArmOutcome::Ready => self.task_work.publish(),
            AddressSpaceReclaimArmOutcome::Armed => {}
        }
    }
}
