//! Bounded IRQ evidence consumption, watchdog handling, and staged dispatch.

use core::sync::atomic::Ordering;

use rdif_block::{
    BlkError, CompletedRequest, DmaQuiesced, QueueExecution, RecoveryCause, ServiceProgress,
};

use super::{
    DeferredCompletionSink, DispatchDisposition, DispatchSource, HardwareQueue, HardwareQueueError,
    OwnerServiceProgress, QuarantineRetention, RequestTag, RuntimeSubmitError,
};
use crate::block::{
    HctxCause, HctxPhase, ServiceBatch, ServiceBudget, ServiceContinuation,
    hctx_model::HCTX_SERVICE_BUDGET,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WatchdogProgress {
    Idle,
    TimeoutClaimed,
    DeferredForIrq,
}

fn consume_service_budget(
    budget: &mut ServiceBudget,
    operations: usize,
) -> Result<(), HardwareQueueError> {
    budget
        .consume(operations)
        .map_err(|_| HardwareQueueError::Capacity)
}

impl HardwareQueue {
    pub(in crate::block) fn service_bounded(
        &self,
    ) -> Result<OwnerServiceProgress, HardwareQueueError> {
        if ax_hal::irq::in_irq_context()
            || crate::task::current_thread_id()? != self.maintenance.owner_thread()
            || ax_hal::percpu::this_cpu_id() != self.maintenance.owner_cpu()
        {
            return Err(HardwareQueueError::WrongOwner);
        }
        let Some(_access) = self.try_driver_access() else {
            return Ok(OwnerServiceProgress::Complete);
        };
        let causes = self.control.take_service_batch();
        let services_accepted_work = self.control.services_accepted_work();
        let mut budget = ServiceBudget::new(HCTX_SERVICE_BUDGET)
            .expect("the fixed hctx service budget is valid");
        if services_accepted_work && (causes.contains(HctxCause::Irq) || !self.events.is_empty()) {
            // Consume acknowledged evidence, or run its explicitly deferred
            // acknowledged-event continuation, before a concurrent watchdog can
            // claim the same request as timed out.
            self.service_irq_events(&mut budget)?;
        }
        if budget.is_exhausted() {
            self.defer_after_irq_budget(causes);
            return Ok(OwnerServiceProgress::More);
        }
        if causes.contains(HctxCause::EventOverflow) {
            consume_service_budget(&mut budget, 1)?;
            self.begin_recovery(RecoveryCause::EventOverflow {
                queue_id: self.info.id,
            });
            return Ok(OwnerServiceProgress::Complete);
        }
        if causes.contains(HctxCause::Timeout) {
            consume_service_budget(&mut budget, 1)?;
            let cause = self.requests.timing_out_request_id().map_or(
                RecoveryCause::QueueFault {
                    queue_id: self.info.id,
                },
                |request_id| RecoveryCause::Timeout {
                    queue_id: self.info.id,
                    request_id,
                },
            );
            self.begin_recovery(cause);
            return Ok(OwnerServiceProgress::Complete);
        }
        if self.control.has_irq_or_error_pending() || !self.events.is_empty() {
            // One service call may return all 64 hctx requests, so a second
            // IRQ snapshot belongs to a fresh callback budget. Preserve every
            // lower-priority cause until that acknowledged evidence is drained.
            self.defer_after_irq_budget(causes);
            return Ok(OwnerServiceProgress::More);
        }
        if causes.contains(HctxCause::Watchdog) {
            match self.service_watchdog(&mut budget)? {
                WatchdogProgress::Idle => {}
                WatchdogProgress::TimeoutClaimed => {
                    return Ok(OwnerServiceProgress::Complete);
                }
                WatchdogProgress::DeferredForIrq => {
                    self.defer_after_terminal_budget(causes);
                    return Ok(OwnerServiceProgress::More);
                }
            }
        }
        if causes.contains(HctxCause::Cancel)
            && let Some(request_id) = self.service_cancellations(&mut budget)?
        {
            if budget.consume(1).is_err() {
                self.control.raise(HctxCause::Cancel);
                self.defer_after_terminal_budget(causes);
                return Ok(OwnerServiceProgress::More);
            }
            self.begin_recovery(RecoveryCause::Cancelled {
                queue_id: self.info.id,
                request_id,
            });
            return Ok(OwnerServiceProgress::Complete);
        }
        if !services_accepted_work {
            return Ok(OwnerServiceProgress::Complete);
        }

        if budget.is_exhausted() {
            self.defer_dispatch_causes(causes);
            return Ok(OwnerServiceProgress::More);
        }
        let mut dispatch_budget_exhausted = false;
        if causes.contains(HctxCause::Submit)
            || causes.contains(HctxCause::Shutdown)
            || self.has_staged()
        {
            dispatch_budget_exhausted = self.dispatch_staged(&mut budget)?;
        }
        if self.is_drained() {
            self.drain_wait.notify_all();
        }
        let continuation = ServiceContinuation {
            cause_pending: self.control.has_pending(),
            dispatch_budget_exhausted,
            staged_request: self.has_staged(),
            inflight_request: self.inflight.load(Ordering::Acquire) != 0,
        };
        Ok(if continuation.requires_immediate_requeue() {
            OwnerServiceProgress::More
        } else {
            OwnerServiceProgress::Complete
        })
    }

    fn defer_after_irq_budget(&self, causes: ServiceBatch) {
        self.defer_causes(
            causes,
            &[
                HctxCause::EventOverflow,
                HctxCause::Timeout,
                HctxCause::Watchdog,
                HctxCause::Cancel,
                HctxCause::Shutdown,
                HctxCause::Submit,
            ],
        );
    }

    fn defer_after_terminal_budget(&self, causes: ServiceBatch) {
        self.defer_causes(
            causes,
            &[HctxCause::Cancel, HctxCause::Shutdown, HctxCause::Submit],
        );
    }

    fn defer_dispatch_causes(&self, causes: ServiceBatch) {
        self.defer_causes(causes, &[HctxCause::Shutdown, HctxCause::Submit]);
    }

    fn defer_causes(&self, causes: ServiceBatch, deferred: &[HctxCause]) {
        for cause in deferred {
            if causes.contains(*cause) {
                self.control.raise(*cause);
            }
        }
    }

    fn service_irq_events(&self, budget: &mut ServiceBudget) -> Result<(), HardwareQueueError> {
        let Some(snapshot) = self.events.pop() else {
            return Ok(());
        };
        if !self.control.accepts_event(snapshot.epoch) {
            consume_service_budget(budget, 1)?;
            if !self.events.is_empty() {
                self.control.raise(HctxCause::Irq);
            }
            return Ok(());
        }
        let Some(events) = snapshot.event.for_queue(self.info.id) else {
            consume_service_budget(budget, 1)?;
            if !self.events.is_empty() {
                self.control.raise(HctxCause::Irq);
            }
            return Ok(());
        };

        // The portable queue API can return every one of this hctx's 64
        // outstanding requests from a single event. Invoking it at most once
        // per callback makes that worst case fit the callback-wide budget;
        // A typed continuation retains this exact source epoch for the next
        // pass; ordinary driver Busy cannot synthesize completion polling.
        // Construct the sink before the driver lease so unwinding restores the
        // endpoint first, then lets the sink's Drop publish waiter wakeups.
        let mut completions = DeferredCompletionSink::new(self);
        let mut driver = self.take_driver_on_owner()?;
        let progress = driver.service_events(&events, &mut completions);
        drop(driver);
        let delivery = completions.finish();
        let budget_result = consume_service_budget(budget, delivery.completed.max(1));
        let has_continuation = matches!(progress, Ok(ServiceProgress::Requeue(_)));
        let continuation_result = match &progress {
            Ok(ServiceProgress::Requeue(continuation))
                if continuation.source_id() == snapshot.event.source_id()
                    && continuation.source_epoch() == snapshot.event.epoch() =>
            {
                self.events.try_push(snapshot).map_err(|_| {
                    self.control.raise(HctxCause::EventOverflow);
                    HardwareQueueError::Capacity
                })
            }
            Ok(ServiceProgress::Requeue(_)) => Err(HardwareQueueError::StaleIrqEvent),
            Ok(ServiceProgress::Idle) | Err(_) => Ok(()),
        };
        if has_continuation || !self.events.is_empty() {
            self.control.raise(HctxCause::Irq);
        }

        if let Some(error) = delivery.error {
            return Err(error);
        }
        budget_result?;
        continuation_result?;
        progress.map(|_| ()).map_err(Into::into)
    }

    pub(super) fn publish_one_completion(
        &self,
        tag: RequestTag,
        completion: rdif_block::CompletedRequest,
    ) -> Result<(), HardwareQueueError> {
        let was_inflight = self.install_completion_for_delivery(tag, completion)?;
        self.finish_installed_completion(tag, was_inflight);
        Ok(())
    }

    pub(super) fn install_completion_for_delivery(
        &self,
        tag: RequestTag,
        completion: rdif_block::CompletedRequest,
    ) -> Result<bool, HardwareQueueError> {
        let was_inflight = match self.requests.install_completion(tag, completion) {
            Ok(was_inflight) => was_inflight,
            Err(rejected) => return Err(self.retain_failed_publication(rejected)),
        };
        Ok(was_inflight)
    }

    pub(super) fn finish_installed_completion(&self, tag: RequestTag, was_inflight: bool) {
        if was_inflight {
            let previous = self.inflight.fetch_sub(1, Ordering::AcqRel);
            assert!(previous != 0, "block hctx inflight count underflowed");
        }
        self.finish_accepted_request();
        self.requests.notify_completion(tag);
    }

    fn retain_failed_publication(
        &self,
        rejected: super::CompletionPublicationError,
    ) -> HardwareQueueError {
        let retention = self
            .rejected_completions
            .lock()
            .as_mut()
            .expect("live hctx retains its rejected completion quarantine")
            .retain(rejected);
        self.finish_failed_completion_retention(retention)
    }

    pub(super) fn retain_failed_completion(
        &self,
        error: HardwareQueueError,
        completion: CompletedRequest,
    ) -> HardwareQueueError {
        let retention = self
            .rejected_completions
            .lock()
            .as_mut()
            .expect("live hctx retains its rejected completion quarantine")
            .retain_completion(error, completion);
        self.finish_failed_completion_retention(retention)
    }

    fn finish_failed_completion_retention(
        &self,
        retention: QuarantineRetention,
    ) -> HardwareQueueError {
        match retention {
            QuarantineRetention::Retained(error) => {
                self.enter_fatal_completion_quarantine();
                error
            }
            QuarantineRetention::Excess { error, completion } => {
                // All 64 possible accepted owners are already retained. This
                // additional rejected value cannot be a distinct accepted
                // owner, so after publishing the fatal recovery transition its
                // ordinary Rust Drop is the only valid ownership operation.
                self.enter_fatal_completion_quarantine();
                error!(
                    "block hctx {} dropped a driver-fabricated excess completion after: {error}",
                    self.info.id
                );
                drop(completion);
                HardwareQueueError::Capacity
            }
        }
    }

    fn enter_fatal_completion_quarantine(&self) {
        if self
            .fatal_completion_quarantine
            .swap(true, Ordering::AcqRel)
        {
            return;
        }

        // Stop submission and IRQ publication first. The fixed controller
        // maintenance owner then masks the device, drains every OS IRQ action,
        // proves DMA quiescence, releases ordinary quarantine entries, and only
        // afterwards commits Offline. This ordering prevents another driver
        // callback from creating an unbounded retention path.
        self.access_gate.close();
        let _transition = self.control.begin_recovery();
        self.drain_wait.notify_all();
        if !self
            .controller_link
            .request_recovery(RecoveryCause::QueueFault {
                queue_id: self.info.id,
            })
        {
            panic!("fatal block completion quarantine has no controller recovery owner");
        }
    }

    fn service_watchdog(
        &self,
        budget: &mut ServiceBudget,
    ) -> Result<WatchdogProgress, HardwareQueueError> {
        let Some(cutoff) = self.terminal_gate.try_begin_terminal() else {
            self.control.raise(HctxCause::Watchdog);
            return Ok(WatchdogProgress::DeferredForIrq);
        };
        if self.control.has_irq_or_error_pending() || !self.events.is_empty() {
            drop(cutoff);
            self.control.raise(HctxCause::Watchdog);
            return Ok(WatchdogProgress::DeferredForIrq);
        }

        let now_ns = ax_hal::time::monotonic_time_nanos();
        if let Some(tag) = self.requests.first_expired(now_ns)
            && self.claim_timeout(tag)?
        {
            consume_service_budget(budget, 1)?;
            drop(cutoff);
            return Ok(WatchdogProgress::TimeoutClaimed);
        }
        drop(cutoff);
        Ok(WatchdogProgress::Idle)
    }

    fn service_cancellations(
        &self,
        budget: &mut ServiceBudget,
    ) -> Result<Option<rdif_block::RequestId>, HardwareQueueError> {
        while !budget.is_exhausted() {
            let Some(tag) = self.requests.first_canceling_staged() else {
                break;
            };
            self.remove_staged_tag(tag)?;
            let completion = self.requests.complete_canceling_staged(tag)?;
            self.publish_one_completion(tag, completion)?;
            consume_service_budget(budget, 1)?;
        }

        if self.requests.first_canceling_staged().is_some() {
            self.control.raise(HctxCause::Cancel);
        }
        Ok(self.requests.canceling_inflight_request_id())
    }

    fn remove_staged_tag(&self, tag: RequestTag) -> Result<(), HardwareQueueError> {
        let mut removed = usize::from(self.dispatch_list.lock().remove(tag));
        for context in &self.software_contexts {
            removed += usize::from(context.lock().remove(tag));
        }
        if removed == 1 {
            Ok(())
        } else {
            Err(HardwareQueueError::RequestState)
        }
    }

    fn dispatch_staged(&self, budget: &mut ServiceBudget) -> Result<bool, HardwareQueueError> {
        let mut driver = self.take_driver_on_owner()?;
        while !budget.is_exhausted() {
            let Some(tag) = self.next_dispatch_tag() else {
                break;
            };
            let mut result = self.dispatch_one_locked(tag, &mut driver)?;
            match result.disposition {
                DispatchDisposition::Queued => {
                    consume_service_budget(budget, 1)?;
                    if let Some(error) = result.take_recovery_error() {
                        self.record_service_error(&error);
                        return Ok(false);
                    }
                    if self.info.execution == QueueExecution::Serialized {
                        break;
                    }
                }
                DispatchDisposition::Terminal => {
                    let (completion, recovery_error) = result.take_terminal()?;
                    drop(driver);
                    self.publish_one_completion(tag, completion)?;
                    if let Some(error) = recovery_error {
                        self.record_service_error(&error);
                        return Ok(false);
                    }
                    consume_service_budget(budget, 1)?;
                    driver = self.take_driver_on_owner()?;
                }
                DispatchDisposition::Deferred => {
                    self.dispatch_list.lock().push(tag)?;
                    break;
                }
            }
        }
        if self.has_staged() && self.inflight.load(Ordering::Acquire) == 0 {
            return Err(HardwareQueueError::Driver(BlkError::Busy));
        }
        Ok(budget.is_exhausted() && self.has_staged())
    }

    fn next_dispatch_tag(&self) -> Option<RequestTag> {
        let hardware_ready = !self.dispatch_list.lock().is_empty();
        let software_ready =
            core::array::from_fn(|cpu| !self.software_contexts[cpu].lock().is_empty());
        let source = self
            .dispatch_arbiter
            .lock()
            .select(hardware_ready, &software_ready)?;
        match source {
            DispatchSource::HardwareDispatchList => self.dispatch_list.lock().pop(),
            DispatchSource::Cpu(cpu) => self.software_contexts[cpu].lock().pop(),
        }
    }

    pub(super) fn has_staged(&self) -> bool {
        !self.dispatch_list.lock().is_empty()
            || self
                .software_contexts
                .iter()
                .any(|context| !context.lock().is_empty())
    }

    pub(super) fn reserve_submission(
        &self,
        request: rdif_block::OwnedRequest,
    ) -> Result<RequestTag, RuntimeSubmitError> {
        if !self.control.accepts_submission() {
            return Err(RuntimeSubmitError::new(
                HardwareQueueError::Offline,
                request,
            ));
        }
        self.accepted_requests.fetch_add(1, Ordering::AcqRel);
        if !self.control.accepts_submission() {
            self.finish_accepted_request();
            return Err(RuntimeSubmitError::new(
                HardwareQueueError::Offline,
                request,
            ));
        }
        match self.requests.reserve(request) {
            Ok(tag) => Ok(tag),
            Err(error) => {
                self.finish_accepted_request();
                Err(error)
            }
        }
    }

    pub(super) fn finish_accepted_request(&self) {
        let previous = self.accepted_requests.fetch_sub(1, Ordering::AcqRel);
        assert!(
            previous != 0,
            "block hctx accepted request count underflowed"
        );
        if previous == 1 && self.control.phase() == HctxPhase::Quiescing {
            self.drain_wait.notify_all();
        }
    }

    pub(super) fn is_drained(&self) -> bool {
        self.accepted_requests.load(Ordering::Acquire) == 0
            && self.inflight.load(Ordering::Acquire) == 0
            && !self.has_staged()
            && self.events.is_empty()
            && !self.control.has_pending()
    }

    fn begin_recovery(&self, cause: RecoveryCause) {
        if matches!(
            self.control.phase(),
            HctxPhase::Running | HctxPhase::Quiescing
        ) && self.control.begin_recovery().is_ok()
        {
            self.drain_wait.notify_all();
            assert!(
                self.controller_link.request_recovery(cause),
                "block recovery lost its shutdown-lifetime controller owner"
            );
        }
    }

    pub(super) fn record_irq_service_error(&self, error: &HardwareQueueError) {
        let code = match error {
            HardwareQueueError::Capacity | HardwareQueueError::EventOverflow { .. } => 1,
            HardwareQueueError::StaleCompletion
            | HardwareQueueError::SynchronousCompletion
            | HardwareQueueError::RequestState => 2,
            HardwareQueueError::Driver(_) => 3,
            _ => 4,
        };
        self.service_error.store(code, Ordering::Release);
        if matches!(
            self.control.phase(),
            HctxPhase::Running | HctxPhase::Quiescing
        ) {
            let _ = self.control.begin_recovery();
        }
        self.access_gate.close();
        assert!(
            self.controller_link.request_irq_recovery(self.info.id),
            "block IRQ recovery lost its shutdown-lifetime controller owner"
        );
    }

    pub(super) fn record_service_error(&self, error: &HardwareQueueError) {
        self.record_irq_service_error(error);
        error!("block hctx {} entered recovery: {error}", self.info.id);
    }

    pub(in crate::block) fn reclaim_after_quiesce(
        &self,
        proof: &DmaQuiesced,
    ) -> Result<(), HardwareQueueError> {
        self.rejected_completions
            .lock()
            .as_mut()
            .expect("live hctx retains its rejected completion quarantine")
            .release_after_dma_quiesce(proof)?;
        // Construct the sink first so a driver panic restores its endpoint
        // lease before Drop publishes terminal notifications.
        let mut completions = DeferredCompletionSink::new(self);
        let driver_result = self
            .take_driver_on_owner()
            .map_err(|_| BlkError::Offline)
            .and_then(|mut driver| driver.reclaim_after_quiesce(proof, &mut completions));

        self.dispatch_list.lock().clear();
        for context in &self.software_contexts {
            context.lock().clear();
        }
        let runtime_result = self.requests.reclaim_runtime_owned(&mut completions);
        while self.events.pop().is_some() {}
        let _stale_causes = self.control.take_service_batch();
        let delivery = completions.finish();
        if let Some(error) = delivery.error {
            return Err(error);
        }
        runtime_result?;
        driver_result.map_err(HardwareQueueError::from)?;
        if self.accepted_requests.load(Ordering::Acquire) != 0
            || self.inflight.load(Ordering::Acquire) != 0
        {
            return Err(HardwareQueueError::StaleCompletion);
        }
        if self.fatal_completion_quarantine.load(Ordering::Acquire) {
            return Err(HardwareQueueError::Capacity);
        }
        Ok(())
    }
}
