use alloc::{
    boxed::Box,
    collections::{BTreeMap, VecDeque},
    format,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use rdif_block::{
    BatchSubmitDisposition, BlkError, CompletedRequest, CompletionSink, ControllerEvent,
    HardwareQueue, OwnedRequest, OwnedRequestBatch, QueueInfo, RequestFlags, RequestId, RequestOp,
    SubmissionSink,
};

use super::{
    channel::BoundedChannel,
    completion::CompletionSender,
    irq::{IrqEventLatch, IrqTarget},
};
use crate::os::{BlockNotification, BlockThread, runtime_ops, sync::IrqMutex, wall_time};

#[cfg(not(test))]
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const REQUEST_TIMEOUT: Duration = Duration::from_millis(100);

pub(super) trait HctxObserver: Send + Sync {
    fn request_completed(&self, op: RequestOp, block_count: u32, result: Result<(), BlkError>);

    fn hctx_failed(&self, hctx_id: usize, error: BlkError);
}

pub(super) trait ControllerEventPort: Send + Sync {
    fn post(&self, event: ControllerEvent);
}

pub(super) struct Submission {
    pub(super) request: OwnedRequest,
    pub(super) completion: CompletionSender,
}

pub(super) struct Hctx {
    id: usize,
    cpu: usize,
    info: QueueInfo,
    state: Arc<HctxState>,
    thread: IrqMutex<Option<Box<dyn BlockThread>>>,
}

struct HctxState {
    submission_channels: IrqMutex<Vec<Arc<BoundedChannel<Submission>>>>,
    notification: Arc<dyn BlockNotification>,
    irq_latches: IrqMutex<Vec<Arc<IrqEventLatch>>>,
    stopping: AtomicBool,
}

struct PendingRequest {
    completion: CompletionSender,
    op: RequestOp,
    block_count: u32,
    deadline: Duration,
}

struct SubmissionMetadata {
    completion: CompletionSender,
    op: RequestOp,
    block_count: u32,
}

struct AcceptedRequestIds {
    ids: Vec<RequestId>,
}

impl SubmissionSink for AcceptedRequestIds {
    fn accepted(&mut self, id: RequestId) {
        self.ids.push(id);
    }
}

struct SubmissionLoop<'a> {
    state: &'a HctxState,
    pending: &'a mut BTreeMap<RequestId, PendingRequest>,
    retry_submissions: &'a mut VecDeque<Submission>,
    protocol_failed: &'a mut Vec<PendingRequest>,
    fatal_error: &'a mut Option<BlkError>,
    next_channel: &'a mut usize,
}

struct SubmissionReconciliation<'a> {
    deadline: Duration,
    pending: &'a mut BTreeMap<RequestId, PendingRequest>,
    retry_submissions: &'a mut VecDeque<Submission>,
    protocol_failed: &'a mut Vec<PendingRequest>,
}

impl Hctx {
    pub(super) fn start(
        queue: Box<dyn HardwareQueue>,
        cpu: usize,
        observer: Weak<dyn HctxObserver>,
        controller: Arc<dyn ControllerEventPort>,
    ) -> Result<Arc<Self>, BlkError> {
        let info = queue.info();
        if info.limits.max_inflight == 0
            || info.limits.max_submit_batch == 0
            || info.limits.max_submit_batch > info.limits.max_inflight
            || info.id >= u64::BITS as usize
        {
            return Err(BlkError::InvalidRequest);
        }

        let ops =
            runtime_ops().map_err(|_| BlkError::Other("block runtime adapter is not installed"))?;
        let notification = ops.notification();
        let state = Arc::new(HctxState {
            submission_channels: IrqMutex::new(Vec::new()),
            notification,
            irq_latches: IrqMutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
        });
        let hctx = Arc::new(Self {
            id: info.id,
            cpu,
            info,
            state: Arc::clone(&state),
            thread: IrqMutex::new(None),
        });
        let name = format!("blk-hctx/{}", info.id);
        let thread = ops
            .spawn_pinned(
                name,
                cpu,
                Box::new(move || run_hctx(queue, state, observer, controller)),
            )
            .map_err(|_| BlkError::NoMemory)?;
        *hctx.thread.lock() = Some(thread);
        info!(
            "block hctx {} bound to CPU {} with hardware depth {}",
            info.id, cpu, info.limits.max_inflight
        );
        Ok(hctx)
    }

    pub(super) const fn id(&self) -> usize {
        self.id
    }

    pub(super) const fn cpu(&self) -> usize {
        self.cpu
    }

    pub(super) const fn info(&self) -> QueueInfo {
        self.info
    }

    pub(super) fn add_submission_channel(
        &self,
    ) -> Result<Arc<BoundedChannel<Submission>>, BlkError> {
        let channel = Arc::new(
            BoundedChannel::with_item_notification(
                self.info.limits.max_inflight,
                Arc::clone(&self.state.notification),
            )
            .map_err(|_| BlkError::NoMemory)?,
        );
        self.state
            .submission_channels
            .lock()
            .push(Arc::clone(&channel));
        self.state.notification.notify();
        Ok(channel)
    }

    pub(super) fn irq_target(&self, source_id: usize) -> IrqTarget {
        let latch = Arc::new(IrqEventLatch::new(source_id));
        self.state.irq_latches.lock().push(Arc::clone(&latch));
        IrqTarget::new(self.id, latch, Arc::clone(&self.state.notification))
    }

    pub(super) fn stop(&self) {
        if !self.state.stopping.swap(true, Ordering::AcqRel) {
            for channel in self.state.submission_channels.lock().iter() {
                channel.close();
            }
            self.state.notification.notify();
        }
        // Drop the IRQ-disabling slot guard before `join`, which may sleep.
        let thread = self.thread.lock().take();
        if let Some(thread) = thread {
            thread.join();
        }
    }
}

fn run_hctx(
    mut queue: Box<dyn HardwareQueue>,
    state: Arc<HctxState>,
    observer: Weak<dyn HctxObserver>,
    controller: Arc<dyn ControllerEventPort>,
) {
    let mut pending = BTreeMap::new();
    let mut protocol_failed = Vec::new();
    let mut retry_submissions = VecDeque::new();
    let mut fatal_error = None;
    let mut next_channel = 0;

    while !state.stopping.load(Ordering::Acquire) {
        let irq_progress = drain_latched_irqs(
            &mut *queue,
            &state,
            &mut pending,
            &observer,
            &*controller,
            &mut fatal_error,
        );
        let submit_progress = submit_available(
            &mut *queue,
            SubmissionLoop {
                state: &state,
                pending: &mut pending,
                retry_submissions: &mut retry_submissions,
                protocol_failed: &mut protocol_failed,
                fatal_error: &mut fatal_error,
                next_channel: &mut next_channel,
            },
        );

        if fatal_error.is_none() && pending_deadline_expired(&pending, wall_time()) {
            fatal_error = Some(BlkError::TimedOut);
            state.stopping.store(true, Ordering::Release);
        }
        if state.stopping.load(Ordering::Acquire) {
            break;
        }
        if irq_progress || submit_progress {
            continue;
        }
        if let Some(deadline) = next_pending_deadline(&pending) {
            let now = wall_time();
            if deadline <= now {
                continue;
            }
            state.notification.wait_timeout(deadline - now);
        } else {
            state.notification.wait();
        }
    }

    let mut unexpected_completion = false;
    let shutdown_result = {
        let mut sink = HctxCompletionSink {
            pending: &mut pending,
            observer: &observer,
            override_error: fatal_error,
            unexpected_completion: &mut unexpected_completion,
        };
        queue.shutdown(&mut sink)
    };
    if (shutdown_result.is_err() || unexpected_completion) && fatal_error.is_none() {
        fatal_error = Some(BlkError::Io);
    }
    if let Some(error) = fatal_error {
        if let Some(observer) = observer.upgrade() {
            observer.hctx_failed(queue.id(), error);
        }
        controller.post(ControllerEvent::Watchdog {
            queue_id: queue.id(),
        });
    }
    while let Some(submission) = retry_submissions.pop_front() {
        reject_unsubmitted(submission, &observer);
    }
    for channel in state.submission_channels.lock().iter() {
        while let Some(submission) = channel.try_recv() {
            reject_unsubmitted(submission, &observer);
        }
    }
    let terminal_error = fatal_error.unwrap_or(BlkError::Io);
    for (_, request) in pending {
        request.completion.complete(CompletedRequest::new(
            RequestId::new(usize::MAX),
            Err(terminal_error),
            None,
        ));
        notify_observer(
            &observer,
            request.op,
            request.block_count,
            Err(terminal_error),
        );
    }
    for request in protocol_failed {
        request.completion.complete(CompletedRequest::new(
            RequestId::new(usize::MAX),
            Err(terminal_error),
            None,
        ));
        notify_observer(
            &observer,
            request.op,
            request.block_count,
            Err(terminal_error),
        );
    }
}

fn drain_latched_irqs(
    queue: &mut dyn HardwareQueue,
    state: &HctxState,
    pending: &mut BTreeMap<RequestId, PendingRequest>,
    observer: &Weak<dyn HctxObserver>,
    controller: &dyn ControllerEventPort,
    fatal_error: &mut Option<BlkError>,
) -> bool {
    let latches = state.irq_latches.lock().clone();
    let mut progressed = false;
    for latch in latches {
        let event = latch.take();
        if event.queue_ready {
            let mut unexpected_completion = false;
            let drain_result = {
                let mut sink = HctxCompletionSink {
                    pending,
                    observer,
                    override_error: None,
                    unexpected_completion: &mut unexpected_completion,
                };
                queue.drain_completions(&mut sink)
            };
            if drain_result.is_err() || unexpected_completion {
                set_hctx_fatal(state, fatal_error, BlkError::Io);
            }
            progressed = true;
        }
        // Queue-owned state must observe the acknowledged hardware event
        // before the controller reacts to the same IRQ. Initialization uses
        // this ordering to publish discovered geometry before Ready.
        if !event.control.is_empty() {
            controller.post(ControllerEvent::Irq(event.control));
            progressed = true;
        }
        if event.needs_rearm {
            controller.post(ControllerEvent::Rearm {
                source_id: event.control.source_id(),
            });
            progressed = true;
        }
    }
    progressed
}

fn submit_available(queue: &mut dyn HardwareQueue, context: SubmissionLoop<'_>) -> bool {
    let mut progressed = false;
    let limits = queue.info().limits;
    while context.pending.len() < limits.max_inflight {
        let available = limits.max_inflight - context.pending.len();
        let batch_limit = available.min(limits.max_submit_batch);
        let submissions = collect_submission_batch(
            context.state,
            context.retry_submissions,
            context.next_channel,
            batch_limit,
        );
        if submissions.is_empty() {
            break;
        }

        let offered = submissions.len();
        let (mut requests, metadata) = split_submission_batch(submissions);
        let mut accepted_ids = AcceptedRequestIds {
            ids: Vec::with_capacity(offered),
        };
        let result = queue.submit_batch_owned(&mut requests, &mut accepted_ids);
        let remaining_count_valid = requests.len() <= offered;
        let removed = if remaining_count_valid {
            offered - requests.len()
        } else {
            0
        };
        let contract_valid = requests.len() <= offered
            && result.accepted() == removed
            && accepted_ids.ids.len() == removed;

        if removed != 0 && queue.commit_submissions().is_err() {
            set_hctx_fatal(context.state, context.fatal_error, BlkError::Io);
        }

        let deadline = wall_time().saturating_add(REQUEST_TIMEOUT);
        let ownership_valid = reconcile_submission_batch(
            requests,
            metadata,
            accepted_ids.ids,
            removed,
            SubmissionReconciliation {
                deadline,
                pending: context.pending,
                retry_submissions: context.retry_submissions,
                protocol_failed: context.protocol_failed,
            },
        );

        progressed |= removed != 0;
        if !contract_valid || !ownership_valid {
            set_hctx_fatal(context.state, context.fatal_error, BlkError::Io);
        }
        match result.disposition() {
            BatchSubmitDisposition::Continue => {
                if removed == 0 && !context.retry_submissions.is_empty() {
                    set_hctx_fatal(context.state, context.fatal_error, BlkError::Io);
                }
            }
            BatchSubmitDisposition::QueueFull => break,
            BatchSubmitDisposition::Fatal(error) => {
                set_hctx_fatal(context.state, context.fatal_error, error);
            }
        }
        if context.state.stopping.load(Ordering::Acquire) {
            break;
        }
    }
    progressed
}

fn collect_submission_batch(
    state: &HctxState,
    retry_submissions: &mut VecDeque<Submission>,
    next_channel: &mut usize,
    limit: usize,
) -> VecDeque<Submission> {
    let mut submissions = VecDeque::with_capacity(limit);
    while submissions.len() < limit {
        let submission = retry_submissions
            .pop_front()
            .or_else(|| try_recv_submission(state, next_channel));
        let Some(submission) = submission else {
            break;
        };
        submissions.push_back(submission);
    }
    submissions
}

fn split_submission_batch(
    submissions: VecDeque<Submission>,
) -> (OwnedRequestBatch, VecDeque<SubmissionMetadata>) {
    let mut requests = OwnedRequestBatch::with_capacity(submissions.len());
    let mut metadata = VecDeque::with_capacity(submissions.len());
    for submission in submissions {
        let Submission {
            mut request,
            completion,
        } = submission;
        request.flags = request.flags.without(RequestFlags::NOWAIT);
        let op = request.op;
        let block_count = request.block_count;
        requests.push_back(request);
        metadata.push_back(SubmissionMetadata {
            completion,
            op,
            block_count,
        });
    }
    (requests, metadata)
}

fn reconcile_submission_batch(
    requests: OwnedRequestBatch,
    mut metadata: VecDeque<SubmissionMetadata>,
    accepted_ids: Vec<RequestId>,
    removed: usize,
    context: SubmissionReconciliation<'_>,
) -> bool {
    let mut ownership_valid = accepted_ids.len() == removed && removed <= metadata.len();
    for index in 0..removed.min(metadata.len()) {
        let request = pending_from_metadata(
            metadata
                .pop_front()
                .expect("removed request metadata count was checked"),
            context.deadline,
        );
        let Some(id) = accepted_ids.get(index).copied() else {
            context.protocol_failed.push(request);
            ownership_valid = false;
            continue;
        };
        match context.pending.entry(id) {
            alloc::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(request);
            }
            alloc::collections::btree_map::Entry::Occupied(_) => {
                context.protocol_failed.push(request);
                ownership_valid = false;
            }
        }
    }

    let request_count = requests.len();
    let metadata_count = metadata.len();
    ownership_valid &= request_count == metadata_count;
    let paired_count = request_count.min(metadata_count);
    let mut request_iter = requests.into_iter();
    let mut restored = VecDeque::with_capacity(paired_count + context.retry_submissions.len());
    for _ in 0..paired_count {
        let request = request_iter
            .next()
            .expect("runtime-owned request pair count was checked");
        let metadata = metadata
            .pop_front()
            .expect("runtime-owned metadata pair count was checked");
        restored.push_back(Submission {
            request,
            completion: metadata.completion,
        });
    }
    for request in request_iter {
        drop(super::dma::complete_without_submit(request.data));
    }
    for request in metadata {
        context
            .protocol_failed
            .push(pending_from_metadata(request, context.deadline));
    }
    restored.append(context.retry_submissions);
    *context.retry_submissions = restored;
    ownership_valid
}

fn pending_from_metadata(metadata: SubmissionMetadata, deadline: Duration) -> PendingRequest {
    PendingRequest {
        completion: metadata.completion,
        op: metadata.op,
        block_count: metadata.block_count,
        deadline,
    }
}

fn set_hctx_fatal(state: &HctxState, fatal_error: &mut Option<BlkError>, error: BlkError) {
    if fatal_error.is_none() {
        *fatal_error = Some(error);
    }
    state.stopping.store(true, Ordering::Release);
}

fn try_recv_submission(state: &HctxState, next_channel: &mut usize) -> Option<Submission> {
    let channels = state.submission_channels.lock();
    if channels.is_empty() {
        return None;
    }
    for offset in 0..channels.len() {
        let index = (*next_channel + offset) % channels.len();
        if let Some(submission) = channels[index].try_recv() {
            *next_channel = (index + 1) % channels.len();
            return Some(submission);
        }
    }
    None
}

fn reject_unsubmitted(submission: Submission, observer: &Weak<dyn HctxObserver>) {
    let op = submission.request.op;
    let block_count = submission.request.block_count;
    let data = super::dma::complete_without_submit(submission.request.data);
    submission.completion.complete(CompletedRequest::new(
        RequestId::new(usize::MAX),
        Err(BlkError::Io),
        data,
    ));
    notify_observer(observer, op, block_count, Err(BlkError::Io));
}

fn next_pending_deadline(pending: &BTreeMap<RequestId, PendingRequest>) -> Option<Duration> {
    pending.values().map(|request| request.deadline).min()
}

fn pending_deadline_expired(pending: &BTreeMap<RequestId, PendingRequest>, now: Duration) -> bool {
    next_pending_deadline(pending).is_some_and(|deadline| deadline <= now)
}

struct HctxCompletionSink<'a> {
    pending: &'a mut BTreeMap<RequestId, PendingRequest>,
    observer: &'a Weak<dyn HctxObserver>,
    override_error: Option<BlkError>,
    unexpected_completion: &'a mut bool,
}

impl CompletionSink for HctxCompletionSink<'_> {
    fn complete(&mut self, mut completed: CompletedRequest) {
        let Some(request) = self.pending.remove(&completed.id) else {
            *self.unexpected_completion = true;
            drop(completed);
            return;
        };
        if let Some(error) = self.override_error {
            completed.result = Err(error);
        }
        let result = completed.result;
        request.completion.complete(completed);
        notify_observer(self.observer, request.op, request.block_count, result);
    }
}

fn notify_observer(
    observer: &Weak<dyn HctxObserver>,
    op: RequestOp,
    block_count: u32,
    result: Result<(), BlkError>,
) {
    if let Some(observer) = observer.upgrade() {
        observer.request_completed(op, block_count, result);
    }
}

pub(super) fn request_is_nowait(request: &OwnedRequest) -> bool {
    request.flags.contains(RequestFlags::NOWAIT)
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
    use core::{
        any::Any,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    use std::{sync::Mutex, thread, time::Instant};

    use rdif_block::{
        ControlEvent, ControllerEvent, DeviceInfo, DriverGeneric, HardIrqHandler, IrqAck,
        IrqQueueMask, QueueLimits,
    };

    use super::*;
    use crate::block::runtime::{completion::CompletionSubscription, irq::BlockIrqAction};

    #[derive(Default)]
    struct QueueCounters {
        submitted: AtomicUsize,
        committed: AtomicUsize,
        drained: AtomicUsize,
        shutdown: AtomicUsize,
    }

    #[derive(Default)]
    struct TestObserver {
        completed: AtomicUsize,
        failed: AtomicUsize,
    }

    impl HctxObserver for TestObserver {
        fn request_completed(
            &self,
            _op: RequestOp,
            _block_count: u32,
            _result: Result<(), BlkError>,
        ) {
            self.completed.fetch_add(1, Ordering::AcqRel);
        }

        fn hctx_failed(&self, _hctx_id: usize, _error: BlkError) {
            self.failed.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[derive(Default)]
    struct TestControllerPort {
        events: Mutex<Vec<ControllerEvent>>,
    }

    impl ControllerEventPort for TestControllerPort {
        fn post(&self, event: ControllerEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    struct NeverCompletesQueue {
        counters: Arc<QueueCounters>,
        next_id: usize,
        pending: Vec<RequestId>,
    }

    impl DriverGeneric for NeverCompletesQueue {
        fn name(&self) -> &str {
            "never-completes"
        }

        fn raw_any(&self) -> Option<&dyn Any> {
            Some(self)
        }

        fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
            Some(self)
        }
    }

    impl HardwareQueue for NeverCompletesQueue {
        fn id(&self) -> usize {
            0
        }

        fn info(&self) -> QueueInfo {
            test_queue_info(2)
        }

        fn submit_batch_owned(
            &mut self,
            requests: &mut OwnedRequestBatch,
            sink: &mut dyn SubmissionSink,
        ) -> rdif_block::BatchSubmitResult {
            let mut accepted = 0;
            while self.pending.len() < 2 && requests.pop_front().is_some() {
                self.next_id += 1;
                let id = RequestId::new(self.next_id);
                self.pending.push(id);
                sink.accepted(id);
                accepted += 1;
                self.counters.submitted.fetch_add(1, Ordering::AcqRel);
            }
            let disposition = if requests.is_empty() {
                BatchSubmitDisposition::Continue
            } else {
                BatchSubmitDisposition::QueueFull
            };
            rdif_block::BatchSubmitResult::new(accepted, disposition)
        }

        fn commit_submissions(&mut self) -> Result<(), BlkError> {
            self.counters.committed.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }

        fn drain_completions(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            self.counters.drained.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }

        fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            self.counters.shutdown.fetch_add(1, Ordering::AcqRel);
            for id in self.pending.drain(..) {
                sink.complete(CompletedRequest::new(id, Err(BlkError::Io), None));
            }
            Ok(())
        }
    }

    struct ReverseCompletionQueue {
        counters: Arc<QueueCounters>,
        next_id: usize,
        pending: Vec<RequestId>,
        accept_limit: usize,
        fatal_after_accept: bool,
        fail_commit: bool,
        inject_unexpected_completion: bool,
    }

    impl DriverGeneric for ReverseCompletionQueue {
        fn name(&self) -> &str {
            "reverse-completion"
        }
    }

    struct UnderreportedAcceptanceQueue {
        counters: Arc<QueueCounters>,
        pending: Vec<RequestId>,
    }

    impl DriverGeneric for UnderreportedAcceptanceQueue {
        fn name(&self) -> &str {
            "underreported-acceptance"
        }
    }

    impl HardwareQueue for UnderreportedAcceptanceQueue {
        fn id(&self) -> usize {
            0
        }

        fn info(&self) -> QueueInfo {
            test_queue_info(2)
        }

        fn submit_batch_owned(
            &mut self,
            requests: &mut OwnedRequestBatch,
            sink: &mut dyn SubmissionSink,
        ) -> rdif_block::BatchSubmitResult {
            for index in 0..2 {
                if requests.pop_front().is_none() {
                    break;
                }
                let id = RequestId::new(index + 1);
                self.pending.push(id);
                if index == 0 {
                    sink.accepted(id);
                }
                self.counters.submitted.fetch_add(1, Ordering::AcqRel);
            }
            rdif_block::BatchSubmitResult::new(2, BatchSubmitDisposition::Continue)
        }

        fn commit_submissions(&mut self) -> Result<(), BlkError> {
            self.counters.committed.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }

        fn drain_completions(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            Ok(())
        }

        fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            self.counters.shutdown.fetch_add(1, Ordering::AcqRel);
            for id in self.pending.drain(..) {
                sink.complete(CompletedRequest::new(id, Err(BlkError::Io), None));
            }
            Ok(())
        }
    }

    impl HardwareQueue for ReverseCompletionQueue {
        fn id(&self) -> usize {
            0
        }

        fn info(&self) -> QueueInfo {
            test_queue_info(2)
        }

        fn submit_batch_owned(
            &mut self,
            requests: &mut OwnedRequestBatch,
            sink: &mut dyn SubmissionSink,
        ) -> rdif_block::BatchSubmitResult {
            let mut accepted = 0;
            while self.pending.len() < 2
                && accepted < self.accept_limit
                && requests.pop_front().is_some()
            {
                self.next_id += 1;
                let id = RequestId::new(self.next_id);
                self.pending.push(id);
                sink.accepted(id);
                accepted += 1;
                self.counters.submitted.fetch_add(1, Ordering::AcqRel);
            }
            let disposition = if self.fatal_after_accept && accepted != 0 {
                BatchSubmitDisposition::Fatal(BlkError::Io)
            } else if requests.is_empty() {
                BatchSubmitDisposition::Continue
            } else {
                BatchSubmitDisposition::QueueFull
            };
            rdif_block::BatchSubmitResult::new(accepted, disposition)
        }

        fn commit_submissions(&mut self) -> Result<(), BlkError> {
            self.counters.committed.fetch_add(1, Ordering::AcqRel);
            if self.fail_commit {
                Err(BlkError::Io)
            } else {
                Ok(())
            }
        }

        fn drain_completions(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            self.counters.drained.fetch_add(1, Ordering::AcqRel);
            if self.inject_unexpected_completion {
                sink.complete(CompletedRequest::new(
                    RequestId::new(usize::MAX),
                    Ok(()),
                    None,
                ));
                return Ok(());
            }
            while let Some(id) = self.pending.pop() {
                sink.complete(CompletedRequest::new(id, Ok(()), None));
            }
            Ok(())
        }

        fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            self.counters.shutdown.fetch_add(1, Ordering::AcqRel);
            while let Some(id) = self.pending.pop() {
                sink.complete(CompletedRequest::new(id, Err(BlkError::Io), None));
            }
            Ok(())
        }
    }

    struct QueueZeroIrq;

    impl HardIrqHandler for QueueZeroIrq {
        fn ack(&mut self) -> IrqAck {
            IrqAck::cleared(IrqQueueMask::from_queue(0), ControlEvent::new(0, 0))
        }
    }

    fn test_queue_info(depth: usize) -> QueueInfo {
        let mut limits = QueueLimits::simple(512, u64::MAX);
        limits.max_inflight = depth;
        limits.max_submit_batch = depth;
        limits.supports_flush = true;
        QueueInfo {
            id: 0,
            device: DeviceInfo::new(32, 512),
            limits,
        }
    }

    fn flush_submission() -> (CompletionSubscription, Submission) {
        let (subscription, completion) = CompletionSubscription::pair().unwrap();
        (
            subscription,
            Submission {
                request: OwnedRequest {
                    op: RequestOp::Flush,
                    lba: 0,
                    block_count: 0,
                    data: None,
                    flags: RequestFlags::NONE,
                },
                completion,
            },
        )
    }

    fn wait_for_submissions(counters: &QueueCounters, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while counters.submitted.load(Ordering::Acquire) < expected {
            assert!(Instant::now() < deadline, "maintenance task did not submit");
            thread::yield_now();
        }
    }

    fn wait_for_commits(counters: &QueueCounters, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while counters.committed.load(Ordering::Acquire) < expected {
            assert!(Instant::now() < deadline, "maintenance task did not commit");
            thread::yield_now();
        }
    }

    #[test]
    fn missing_irq_times_out_without_completion_drain() {
        crate::os::task::install_test_runtime_ops();
        let counters = Arc::new(QueueCounters::default());
        let observer = Arc::new(TestObserver::default());
        let observer_dyn: Arc<dyn HctxObserver> = observer.clone();
        let controller = Arc::new(TestControllerPort::default());
        let queue = NeverCompletesQueue {
            counters: Arc::clone(&counters),
            next_id: 0,
            pending: Vec::new(),
        };
        let hctx = Hctx::start(
            Box::new(queue),
            0,
            Arc::downgrade(&observer_dyn),
            controller.clone(),
        )
        .unwrap();
        let channel = hctx.add_submission_channel().unwrap();
        let (subscription, submission) = flush_submission();
        assert!(channel.send(submission, false).is_ok());

        let completed = subscription.recv().unwrap();
        assert_eq!(completed.result, Err(BlkError::TimedOut));
        hctx.stop();

        assert_eq!(counters.drained.load(Ordering::Acquire), 0);
        assert_eq!(counters.shutdown.load(Ordering::Acquire), 1);
        assert_eq!(observer.failed.load(Ordering::Acquire), 1);
        assert!(
            controller
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|event| { *event == ControllerEvent::Watchdog { queue_id: 0 } })
        );
    }

    #[test]
    fn out_of_order_irq_completions_reach_the_right_subscriptions() {
        crate::os::task::install_test_runtime_ops();
        let counters = Arc::new(QueueCounters::default());
        let observer: Arc<dyn HctxObserver> = Arc::new(TestObserver::default());
        let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
        let queue = ReverseCompletionQueue {
            counters: Arc::clone(&counters),
            next_id: 0,
            pending: Vec::new(),
            accept_limit: 2,
            fatal_after_accept: false,
            fail_commit: false,
            inject_unexpected_completion: false,
        };
        let hctx = Hctx::start(Box::new(queue), 0, Arc::downgrade(&observer), controller).unwrap();
        let channel = hctx.add_submission_channel().unwrap();
        let (first, first_submission) = flush_submission();
        let (second, second_submission) = flush_submission();
        assert!(
            channel
                .send_many(VecDeque::from([first_submission, second_submission]), false,)
                .is_ok()
        );
        wait_for_submissions(&counters, 2);
        wait_for_commits(&counters, 1);
        assert_eq!(counters.committed.load(Ordering::Acquire), 1);

        let target = hctx.irq_target(0);
        let mut action = BlockIrqAction::new(Box::new(QueueZeroIrq), vec![target]);
        assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);

        assert_eq!(usize::from(first.recv().unwrap().id), 1);
        assert_eq!(usize::from(second.recv().unwrap().id), 2);
        assert_eq!(counters.drained.load(Ordering::Acquire), 1);
        hctx.stop();
    }

    #[test]
    fn dropped_subscription_does_not_cancel_hardware_ownership() {
        crate::os::task::install_test_runtime_ops();
        let counters = Arc::new(QueueCounters::default());
        let observer = Arc::new(TestObserver::default());
        let observer_dyn: Arc<dyn HctxObserver> = observer.clone();
        let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
        let queue = ReverseCompletionQueue {
            counters: Arc::clone(&counters),
            next_id: 0,
            pending: Vec::new(),
            accept_limit: 2,
            fatal_after_accept: false,
            fail_commit: false,
            inject_unexpected_completion: false,
        };
        let hctx = Hctx::start(
            Box::new(queue),
            0,
            Arc::downgrade(&observer_dyn),
            controller,
        )
        .unwrap();
        let channel = hctx.add_submission_channel().unwrap();
        let (subscription, submission) = flush_submission();
        assert!(channel.send(submission, false).is_ok());
        wait_for_submissions(&counters, 1);
        drop(subscription);

        let target = hctx.irq_target(0);
        let mut action = BlockIrqAction::new(Box::new(QueueZeroIrq), vec![target]);
        assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);
        let deadline = Instant::now() + Duration::from_secs(1);
        while observer.completed.load(Ordering::Acquire) != 1 {
            assert!(
                Instant::now() < deadline,
                "dropped subscription prevented deferred completion"
            );
            thread::yield_now();
        }

        assert_eq!(counters.drained.load(Ordering::Acquire), 1);
        hctx.stop();
    }

    #[test]
    fn partial_batch_is_committed_and_remaining_request_is_retried_after_irq() {
        crate::os::task::install_test_runtime_ops();
        let counters = Arc::new(QueueCounters::default());
        let observer: Arc<dyn HctxObserver> = Arc::new(TestObserver::default());
        let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
        let queue = ReverseCompletionQueue {
            counters: Arc::clone(&counters),
            next_id: 0,
            pending: Vec::new(),
            accept_limit: 1,
            fatal_after_accept: false,
            fail_commit: false,
            inject_unexpected_completion: false,
        };
        let hctx = Hctx::start(Box::new(queue), 0, Arc::downgrade(&observer), controller).unwrap();
        let channel = hctx.add_submission_channel().unwrap();
        let (first, first_submission) = flush_submission();
        let (second, second_submission) = flush_submission();
        assert!(
            channel
                .send_many(VecDeque::from([first_submission, second_submission]), false,)
                .is_ok()
        );
        wait_for_submissions(&counters, 1);
        wait_for_commits(&counters, 1);
        assert_eq!(counters.submitted.load(Ordering::Acquire), 1);

        let target = hctx.irq_target(0);
        let mut action = BlockIrqAction::new(Box::new(QueueZeroIrq), vec![target]);
        assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);
        wait_for_submissions(&counters, 2);
        wait_for_commits(&counters, 2);
        assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);

        assert!(first.recv().unwrap().result.is_ok());
        assert!(second.recv().unwrap().result.is_ok());
        assert_eq!(counters.submitted.load(Ordering::Acquire), 2);
        assert_eq!(counters.committed.load(Ordering::Acquire), 2);
        hctx.stop();
    }

    #[test]
    fn malformed_acceptance_report_still_terminates_every_runtime_request() {
        crate::os::task::install_test_runtime_ops();
        let counters = Arc::new(QueueCounters::default());
        let observer = Arc::new(TestObserver::default());
        let observer_dyn: Arc<dyn HctxObserver> = observer.clone();
        let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
        let queue = UnderreportedAcceptanceQueue {
            counters: Arc::clone(&counters),
            pending: Vec::new(),
        };
        let hctx = Hctx::start(
            Box::new(queue),
            0,
            Arc::downgrade(&observer_dyn),
            controller,
        )
        .unwrap();
        let channel = hctx.add_submission_channel().unwrap();
        let (_first, first_submission) = flush_submission();
        let (_second, second_submission) = flush_submission();
        assert!(
            channel
                .send_many(VecDeque::from([first_submission, second_submission]), false,)
                .is_ok()
        );

        let deadline = Instant::now() + Duration::from_secs(1);
        while observer.failed.load(Ordering::Acquire) != 1 {
            assert!(
                Instant::now() < deadline,
                "malformed queue contract did not fail the hctx"
            );
            thread::yield_now();
        }
        hctx.stop();

        assert_eq!(counters.committed.load(Ordering::Acquire), 1);
        assert_eq!(counters.shutdown.load(Ordering::Acquire), 1);
        assert_eq!(observer.completed.load(Ordering::Acquire), 2);
    }

    #[test]
    fn accepted_prefix_is_committed_before_fatal_batch_teardown() {
        crate::os::task::install_test_runtime_ops();
        let counters = Arc::new(QueueCounters::default());
        let observer = Arc::new(TestObserver::default());
        let observer_dyn: Arc<dyn HctxObserver> = observer.clone();
        let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
        let queue = ReverseCompletionQueue {
            counters: Arc::clone(&counters),
            next_id: 0,
            pending: Vec::new(),
            accept_limit: 1,
            fatal_after_accept: true,
            fail_commit: false,
            inject_unexpected_completion: false,
        };
        let hctx = Hctx::start(
            Box::new(queue),
            0,
            Arc::downgrade(&observer_dyn),
            controller,
        )
        .unwrap();
        let channel = hctx.add_submission_channel().unwrap();
        let (_accepted, accepted_submission) = flush_submission();
        let (_remaining, remaining_submission) = flush_submission();
        assert!(
            channel
                .send_many(
                    VecDeque::from([accepted_submission, remaining_submission]),
                    false,
                )
                .is_ok()
        );

        let deadline = Instant::now() + Duration::from_secs(1);
        while observer.failed.load(Ordering::Acquire) != 1 {
            assert!(
                Instant::now() < deadline,
                "fatal submission result did not stop the hctx"
            );
            thread::yield_now();
        }
        hctx.stop();

        assert_eq!(counters.submitted.load(Ordering::Acquire), 1);
        assert_eq!(counters.committed.load(Ordering::Acquire), 1);
        assert_eq!(counters.shutdown.load(Ordering::Acquire), 1);
        assert_eq!(observer.completed.load(Ordering::Acquire), 2);
    }

    #[test]
    fn commit_failure_terminates_every_accepted_request() {
        crate::os::task::install_test_runtime_ops();
        let counters = Arc::new(QueueCounters::default());
        let observer = Arc::new(TestObserver::default());
        let observer_dyn: Arc<dyn HctxObserver> = observer.clone();
        let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
        let queue = ReverseCompletionQueue {
            counters: Arc::clone(&counters),
            next_id: 0,
            pending: Vec::new(),
            accept_limit: 2,
            fatal_after_accept: false,
            fail_commit: true,
            inject_unexpected_completion: false,
        };
        let hctx = Hctx::start(
            Box::new(queue),
            0,
            Arc::downgrade(&observer_dyn),
            controller,
        )
        .unwrap();
        let channel = hctx.add_submission_channel().unwrap();
        let (first, first_submission) = flush_submission();
        let (second, second_submission) = flush_submission();
        assert!(
            channel
                .send_many(VecDeque::from([first_submission, second_submission]), false)
                .is_ok()
        );

        assert_eq!(first.recv().unwrap().result, Err(BlkError::Io));
        assert_eq!(second.recv().unwrap().result, Err(BlkError::Io));
        hctx.stop();

        assert_eq!(counters.submitted.load(Ordering::Acquire), 2);
        assert_eq!(counters.committed.load(Ordering::Acquire), 1);
        assert_eq!(counters.shutdown.load(Ordering::Acquire), 1);
        assert_eq!(observer.completed.load(Ordering::Acquire), 2);
        assert_eq!(observer.failed.load(Ordering::Acquire), 1);
    }

    #[test]
    fn unexpected_completion_fails_hctx_and_preserves_pending_ownership() {
        crate::os::task::install_test_runtime_ops();
        let counters = Arc::new(QueueCounters::default());
        let observer = Arc::new(TestObserver::default());
        let observer_dyn: Arc<dyn HctxObserver> = observer.clone();
        let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
        let queue = ReverseCompletionQueue {
            counters: Arc::clone(&counters),
            next_id: 0,
            pending: Vec::new(),
            accept_limit: 1,
            fatal_after_accept: false,
            fail_commit: false,
            inject_unexpected_completion: true,
        };
        let hctx = Hctx::start(
            Box::new(queue),
            0,
            Arc::downgrade(&observer_dyn),
            controller,
        )
        .unwrap();
        let channel = hctx.add_submission_channel().unwrap();
        let (subscription, submission) = flush_submission();
        assert!(channel.send(submission, false).is_ok());
        wait_for_submissions(&counters, 1);

        let target = hctx.irq_target(0);
        let mut action = BlockIrqAction::new(Box::new(QueueZeroIrq), vec![target]);
        assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);

        assert_eq!(subscription.recv().unwrap().result, Err(BlkError::Io));
        hctx.stop();

        assert_eq!(counters.drained.load(Ordering::Acquire), 1);
        assert_eq!(counters.shutdown.load(Ordering::Acquire), 1);
        assert_eq!(observer.completed.load(Ordering::Acquire), 1);
        assert_eq!(observer.failed.load(Ordering::Acquire), 1);
    }
}
