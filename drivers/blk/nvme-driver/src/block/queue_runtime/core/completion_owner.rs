use super::*;

impl NvmeQueueCore {
    pub(super) fn completion_failed(&self) -> bool {
        self.completion_fault.load(Ordering::Acquire)
    }

    pub(super) fn submit_command(&self, command: crate::queue::CommandSet) {
        // SAFETY: RDIF gives one CPU-pinned maintenance owner to this queue,
        // so SQ and CQ mutation are serialized in the same domain.
        self.queue.submit_io_data(command);
    }

    pub(super) fn drain_owner_completions(&self, budget: usize) -> CompletionDrain {
        // SAFETY: `IQueue::service_events` is called only by the queue's
        // CPU-pinned maintenance owner. The acknowledged source remains
        // device-masked, and lifecycle reset cannot run until that owner has
        // closed queue access and drained the IRQ action.
        // SAFETY: the maintenance owner is the only live queue accessor. A
        // lifecycle reset can touch this cache only after that owner and its
        // IRQ action have been drained.
        let cache = unsafe { &mut *self.completion_cache.get() };
        let drain = drain_owner_completions_to_cache(self.queue.as_ref(), cache, budget);
        self.publish_completion_drain(drain);
        drain
    }

    pub(super) fn emit_owner_cached_completions(
        &self,
        budget: usize,
        sink: &mut dyn CompletionSink,
    ) -> Result<Option<usize>, BlkError> {
        let Some(mut state) = self.try_claim_state() else {
            return Ok(None);
        };
        // SAFETY: only the CPU-pinned maintenance owner emits retained
        // completions. The state claim excludes proof-gated lifecycle reset
        // while request ownership is transferred to the sink.
        let cache = unsafe { &mut *self.completion_cache.get() };
        state
            .emit_cached_completions(self.id, cache, budget, sink)
            .map(Some)
    }

    pub(in crate::block) fn service_claimed_evidence(
        &self,
        budget: usize,
        sink: &mut dyn CompletionSink,
    ) -> Result<NvmeQueueEvidenceProgress, BlkError> {
        if self.completion_failed() {
            return Err(BlkError::Io);
        }
        if budget == 0 {
            return Ok(NvmeQueueEvidenceProgress { retained: true });
        }

        let emitted = self
            .emit_owner_cached_completions(budget, sink)?
            .ok_or(BlkError::Busy)?;
        let remaining = budget.saturating_sub(emitted);
        if remaining == 0 {
            return Ok(NvmeQueueEvidenceProgress { retained: true });
        }

        let drain = self.drain_owner_completions(remaining);
        if self.completion_failed() {
            return Err(BlkError::Io);
        }
        let emitted_after_drain = self
            .emit_owner_cached_completions(remaining, sink)?
            .ok_or(BlkError::Busy)?;
        let completed = emitted + emitted_after_drain;
        Ok(NvmeQueueEvidenceProgress {
            retained: drain.may_have_more || completed == budget || self.service_pending(),
        })
    }

    pub(super) fn service_pending(&self) -> bool {
        // SAFETY: this query runs in the same maintenance-owner scope as
        // completion drain and publication. Lifecycle reset is allowed only
        // after that scope and its IRQ action have been drained.
        let cache = unsafe { &*self.completion_cache.get() };
        cache.has_ready() || self.completion_fault.load(Ordering::Acquire)
    }

    pub(super) fn clear_service_state_after_quiesce(&self) {
        // SAFETY: the caller presents the controller's DMA-quiesced proof and
        // has already reclaimed every accepted request from the owner scope.
        let cache = unsafe { &mut *self.completion_cache.get() };
        cache.clear_after_quiesce();
        self.completion_fault.store(false, Ordering::Release);
    }

    pub(in crate::block) fn reclaim_requests_after_quiesce(
        &self,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        let Some(mut state) = self.try_claim_state() else {
            return Err(BlkError::Busy);
        };
        state.cancel_all(sink);
        drop(state);
        self.clear_service_state_after_quiesce();
        Ok(())
    }

    pub(in crate::block) fn shutdown(&self) -> Result<(), BlkError> {
        let Some(state) = self.try_claim_state() else {
            return Err(BlkError::Busy);
        };
        if state.has_accepted() {
            return Err(BlkError::Busy);
        }
        drop(state);
        if self.completion_failed() {
            return Err(BlkError::Io);
        }
        if self.service_pending() {
            return Err(BlkError::Busy);
        }
        Ok(())
    }

    fn publish_completion_drain(&self, drain: CompletionDrain) {
        if drain.invalid {
            self.completion_fault.store(true, Ordering::Release);
        }
    }

    /// Resets retained queue state after the controller stopped DMA.
    ///
    /// # Safety
    ///
    /// The caller must hold the controller's DmaQuiesced proof and keep hctx
    /// driver access plus every IRQ action drained for the duration.
    pub(in crate::block) unsafe fn reset_after_quiesce(&self) -> Result<(), InitError> {
        if self
            .state_claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(InitError::Hardware(
                "NVMe request state remained claimed after hctx drain",
            ));
        }
        // SAFETY: the successful claim grants exclusive request ownership
        // while the runtime keeps hctx access closed.
        let state = unsafe { &mut *self.state.get() };
        if state.has_accepted() {
            self.state_claimed.store(false, Ordering::Release);
            return Err(InitError::Hardware(
                "NVMe request ownership was not reclaimed before reset",
            ));
        }
        // SAFETY: controller RDY is zero, the request-state claim is held, and
        // the maintenance owner has already drained its IRQ action and queue
        // access, so retained queue memory has no concurrent accessor.
        // SAFETY: the method contract and owner claim exclude every device,
        // task, and IRQ access to retained queue memory.
        unsafe { self.queue.reset_after_controller_disable() };
        if !state.advance_cid_epoch_after_quiesce() {
            self.state_claimed.store(false, Ordering::Release);
            return Err(InitError::Hardware("NVMe CID queue epoch exhausted"));
        }
        // SAFETY: the method contract excludes device, task, and IRQ access.
        let cache = unsafe { &mut *self.completion_cache.get() };
        cache.clear_after_quiesce();
        self.completion_fault.store(false, Ordering::Release);
        self.state_claimed.store(false, Ordering::Release);
        Ok(())
    }
}
