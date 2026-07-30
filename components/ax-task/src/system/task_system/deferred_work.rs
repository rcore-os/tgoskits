//! Deferred task-context work and resource reclamation.

use super::*;

impl TaskSystem {
    pub(crate) fn publish_current_scheduler_tick_work(&self, cpu: &CpuLocal) {
        let Some(core) = cpu.current_core() else {
            return;
        };
        self.publish_scheduler_tick_work(core);
    }

    fn publish_scheduler_tick_work(&self, core: &Arc<ThreadCore>) {
        if !core.begin_scheduler_tick_work() {
            return;
        }
        if !core.reserve_scheduler_inbox_delivery() {
            core.cancel_scheduler_tick_work();
            return;
        }

        let core_ptr = Arc::as_ptr(core);
        // SAFETY: the retained strong count is transferred to the task-work
        // inbox and released by its sole consumer after callback completion.
        unsafe { Arc::increment_strong_count(core_ptr) };
        // SAFETY: Arc allocations are pinned for their lifetime, and the
        // delivery reservation keeps this core and its extension alive.
        let node = unsafe { Pin::new_unchecked((&*core_ptr).scheduler_tick_work_node()) };
        let result = self.deferred_scheduler_ticks.publish(
            node,
            InboxMessage::scheduler_tick(core.id(), core_ptr.expose_provenance()),
        );
        if result == PublishResult::Published {
            self.task_work.publish();
            return;
        }

        // SAFETY: a rejected publication did not consume the transferred Arc.
        unsafe { Arc::decrement_strong_count(core_ptr) };
        core.cancel_scheduler_inbox_delivery();
        core.cancel_scheduler_tick_work();
        task_runtime::fatal_invariant(0x5457_0001, result as usize);
    }

    /// Publishes one zero-reference resource to the task-context reaper.
    ///
    /// The pinned node supplies its own fixed reclaim function. Publication is
    /// allocation-free and does not invoke that function, so callers may use it
    /// from hard IRQ context. `data` must be an exposed allocation address
    /// numerically equal to the node address; this is checked before its
    /// intrusive membership is published.
    pub(crate) fn publish_deferred_reclaim(
        &self,
        node: Pin<&'static DeferredReclaimNode>,
        data: usize,
    ) -> PublishResult {
        if data != node.address() {
            task_runtime::fatal_invariant(0x4558_0007, data);
        }
        let _publication = IrqScope::enter();
        let result = self.deferred_reclaims.publish(
            node.inbox(),
            InboxMessage::reclaim(ThreadId::from_parts(0, 0), 0, data),
        );
        if result != PublishResult::WrongKind {
            self.task_work.publish();
        }
        result
    }

    pub(crate) fn task_work_doorbell(&self) -> Arc<TaskWorkDoorbell> {
        Arc::clone(&self.task_work)
    }

    pub(crate) fn begin_task_work_worker_install(&self) -> Result<(), TaskError> {
        self.task_work.begin_worker_install()
    }

    pub(crate) fn finish_task_work_worker_install(&self) {
        self.task_work.finish_worker_install();
    }

    pub(crate) fn cancel_task_work_worker_install(&self) {
        self.task_work.cancel_worker_install();
    }

    /// Reports whether a sticky task-work publication awaits the service thread.
    pub fn deferred_task_work_pending(&self) -> bool {
        self.task_work.is_pending()
    }

    /// Runs one bounded task-context pass as the single task-work consumer.
    ///
    /// Unrelated work classes are interleaved through a persistent round-robin
    /// cursor. Per-thread claim predicates still enforce Deadline callback,
    /// exit callback, and record-reaping order. A concurrent or reentrant
    /// consumer receives [`TaskError::ThreadBusy`] without consuming work.
    pub fn service_deferred_task_work(
        &self,
        limit: usize,
    ) -> Result<DeferredTaskWorkBatch, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let limit = limit.min(crate::DEFAULT_BATCH_LIMIT);
        if limit == 0 {
            return Ok(DeferredTaskWorkBatch::default());
        }
        let _consumer: TaskWorkConsumerGuard<'_> = self.task_work.try_claim_consumer()?;
        let mut next_class = self.state.lock().task_work_class_cursor;
        let outcome = (|| {
            let mut batch = DeferredTaskWorkBatch::default();
            let mut classes_without_progress = 0;
            while batch.processed() < limit
                && classes_without_progress < DeferredTaskWorkClass::COUNT
            {
                let class = next_class;
                next_class = class.next();
                let processed = match class {
                    DeferredTaskWorkClass::Deadline => {
                        let (events, callbacks) = self.dispatch_deadline_overruns_inner(1)?;
                        batch.deadline_events += events;
                        batch.deadline_callbacks += callbacks;
                        events
                    }
                    DeferredTaskWorkClass::SchedulerTick => {
                        let (events, callbacks) = self.dispatch_scheduler_tick_work_inner(1)?;
                        batch.scheduler_tick_events += events;
                        batch.scheduler_tick_callbacks += callbacks;
                        events
                    }
                    DeferredTaskWorkClass::Exit => {
                        let callbacks = self.dispatch_exit_callbacks_inner(1)?;
                        batch.exit_callbacks += callbacks;
                        callbacks
                    }
                    DeferredTaskWorkClass::Reap => {
                        let reaped = self.reap_unreferenced_exited_inner(1)?;
                        batch.reaped_threads += reaped;
                        reaped
                    }
                    DeferredTaskWorkClass::Reclaim => {
                        let reclaimed = self.reclaim_one_resource()?;
                        batch.reclaimed_resources += reclaimed;
                        reclaimed
                    }
                };
                if processed == 0 {
                    classes_without_progress += 1;
                } else {
                    classes_without_progress = 0;
                }
            }
            debug_assert!(batch.processed() <= limit);
            Ok(batch)
        })();
        self.state.lock().task_work_class_cursor = next_class;
        outcome
    }

    fn dispatch_scheduler_tick_work_inner(
        &self,
        limit: usize,
    ) -> Result<(usize, usize), TaskError> {
        let mut messages = [InboxMessage::EMPTY; crate::DEFAULT_BATCH_LIMIT];
        let batch = self
            .deferred_scheduler_ticks
            .drain(limit.min(crate::DEFAULT_BATCH_LIMIT), &mut messages);
        let mut callbacks = 0;
        for message in messages.iter().take(batch.drained()) {
            if message.operation() != InboxOperation::SchedulerTick || message.payload() == 0 {
                task_runtime::fatal_invariant(0x5457_0002, message.payload());
            }
            let core = unsafe {
                // SAFETY: publication transferred exactly one Arc strong count
                // whose pointer is carried by this detached message.
                Arc::from_raw(ptr::with_exposed_provenance::<ThreadCore>(
                    message.payload(),
                ))
            };
            let _delivery = core.accept_scheduler_inbox_delivery();
            if core.id() != message.thread_id() {
                core.cancel_scheduler_tick_work();
                continue;
            }
            let work = core.take_scheduler_tick_work();
            if let Some(work) = work
                && let Some(extension) = core.extension_view()
            {
                // SAFETY: the inbox delivery reservation prevents extension
                // reclamation even if the carrier thread exits concurrently.
                // The gate generation decides whether the process/subsystem
                // work remains relevant; carrier-thread state does not.
                unsafe { work.invoke(extension.data(), core.id()) };
                callbacks += 1;
            }
        }
        if batch.pending() {
            self.task_work.publish();
        }
        Ok((batch.drained(), callbacks))
    }

    fn reclaim_one_resource(&self) -> Result<usize, TaskError> {
        let thread_release_first = {
            let mut state = self.state.lock();
            let current = state.thread_release_first;
            state.thread_release_first = !current;
            current
        };
        if thread_release_first {
            if self.retry_pending_resource_release() {
                return Ok(1);
            }
            self.drain_deferred_reclaims_inner(1)
        } else {
            let reclaimed = self.drain_deferred_reclaims_inner(1)?;
            if reclaimed != 0 {
                Ok(reclaimed)
            } else {
                Ok(usize::from(self.retry_pending_resource_release()))
            }
        }
    }

    fn retry_pending_resource_release(&self) -> bool {
        let Some(mut pending) = self.state.lock().pending_resource_releases.pop() else {
            return false;
        };
        match pending.resources_mut().try_release() {
            Ok(()) => {
                pending.finish();
            }
            Err(_error) => {
                self.state.lock().pending_resource_releases.push(pending);
                // A failed release retains every live handle. Re-publish after
                // returning it to the queue so the service thread yields
                // between retries instead of spinning inside one batch.
                self.task_work.publish();
            }
        }
        true
    }

    /// Reclaims at most `limit` resources in ordinary task context.
    ///
    /// The implementation additionally caps one pass at 64 callbacks so an
    /// accidental large caller limit cannot create an unbounded safe point.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::UnsafeContext`] from hard IRQ context.
    pub fn drain_deferred_reclaims(&self, limit: usize) -> Result<usize, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let _consumer = self.task_work.try_claim_consumer()?;
        self.drain_deferred_reclaims_inner(limit)
    }

    fn drain_deferred_reclaims_inner(&self, limit: usize) -> Result<usize, TaskError> {
        const MAX_DRAIN_BATCH: usize = 64;

        let mut messages = [InboxMessage::EMPTY; MAX_DRAIN_BATCH];
        let batch = self
            .deferred_reclaims
            .drain(limit.min(MAX_DRAIN_BATCH), &mut messages);
        for message in messages.iter().take(batch.drained()) {
            let data = ptr::with_exposed_provenance_mut::<()>(message.payload());
            if data.is_null() {
                continue;
            }
            let node = data.cast::<DeferredReclaimNode>();
            unsafe {
                // Detachment cleared this node's inbox membership before the
                // fixed callback receives exclusive ownership of its resource.
                DeferredReclaimNode::reclaim(node, data);
            }
        }
        Ok(batch.drained())
    }
}
