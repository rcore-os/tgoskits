//! Bounded IRQ evidence consumption, watchdog handling, and staged dispatch.

use core::sync::atomic::Ordering;

use rdif_block::{
    BlkError, CompletedRequest, CompletionSink, DispatchMode, DmaQuiesced, RecoveryCause,
    ServiceProgress,
};

use super::{
    CompletionBatch, DispatchDisposition, DispatchSource, HardwareQueue, HardwareQueueError,
    QuarantineRetention, RequestTag, RuntimeSubmitError,
};
use crate::{
    block::{
        HctxCause, HctxPhase, ServiceBatch, ServiceBudget, ServiceContinuation,
        hctx_model::HCTX_SERVICE_BUDGET,
    },
    workqueue::{WorkOutcome, WorkQueueError},
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
    pub(super) fn service_bounded(&'static self) -> Result<WorkOutcome, HardwareQueueError> {
        let causes = self.control.take_service_batch();
        let services_accepted_work = self.control.services_accepted_work();
        let mut budget = ServiceBudget::new(HCTX_SERVICE_BUDGET)
            .expect("the fixed hctx service budget is valid");
        if services_accepted_work && (causes.contains(HctxCause::Irq) || !self.events.is_empty()) {
            // Consume acknowledged evidence, or run its explicitly deferred
            // acknowledgement continuation, before a concurrent watchdog can
            // claim the same request as timed out.
            self.service_irq_events(&mut budget)?;
        }
        if budget.is_exhausted() {
            self.defer_after_irq_budget(causes);
            return Ok(WorkOutcome::Requeue);
        }
        if causes.contains(HctxCause::EventOverflow) {
            consume_service_budget(&mut budget, 1)?;
            self.begin_recovery(RecoveryCause::EventOverflow {
                queue_id: self.info.id,
            });
            return Ok(WorkOutcome::Complete);
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
            return Ok(WorkOutcome::Complete);
        }
        if self.control.has_irq_or_error_pending() || !self.events.is_empty() {
            // One service call may return all 64 hctx requests, so a second
            // IRQ snapshot belongs to a fresh callback budget. Preserve every
            // lower-priority cause until that acknowledged evidence is drained.
            self.defer_after_irq_budget(causes);
            return Ok(WorkOutcome::Requeue);
        }
        if causes.contains(HctxCause::Watchdog) {
            match self.service_watchdog(&mut budget)? {
                WatchdogProgress::Idle => {}
                WatchdogProgress::TimeoutClaimed => return Ok(WorkOutcome::Complete),
                WatchdogProgress::DeferredForIrq => {
                    self.defer_after_terminal_budget(causes);
                    return Ok(WorkOutcome::Requeue);
                }
            }
        }
        if causes.contains(HctxCause::Cancel)
            && let Some(request_id) = self.service_cancellations(&mut budget)?
        {
            if budget.consume(1).is_err() {
                self.control.raise(HctxCause::Cancel);
                self.defer_after_terminal_budget(causes);
                return Ok(WorkOutcome::Requeue);
            }
            self.begin_recovery(RecoveryCause::Cancelled {
                queue_id: self.info.id,
                request_id,
            });
            return Ok(WorkOutcome::Complete);
        }
        if !services_accepted_work {
            return Ok(WorkOutcome::Complete);
        }

        if budget.is_exhausted() {
            self.defer_dispatch_causes(causes);
            return Ok(WorkOutcome::Requeue);
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
        self.refresh_watchdog()?;
        let continuation = ServiceContinuation {
            cause_pending: self.control.has_pending(),
            dispatch_budget_exhausted,
            staged_request: self.has_staged(),
            inflight_request: self.inflight.load(Ordering::Acquire) != 0,
        };
        Ok(if continuation.requires_immediate_requeue() {
            WorkOutcome::Requeue
        } else {
            WorkOutcome::Complete
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
        let mut completions = CompletionBatch::new();
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
        let progress = self.queue.lock().service_events(&events, &mut completions);
        let completed = completions.len;
        let budget_result = consume_service_budget(budget, completed.max(1));
        let has_continuation = matches!(progress, Ok(ServiceProgress::Continue(_)));
        let continuation_result = match &progress {
            Ok(ServiceProgress::Continue(continuation))
                if continuation.source_id() == snapshot.event.source_id()
                    && continuation.source_epoch() == snapshot.event.epoch() =>
            {
                self.events.try_push(snapshot).map_err(|_| {
                    self.control.raise(HctxCause::EventOverflow);
                    HardwareQueueError::Capacity
                })
            }
            Ok(ServiceProgress::Continue(_)) => Err(HardwareQueueError::StaleIrqEvent),
            Ok(ServiceProgress::Idle) | Err(_) => Ok(()),
        };
        if has_continuation || !self.events.is_empty() {
            self.control.raise(HctxCause::Irq);
        }

        // After `service_events` returns, every value in `completions` is an
        // ownership transfer from the driver. No later accounting or
        // continuation error may return before those owners are published or
        // moved into the DMA-proof-gated quarantine.
        let completion_result = completions.drain_with(|completion| {
            let tag = match RequestTag::from_request_id(completion.id) {
                Ok(tag) => tag,
                Err(error) => {
                    return Err(self.retain_failed_completion(error.into(), completion));
                }
            };
            self.publish_one_completion(tag, completion)
        });
        let overflow_result = completions.take_overflow().map(|completion| {
            self.retain_failed_completion(HardwareQueueError::Capacity, completion)
        });
        completion_result?;
        if let Some(error) = overflow_result {
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
        let was_inflight = match self.requests.publish_completion(tag, completion) {
            Ok(was_inflight) => was_inflight,
            Err(rejected) => return Err(self.retain_failed_publication(rejected)),
        };
        if was_inflight {
            let previous = self.inflight.fetch_sub(1, Ordering::AcqRel);
            assert!(previous != 0, "block hctx inflight count underflowed");
        }
        self.finish_accepted_request();
        Ok(())
    }

    fn retain_failed_publication(
        &self,
        rejected: super::CompletionPublicationError,
    ) -> HardwareQueueError {
        let retention = self.rejected_completions.lock().retain(rejected);
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
            .retain_completion(error, completion);
        self.finish_failed_completion_retention(retention)
    }

    fn finish_failed_completion_retention(
        &self,
        retention: QuarantineRetention,
    ) -> HardwareQueueError {
        let (error, poisoned) = match retention {
            QuarantineRetention::Retained(error) => (error, false),
            QuarantineRetention::Poisoned(error) => (error, true),
        };
        self.enter_fatal_completion_quarantine();
        if poisoned {
            error!(
                "block hctx {} retained its shutdown-lifetime poison completion after: {error}",
                self.info.id
            );
            HardwareQueueError::Capacity
        } else {
            error
        }
    }

    fn enter_fatal_completion_quarantine(&self) {
        if self
            .fatal_completion_quarantine
            .swap(true, Ordering::AcqRel)
        {
            return;
        }

        // Stop submission and IRQ publication first. The controller recovery
        // worker then masks the device, drains every OS IRQ action, proves DMA
        // quiescence, releases ordinary quarantine entries, and only afterwards
        // commits Offline. This ordering prevents another completion batch from
        // creating an unbounded retention path.
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
        &'static self,
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
        self.refresh_watchdog()?;
        Ok(WatchdogProgress::Idle)
    }

    fn service_cancellations(
        &self,
        budget: &mut ServiceBudget,
    ) -> Result<Option<rdif_block::RequestId>, HardwareQueueError> {
        let mut completions = CompletionBatch::new();
        {
            // The driver gate serializes cancellation claims with the exact
            // submit boundary. Once a staged tag is removed here, no driver
            // call can observe it.
            let _driver = self.queue.lock();
            while !budget.is_exhausted() {
                let Some(tag) = self.requests.first_canceling_staged() else {
                    break;
                };
                self.remove_staged_tag(tag)?;
                completions.complete(self.requests.complete_canceling_staged(tag)?);
                consume_service_budget(budget, 1)?;
            }
        }

        let completion_result = completions.drain_with(|completion| {
            let tag = match RequestTag::from_request_id(completion.id) {
                Ok(tag) => tag,
                Err(error) => {
                    return Err(self.retain_failed_completion(error.into(), completion));
                }
            };
            self.publish_one_completion(tag, completion)
        });
        let overflow_result = completions.take_overflow().map(|completion| {
            self.retain_failed_completion(HardwareQueueError::Capacity, completion)
        });
        completion_result?;
        if let Some(error) = overflow_result {
            return Err(error);
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

    fn refresh_watchdog(&'static self) -> Result<(), HardwareQueueError> {
        if !self.control.services_accepted_work() {
            return Ok(());
        }
        let Some(deadline_ns) = self.requests.earliest_deadline() else {
            return Ok(());
        };
        let now_ns = ax_hal::time::monotonic_time_nanos();
        let delay_ns = deadline_ns.saturating_sub(now_ns);
        match self.work_domain.mod_delayed_work_on(
            self.affinity_cpu(),
            self.watchdog_work(),
            delay_ns,
        ) {
            Ok(_) | Err(WorkQueueError::DelayedWorkBusy) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn dispatch_staged(
        &'static self,
        budget: &mut ServiceBudget,
    ) -> Result<bool, HardwareQueueError> {
        let mut driver = self.queue.lock();
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
                    if self.info.dispatch_mode == DispatchMode::Serialized {
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
                    driver = self.queue.lock();
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

    fn begin_recovery(&'static self, cause: RecoveryCause) {
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

    pub(super) fn record_irq_service_error(&'static self, error: &HardwareQueueError) {
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

    pub(super) fn record_service_error(&'static self, error: &HardwareQueueError) {
        self.record_irq_service_error(error);
        error!("block hctx {} entered recovery: {error}", self.info.id);
    }

    pub(in crate::block) fn reclaim_after_quiesce(
        &self,
        proof: &DmaQuiesced,
    ) -> Result<(), HardwareQueueError> {
        self.rejected_completions
            .lock()
            .release_after_dma_quiesce(proof)?;
        let mut completions = CompletionBatch::new();
        let driver_result = self
            .queue
            .lock()
            .reclaim_after_quiesce(proof, &mut completions);

        self.dispatch_list.lock().clear();
        for context in &self.software_contexts {
            context.lock().clear();
        }
        let runtime_result = self.requests.reclaim_runtime_owned(&mut completions);
        while self.events.pop().is_some() {}
        let _stale_causes = self.control.take_service_batch();

        let completion_result = completions.drain_with(|completion| {
            let tag = match RequestTag::from_request_id(completion.id) {
                Ok(tag) => tag,
                Err(error) => {
                    return Err(self.retain_failed_completion(error.into(), completion));
                }
            };
            self.publish_one_completion(tag, completion)
        });
        let overflow_result = completions.take_overflow().map(|completion| {
            self.retain_failed_completion(HardwareQueueError::Capacity, completion)
        });
        completion_result?;
        if let Some(error) = overflow_result {
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
