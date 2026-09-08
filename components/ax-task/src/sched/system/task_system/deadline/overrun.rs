//! Overrun under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    pub(in crate::sched::system::task_system) fn publish_deadline_overrun_work(
        &self,
        core: Arc<ThreadCore>,
    ) {
        let core_ptr = Arc::into_raw(core);
        let node = unsafe {
            // SAFETY: Arc allocations remain pinned, and the strong count
            // transferred below keeps the embedded node alive until drain.
            Pin::new_unchecked((&*core_ptr).deadline_callback_node())
        };
        let result = self.deferred_deadline_callbacks.publish(
            node,
            InboxMessage::deadline_overrun(
                unsafe {
                    // SAFETY: the transferred Arc keeps this core alive.
                    (*core_ptr).id()
                },
                core_ptr.expose_provenance(),
            ),
        );
        if result == PublishResult::Published {
            self.task_work.publish();
            return;
        }
        unsafe {
            // SAFETY: a coalesced or rejected publication did not consume the
            // transferred strong count.
            drop(Arc::from_raw(core_ptr));
        }
        if result == PublishResult::WrongKind {
            task_runtime::fatal_invariant(0x444c_0001, result as usize);
        }
    }

    pub(super) fn task_deadline_error(error: TaskDeadlineError) -> TaskError {
        match error {
            TaskDeadlineError::Capacity => TaskError::TimerCapacity,
            TaskDeadlineError::GenerationExhausted | TaskDeadlineError::KindMismatch => {
                TaskError::InvalidConfiguration
            }
        }
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

    pub(in crate::sched::system::task_system) fn dispatch_deadline_overruns_inner(
        &self,
        limit: usize,
    ) -> Result<(usize, usize), TaskError> {
        const MAX_DISPATCH_BATCH: usize = 64;

        let mut messages = [InboxMessage::EMPTY; MAX_DISPATCH_BATCH];
        let batch = self
            .deferred_deadline_callbacks
            .drain(limit.min(MAX_DISPATCH_BATCH), &mut messages);
        let mut dispatched = 0;
        for message in messages.iter().take(batch.drained()) {
            if message.operation() != InboxOperation::DeadlineOverrun || message.payload() == 0 {
                task_runtime::fatal_invariant(0x444c_0002, message.payload());
            }
            let core = unsafe {
                // SAFETY: publication transferred exactly one Arc strong count
                // whose pointer is carried by this detached message.
                Arc::from_raw(ptr::with_exposed_provenance::<ThreadCore>(
                    message.payload(),
                ))
            };
            if core.id() != message.thread_id() {
                task_runtime::fatal_invariant(0x444c_0003, message.payload());
            }
            let claim = self
                .state
                .lock()
                .claim_pending_deadline_overrun(core.id())?;
            match claim {
                DeadlineCallbackClaim::NoCallback { has_more } => {
                    if has_more {
                        self.publish_deadline_overrun_work(Arc::clone(&core));
                    }
                }
                DeadlineCallbackClaim::Callback { extension, thread } => {
                    // SAFETY: the registry's callback claim prevents reaping
                    // while the callback runs, and every scheduler lock was
                    // released above.
                    unsafe {
                        (extension.ops().on_deadline_overrun)(extension.data(), thread);
                    }
                    if self.state.lock().finish_deadline_callback(thread)? {
                        self.publish_deadline_overrun_work(Arc::clone(&core));
                    }
                    dispatched += 1;
                }
            }
        }
        if batch.pending() {
            self.task_work.publish();
        }
        Ok((batch.drained(), dispatched))
    }
}
