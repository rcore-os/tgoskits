//! Creation tokens transfer cancellation to the existing task-context reaper.

use super::*;

impl TaskSystem {
    /// Transfers one unique creation token without taking registry or task locks.
    pub(crate) fn publish_thread_cancellation(&self, core: &Arc<ThreadCore>) {
        let _irq = IrqScope::enter();
        let retained = Arc::into_raw(Arc::clone(core));
        // SAFETY: the transferred Arc pins both ThreadCore and its immutable
        // execution Arc until the sole consumer detaches the embedded node.
        // Only the non-cloneable creation token publishes this node; retries
        // are published by the consumer after detachment.
        let node = unsafe {
            Pin::new_unchecked(
                &(*retained)
                    .execution
                    .as_ref()
                    .expect("only managed tasks have cancellation tokens")
                    .cancellation_node,
            )
        };
        let result = self.deferred_thread_cancellations.publish(
            node,
            InboxMessage::reclaim(core.id(), 0, retained.expose_provenance()),
        );
        if result != PublishResult::Published {
            // SAFETY: rejection did not transfer the additional strong count.
            unsafe { Arc::decrement_strong_count(retained) };
            task_runtime::fatal_invariant(0x4341_0001, core.id().as_u64() as usize);
        }
        self.task_work.publish();
    }

    pub(super) fn process_thread_cancellation(&self) -> Result<usize, TaskError> {
        let mut messages = [InboxMessage::EMPTY];
        let batch = self.deferred_thread_cancellations.drain(1, &mut messages);
        if batch.pending() {
            self.task_work.publish();
        }
        if batch.drained() == 0 {
            return Ok(0);
        }
        let message = messages[0];
        assert_eq!(message.operation(), InboxOperation::Reclaim);
        // SAFETY: publication transferred exactly one strong count. Inbox
        // detachment ends all node readers before returning this payload.
        let core = unsafe {
            Arc::from_raw(ptr::with_exposed_provenance::<ThreadCore>(
                message.payload(),
            ))
        };
        assert_eq!(core.id(), message.thread_id());
        // A managed New task cannot be exited through the public raw API, so
        // the registry still owns its resources even without external leases.
        let reservation = self
            .state
            .lock()
            .thread_record_mut(core.id())
            .expect("managed cancellation retains its New registry record")
            .activation
            .take();
        drop(reservation);
        match self.mark_unqueued_exited(&core) {
            Ok(()) => {}
            Err(TaskError::ThreadBusy) => {
                // A concurrent control operation may still own scheduler
                // activity. Retain the request rather than losing cancellation.
                self.publish_thread_cancellation(&core);
            }
            Err(error) => panic!("managed cancellation invariant: {error}"),
        }
        Ok(1)
    }
}
