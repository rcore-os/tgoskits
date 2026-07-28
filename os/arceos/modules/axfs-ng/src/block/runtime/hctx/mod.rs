mod submission;

use alloc::{
    boxed::Box,
    collections::{BTreeMap, VecDeque},
    format,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    fmt,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use rdif_block::{
    BatchSubmitDisposition, BlkError, CompletedRequest, CompletionSink, ControllerEvent,
    HardwareQueue, OwnedRequest, OwnedRequestBatch, QueueInfo, RequestFlags, RequestId, RequestOp,
    SubmissionSink,
};
use submission::{SubmissionLoop, SubmissionScratch, reject_unsubmitted, submit_available};

use super::{
    channel::BoundedChannel,
    completion::CompletionSender,
    irq::{IrqEventLatch, IrqTarget, LatchedIrqEvent},
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

    fn call(&self, event: ControllerEvent) -> Result<rdif_block::ControllerState, BlkError>;
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

pub(super) struct HctxStartError {
    error: BlkError,
    queue: Box<dyn HardwareQueue>,
}

impl HctxStartError {
    pub(super) fn into_parts(self) -> (BlkError, Box<dyn HardwareQueue>) {
        (self.error, self.queue)
    }
}

impl fmt::Debug for HctxStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HctxStartError")
            .field("error", &self.error)
            .field("queue_id", &self.queue.id())
            .finish()
    }
}

struct HctxState {
    submission_channels: IrqMutex<Vec<Arc<BoundedChannel<Submission>>>>,
    notification: Arc<dyn BlockNotification>,
    lifecycle_notification: Arc<dyn BlockNotification>,
    irq_latches: IrqMutex<Vec<Arc<IrqEventLatch>>>,
    quiescing: AtomicBool,
    quiesced: AtomicBool,
    stopping: AtomicBool,
    terminated: AtomicBool,
}

struct PendingRequest {
    completion: CompletionSender,
    op: RequestOp,
    block_count: u32,
    deadline: Duration,
}

impl Hctx {
    pub(super) fn start(
        queue: Box<dyn HardwareQueue>,
        cpu: usize,
        observer: Weak<dyn HctxObserver>,
        controller: Arc<dyn ControllerEventPort>,
    ) -> Result<Arc<Self>, HctxStartError> {
        let info = queue.info();
        if info.limits.max_inflight == 0
            || info.limits.max_submit_batch == 0
            || info.limits.max_submit_batch > info.limits.max_inflight
            || info.id >= u64::BITS as usize
        {
            return Err(HctxStartError {
                error: BlkError::InvalidRequest,
                queue,
            });
        }

        let ops = match runtime_ops() {
            Ok(ops) => ops,
            Err(_) => {
                return Err(HctxStartError {
                    error: BlkError::Other("block runtime adapter is not installed"),
                    queue,
                });
            }
        };
        let notification = ops.notification();
        let state = Arc::new(HctxState {
            submission_channels: IrqMutex::new(Vec::new()),
            notification,
            lifecycle_notification: ops.notification(),
            irq_latches: IrqMutex::new(Vec::new()),
            quiescing: AtomicBool::new(false),
            quiesced: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
        });
        let hctx = Arc::new(Self {
            id: info.id,
            cpu,
            info,
            state: Arc::clone(&state),
            thread: IrqMutex::new(None),
        });
        let name = format!("blk-hctx/{}", info.id);
        let queue_slot = Arc::new(IrqMutex::new(Some(queue)));
        let worker_queue_slot = Arc::clone(&queue_slot);
        let thread = match ops.spawn_pinned(
            name,
            cpu,
            Box::new(move || {
                let queue = worker_queue_slot
                    .lock()
                    .take()
                    .expect("new hctx worker owns its startup queue");
                run_hctx(queue, state, observer, controller);
            }),
        ) {
            Ok(thread) => thread,
            Err(_) => {
                let queue = queue_slot
                    .lock()
                    .take()
                    .expect("failed hctx spawn retains its startup queue");
                return Err(HctxStartError {
                    error: BlkError::NoMemory,
                    queue,
                });
            }
        };
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
            self.state.lifecycle_notification.notify();
        }
        // Drop the IRQ-disabling slot guard before `join`, which may sleep.
        let thread = self.thread.lock().take();
        if let Some(thread) = thread {
            thread.join();
        }
    }

    /// Stops queue mutation while retaining the hardware queue and its DMA
    /// memory inside the pinned maintenance thread.
    pub(super) fn quiesce(&self) {
        if self.state.stopping.load(Ordering::Acquire) {
            return;
        }
        if !self.state.quiescing.swap(true, Ordering::AcqRel) {
            for channel in self.state.submission_channels.lock().iter() {
                channel.close();
            }
            self.state.notification.notify();
        }
        while !self.state.quiesced.load(Ordering::Acquire)
            && !self.state.terminated.load(Ordering::Acquire)
        {
            self.state.lifecycle_notification.wait();
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
    let mut prefer_retry = true;
    let mut irq_events = Vec::new();
    let mut submission_scratch =
        SubmissionScratch::with_capacity(queue.info().limits.max_submit_batch);

    while !state.stopping.load(Ordering::Acquire) {
        if state.quiescing.load(Ordering::Acquire) {
            if !state.quiesced.swap(true, Ordering::AcqRel) {
                state.lifecycle_notification.notify();
            }
            state.notification.wait();
            continue;
        }
        let irq_progress = drain_latched_irqs(
            &mut *queue,
            &state,
            &mut pending,
            &observer,
            &*controller,
            &mut fatal_error,
            &mut irq_events,
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
                prefer_retry: &mut prefer_retry,
                scratch: &mut submission_scratch,
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

    let controller_stopped = fatal_error.is_none()
        || matches!(
            controller.call(ControllerEvent::Watchdog {
                queue_id: queue.id(),
            }),
            Ok(rdif_block::ControllerState::Shutdown)
        );
    let mut unexpected_completion = false;
    let shutdown_result = if controller_stopped {
        let mut sink = HctxCompletionSink {
            pending: &mut pending,
            observer: &observer,
            override_error: fatal_error,
            unexpected_completion: &mut unexpected_completion,
        };
        queue.shutdown(&mut sink)
    } else {
        Err(BlkError::Io)
    };
    if (shutdown_result.is_err() || unexpected_completion) && fatal_error.is_none() {
        fatal_error = Some(BlkError::Io);
    }
    if let Some(error) = fatal_error
        && let Some(observer) = observer.upgrade()
    {
        observer.hctx_failed(queue.id(), error);
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
        super::metrics::record_terminal_completion(true);
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
        super::metrics::record_terminal_completion(true);
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
    state.terminated.store(true, Ordering::Release);
    state.lifecycle_notification.notify();
    if !controller_stopped {
        warn!(
            "leaking block queue {} because controller shutdown was not confirmed",
            queue.id()
        );
        core::mem::forget(queue);
    }
}

fn drain_latched_irqs(
    queue: &mut dyn HardwareQueue,
    state: &HctxState,
    pending: &mut BTreeMap<RequestId, PendingRequest>,
    observer: &Weak<dyn HctxObserver>,
    controller: &dyn ControllerEventPort,
    fatal_error: &mut Option<BlkError>,
    events: &mut Vec<LatchedIrqEvent>,
) -> bool {
    debug_assert!(events.is_empty());
    {
        let latches = state.irq_latches.lock();
        for latch in latches.iter() {
            let event = latch.take();
            if event.queue_ready || event.needs_rearm || !event.control.is_empty() {
                events.push(event);
            }
        }
    }
    let mut progressed = false;
    for event in events.drain(..) {
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

fn set_hctx_fatal(state: &HctxState, fatal_error: &mut Option<BlkError>, error: BlkError) {
    if fatal_error.is_none() {
        *fatal_error = Some(error);
    }
    state.stopping.store(true, Ordering::Release);
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
        super::metrics::record_terminal_completion(result.is_err());
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
mod tests;
