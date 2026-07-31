//! Thread exit callbacks, registry reaping, and resource release.

use super::*;

impl TaskSystem {
    /// Marks a non-queued thread exited and queues its task-context exit hook.
    pub fn mark_exited(&self, thread: ThreadId) -> Result<(), TaskError> {
        let exited_core = {
            let mut state = self.state.lock();
            let cleanup_deadline_member = {
                let record = state.thread_record_mut(thread)?;
                let mut sched = record.sched.lock();
                if sched.placement.queued_cpu().is_some() || sched.placement.running_cpu().is_some()
                {
                    return Err(TaskError::AlreadyQueued);
                }
                if sched.placement.on_cpu().is_some() {
                    return Err(TaskError::ThreadBusy);
                }
                if record.blocked_on.is_some()
                    || record.pi_waiter_head.is_some()
                    || sched.pi.blocked_waiters != 0
                {
                    return Err(TaskError::InvalidPiState);
                }
                if sched.pi.deadline_cbs_borrower.is_some() {
                    return Err(TaskError::ThreadBusy);
                }
                if sched.deadline.bandwidth_cpu.is_some() {
                    sched.deadline.cleanup_pending = true;
                    true
                } else {
                    false
                }
            };
            if cleanup_deadline_member {
                state.request_owner_reschedule(thread);
                return Err(TaskError::ThreadBusy);
            }
            let exited_core = {
                let record = state.thread_record_mut(thread)?;
                let exited_core = Arc::clone(&record.core);
                let _exit = record
                    .core
                    .try_scheduler_exit()
                    .ok_or(TaskError::ThreadBusy)?;
                let mut sched = record.sched.lock();
                if sched.placement.queued_cpu().is_some() || sched.placement.running_cpu().is_some()
                {
                    return Err(TaskError::AlreadyQueued);
                }
                if sched.placement.on_cpu().is_some() || sched.pi.deadline_cbs_borrower.is_some() {
                    return Err(TaskError::ThreadBusy);
                }
                if record.blocked_on.is_some()
                    || record.pi_waiter_head.is_some()
                    || sched.pi.blocked_waiters != 0
                {
                    return Err(TaskError::InvalidPiState);
                }
                sched.placement.set_migration_target(None)?;
                sched.transition(&record.core, ThreadState::Exited)?;
                record.callbacks.prepare_exit(record.extension.is_some())?;
                exited_core
            };
            state.queue_exited_thread(thread);
            state.release_deadline_reservation_on_exit(thread)?;
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
            state.remove_exited_thread(thread)?
        };
        self.release_thread_record(record)
    }

    /// Atomically removes an exited thread while consuming its owning handle.
    ///
    /// Keeping `handle` alive until registry removal prevents the detached
    /// reaper on another CPU from winning between a handle drop and an ID-based
    /// reap. Retryable failures return the same handle to the caller.
    pub fn reap_thread_handle(&self, handle: ThreadHandle) -> Result<(), OwnedThreadReapError> {
        if task_runtime::in_hard_irq() {
            return Err(OwnedThreadReapError::Retry {
                error: TaskError::UnsafeContext,
                handle,
            });
        }
        let record = {
            let mut state = self.state.lock();
            match state.remove_exited_thread_with_handle(&handle) {
                Ok(record) => record,
                Err(error) => return Err(OwnedThreadReapError::Retry { error, handle }),
            }
        };
        drop(handle);
        self.release_thread_record(record)
            .map_err(OwnedThreadReapError::Committed)
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
            let record = {
                let mut state = self.state.lock();
                state.take_unreferenced_exited()?
            };
            let Some(record) = record else {
                break;
            };
            // Registry removal is already committed. A failed runtime release
            // moves the complete record into the task-work retry queue instead
            // of losing its resource handles or terminating the service loop.
            let _release = self.release_thread_record(record);
            reaped += 1;
        }
        Ok(reaped)
    }

    pub(super) fn release_thread_record(&self, mut record: ThreadRecord) -> Result<(), TaskError> {
        match record.resources.try_release() {
            Ok(()) => {
                drop(record.extension.take());
                Ok(())
            }
            Err(error) => {
                self.state
                    .lock()
                    .pending_resource_releases
                    .push(PendingResourceRelease::Thread(record));
                self.task_work.publish();
                Err(error)
            }
        }
    }

    pub(super) fn release_unpublished_thread(
        &self,
        mut record: DetachedThreadRecord,
    ) -> Result<(), TaskError> {
        match record.try_release_resources() {
            Ok(()) => {
                record.finish_release();
                Ok(())
            }
            Err(error) => {
                self.state
                    .lock()
                    .pending_resource_releases
                    .push(PendingResourceRelease::Detached(record));
                self.task_work.publish();
                Err(error)
            }
        }
    }

    /// Releases a construction transaction that failed before thread registry
    /// publication.
    ///
    /// If the runtime reports a retryable failure, this method retains the
    /// complete bundle and publishes task-context reaper work. The returned
    /// error describes the first failed release operation; ownership never
    /// returns to the caller.
    pub fn release_unpublished_resources(
        &self,
        resources: ThreadResources,
    ) -> Result<(), TaskError> {
        self.release_unpublished_thread(DetachedThreadRecord::new(resources, None))
    }
}
