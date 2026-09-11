//! Polling under the owning scheduler transaction.

use super::*;

impl LocalExecutor {
    pub(super) unsafe fn spawn_scoped<F>(&self, future: F) -> (*mut CoroutineHeader, CoroutineId)
    where
        F: Future<Output = ()>,
    {
        let id = self.allocate_coroutine_id();
        let coroutine = Box::new(Coroutine::new(id, Arc::clone(&self.shared), future));
        let header = Box::into_raw(coroutine).cast::<CoroutineHeader>();
        self.link_active(header);
        unsafe {
            // The fresh pinned allocation owns its permanent owner reference;
            // publication retains a distinct ready-queue reference.
            coroutine::schedule(header);
        }
        (header, id)
    }

    pub(super) fn allocate_coroutine_id(&self) -> CoroutineId {
        let generation = self.next_generation.get();
        self.next_generation.set(
            generation
                .checked_add(1)
                .expect("coroutine generation exhausted"),
        );
        CoroutineId::new(self.shared.owner_thread, generation)
    }

    pub(super) fn take_ready_snapshot(&self) -> *mut CoroutineHeader {
        let pending = self.ready_pending.replace(ptr::null_mut());
        if pending.is_null() {
            unsafe {
                // This executor is the only consumer of its ready inbox.
                self.shared.ready.take_fifo()
            }
        } else {
            pending
        }
    }

    /// Polls one node detached from the ready inbox.
    ///
    /// # Safety
    ///
    /// `header` must carry a live ready-queue reference and belong to this
    /// executor's exclusively owned detached snapshot.
    pub(super) unsafe fn poll_ready_coroutine(
        &self,
        header: *mut CoroutineHeader,
    ) -> PollDisposition {
        let state = unsafe {
            // The queue reference guarantees a valid header throughout polling.
            &(*header).state
        };
        let dequeued_state = state.fetch_and(!RUN_QUEUED, Ordering::AcqRel);
        let mut queue_reference = ReadyQueueReference::new(header);
        if dequeued_state & COMPLETE != 0 {
            return PollDisposition::Skipped;
        }

        state.fetch_or(POLLING, Ordering::AcqRel);
        queue_reference.mark_polling();
        let waker = unsafe {
            // `header` remains pinned and the queue reference outlives the Waker.
            coroutine_waker(header)
        };
        let mut context = Context::from_waker(&waker);
        let result = unsafe {
            // Only the owner reaches this function, and POLLING excludes a second
            // owner poll of the same future.
            CoroutineHeader::poll_raw(header, &mut context)
        };
        drop(waker);
        queue_reference.finish_polling();

        match result {
            Poll::Pending => PollDisposition::Pending,
            Poll::Ready(()) => {
                self.complete_coroutine(header);
                PollDisposition::Completed
            }
        }
    }

    pub(super) fn complete_coroutine(&self, header: *mut CoroutineHeader) {
        let state = unsafe {
            // The owner holds both the permanent and current queue references.
            &(*header).state
        };
        state.fetch_or(COMPLETE, Ordering::AcqRel);
        self.unlink_active(header);
        let _owner_reference = unsafe {
            // Completion consumes the permanent owner reference even if the
            // owner-only future destructor unwinds.
            OwnedCoroutineReference::new(header)
        };
        unsafe {
            // Completion destroys the !Send future on its owner before dropping
            // the permanent owner reference.
            CoroutineHeader::drop_future_raw(header);
        }
    }

    pub(super) fn cancel_coroutine(&self, header: *mut CoroutineHeader) {
        let state = unsafe {
            // ScopedRunGuard owns a live reference and cancellation is owner-only.
            &(*header).state
        };
        if state.fetch_or(COMPLETE, Ordering::AcqRel) & COMPLETE != 0 {
            return;
        }
        self.unlink_active(header);
        let _owner_reference = unsafe {
            // Cancellation consumes the permanent owner reference even if the
            // owner-only future destructor unwinds.
            OwnedCoroutineReference::new(header)
        };
        unsafe {
            // Cancellation follows the same owner-only destructor ordering as
            // normal completion, but a queued reference may remain for later skip.
            CoroutineHeader::drop_future_raw(header);
        }
    }
}
