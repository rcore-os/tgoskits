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
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
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
const QUEUE_REGISTER_TRANSITION_TIMEOUT: Duration = Duration::from_secs(5);

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
    state: Arc<HctxState>,
    thread: IrqMutex<Option<Box<dyn BlockThread>>>,
}

const HCTX_PREPARED: u8 = 0;
const HCTX_ACTIVE: u8 = 1;
const HCTX_ABORTED: u8 = 2;

/// A queue worker whose hardware side effects are paused until installation
/// commits its targets and channels.
pub(super) struct PreparedHctx {
    hctx: Option<Arc<Hctx>>,
}

/// An installed queue whose worker has been made active.
pub(super) struct ActivatedHctx {
    hctx: Arc<Hctx>,
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
    queue_info: IrqMutex<QueueInfoEpoch>,
    submission_channels: IrqMutex<Vec<Arc<BoundedChannel<Submission>>>>,
    submission_channels_sealed: AtomicBool,
    notification: Arc<dyn BlockNotification>,
    lifecycle_notification: Arc<dyn BlockNotification>,
    irq_latches: IrqMutex<Vec<Arc<IrqEventLatch>>>,
    quiescing: AtomicBool,
    quiesced: AtomicBool,
    stopping: AtomicBool,
    terminated: AtomicBool,
    teardown_error: IrqMutex<Option<BlkError>>,
    activation: AtomicU8,
    activation_notification: Arc<dyn BlockNotification>,
    prepared_queue: IrqMutex<Option<Box<dyn HardwareQueue>>>,
}

#[cfg(test)]
impl HctxState {
    pub(super) fn test_new(
        queue_info: QueueInfo,
        submission_channels: Vec<Arc<BoundedChannel<Submission>>>,
    ) -> Self {
        let ops = runtime_ops().expect("test runtime is installed");
        Self {
            queue_info: IrqMutex::new(QueueInfoEpoch::new(queue_info)),
            submission_channels: IrqMutex::new(submission_channels),
            submission_channels_sealed: AtomicBool::new(false),
            notification: ops.notification(),
            lifecycle_notification: ops.notification(),
            irq_latches: IrqMutex::new(Vec::new()),
            quiescing: AtomicBool::new(false),
            quiesced: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
            teardown_error: IrqMutex::new(None),
            activation: AtomicU8::new(HCTX_ACTIVE),
            activation_notification: ops.notification(),
            prepared_queue: IrqMutex::new(None),
        }
    }
}

struct PendingRequest {
    completion: CompletionSender,
    op: RequestOp,
    block_count: u32,
    deadline: Duration,
}

impl Hctx {
    pub(super) fn prepare(
        queue: Box<dyn HardwareQueue>,
        cpu: usize,
        observer: Weak<dyn HctxObserver>,
        controller: Arc<dyn ControllerEventPort>,
    ) -> Result<PreparedHctx, HctxStartError> {
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
        let activation_notification = ops.notification();
        let state = Arc::new(HctxState {
            queue_info: IrqMutex::new(QueueInfoEpoch::new(info)),
            submission_channels: IrqMutex::new(Vec::new()),
            submission_channels_sealed: AtomicBool::new(false),
            notification,
            lifecycle_notification: ops.notification(),
            irq_latches: IrqMutex::new(Vec::new()),
            quiescing: AtomicBool::new(false),
            quiesced: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
            teardown_error: IrqMutex::new(None),
            activation: AtomicU8::new(HCTX_PREPARED),
            activation_notification,
            prepared_queue: IrqMutex::new(None),
        });
        let hctx = Arc::new(Self {
            id: info.id,
            cpu,
            state: Arc::clone(&state),
            thread: IrqMutex::new(None),
        });
        let name = format!("blk-hctx/{}", info.id);
        let queue_slot = Arc::new(IrqMutex::new(Some(queue)));
        let worker_queue_slot = Arc::clone(&queue_slot);
        let worker_state = Arc::clone(&state);
        let thread = match ops.spawn_pinned(
            name,
            cpu,
            Box::new(move || {
                let queue = {
                    let mut slot = worker_queue_slot.lock();
                    let queue = slot.take().expect("new hctx worker owns its startup queue");
                    drop(slot);
                    queue
                };
                while worker_state.activation.load(Ordering::Acquire) == HCTX_PREPARED {
                    worker_state.activation_notification.wait();
                }
                if worker_state.activation.load(Ordering::Acquire) == HCTX_ABORTED {
                    *worker_state.prepared_queue.lock() = Some(queue);
                    worker_state.terminated.store(true, Ordering::Release);
                    worker_state.lifecycle_notification.notify();
                    return;
                }
                run_hctx(queue, worker_state, observer, controller);
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
            "prepared block hctx {} on CPU {} with hardware depth {}",
            info.id, cpu, info.limits.max_inflight
        );
        Ok(PreparedHctx { hctx: Some(hctx) })
    }

    #[cfg(test)]
    pub(super) fn start(
        queue: Box<dyn HardwareQueue>,
        cpu: usize,
        observer: Weak<dyn HctxObserver>,
        controller: Arc<dyn ControllerEventPort>,
    ) -> Result<Arc<Self>, HctxStartError> {
        Ok(Self::prepare(queue, cpu, observer, controller)?
            .activate()
            .notify_and_into_arc())
    }

    pub(super) const fn id(&self) -> usize {
        self.id
    }

    pub(super) const fn cpu(&self) -> usize {
        self.cpu
    }

    pub(super) fn info(&self) -> QueueInfo {
        self.state.queue_info.lock().published()
    }

    pub(super) fn freeze_queue_info(&self) {
        self.state.queue_info.lock().freeze();
    }

    #[cfg(test)]
    pub(super) fn add_submission_channel(
        &self,
    ) -> Result<Arc<BoundedChannel<Submission>>, BlkError> {
        let channel = self.new_submission_channel()?;
        self.install_submission_channel(Arc::clone(&channel))?;
        Ok(channel)
    }

    pub(super) fn new_submission_channel(
        &self,
    ) -> Result<Arc<BoundedChannel<Submission>>, BlkError> {
        if self
            .state
            .submission_channels_sealed
            .load(Ordering::Acquire)
        {
            return Err(BlkError::Io);
        }
        let channel = Arc::new(
            BoundedChannel::with_item_notification(
                self.info().limits.max_inflight,
                Arc::clone(&self.state.notification),
            )
            .map_err(|_| BlkError::NoMemory)?,
        );
        Ok(channel)
    }

    pub(super) fn reserve_submission_channels(&self, additional: usize) -> Result<(), BlkError> {
        let mut channels = self.state.submission_channels.lock();
        if self
            .state
            .submission_channels_sealed
            .load(Ordering::Acquire)
        {
            return Err(BlkError::Io);
        }
        channels
            .try_reserve(additional)
            .map_err(|_| BlkError::NoMemory)
    }

    #[cfg(test)]
    pub(super) fn install_submission_channel(
        &self,
        channel: Arc<BoundedChannel<Submission>>,
    ) -> Result<(), BlkError> {
        let mut channels = self.state.submission_channels.lock();
        if self
            .state
            .submission_channels_sealed
            .load(Ordering::Acquire)
        {
            return Err(BlkError::Io);
        }
        channels.push(channel);
        drop(channels);
        self.state.notification.notify();
        Ok(())
    }

    pub(super) fn install_submission_channel_committed(
        &self,
        channel: Arc<BoundedChannel<Submission>>,
    ) {
        debug_assert!(
            !self
                .state
                .submission_channels_sealed
                .load(Ordering::Acquire)
        );
        let mut channels = self.state.submission_channels.lock();
        debug_assert!(channels.len() < channels.capacity());
        channels.push(channel);
    }

    pub(super) fn notify_submission_channels_changed(&self) {
        self.state.notification.notify();
    }

    #[cfg(test)]
    pub(super) fn submission_channel_count(&self) -> usize {
        self.state.submission_channels.lock().len()
    }

    pub(super) fn seal_submission_channels(&self) {
        let _channels = self.state.submission_channels.lock();
        self.state
            .submission_channels_sealed
            .store(true, Ordering::Release);
    }

    fn close_submission_channels(&self) {
        while let Some(channel) = {
            let channels = self.state.submission_channels.lock();
            channels
                .iter()
                .find(|channel| !channel.is_closed())
                .cloned()
        } {
            channel.close();
        }
    }

    #[cfg(test)]
    pub(super) fn irq_target(&self, source_id: usize) -> IrqTarget {
        let latch = Arc::new(IrqEventLatch::new(source_id));
        self.state.irq_latches.lock().push(Arc::clone(&latch));
        IrqTarget::new(self.id, latch, Arc::clone(&self.state.notification))
    }

    pub(super) fn prepare_irq_target(&self, source_id: usize) -> (IrqTarget, HctxIrqToken) {
        let latch = Arc::new(IrqEventLatch::new(source_id));
        (
            IrqTarget::new(
                self.id,
                Arc::clone(&latch),
                Arc::clone(&self.state.notification),
            ),
            HctxIrqToken {
                state: Arc::clone(&self.state),
                latch,
                committed: false,
            },
        )
    }

    pub(super) fn reserve_irq_targets(&self, additional: usize) -> Result<(), BlkError> {
        self.state
            .irq_latches
            .lock()
            .try_reserve(additional)
            .map_err(|_| BlkError::NoMemory)
    }

    pub(super) fn stop(&self) -> Result<(), BlkError> {
        if !self.state.stopping.swap(true, Ordering::AcqRel) {
            self.state
                .submission_channels_sealed
                .store(true, Ordering::Release);
            self.close_submission_channels();
            self.state.notification.notify();
            self.state.lifecycle_notification.notify();
        }
        // Drop the IRQ-disabling slot guard before `join`, which may sleep.
        let thread = self.thread.lock().take();
        if let Some(thread) = thread {
            thread.join();
        } else if !self.state.terminated.load(Ordering::Acquire) {
            return Err(BlkError::Io);
        }
        self.state.teardown_error.lock().map_or(Ok(()), Err)
    }

    /// Stops queue mutation while retaining the hardware queue and its DMA
    /// memory inside the pinned maintenance thread.
    pub(super) fn quiesce(&self) {
        if !self.state.quiescing.swap(true, Ordering::AcqRel) {
            self.state
                .submission_channels_sealed
                .store(true, Ordering::Release);
            self.close_submission_channels();
            self.state.notification.notify();
        }
        while !self.state.quiesced.load(Ordering::Acquire)
            && !self.state.terminated.load(Ordering::Acquire)
        {
            self.state.lifecycle_notification.wait();
        }
    }
}

pub(super) struct HctxIrqToken {
    state: Arc<HctxState>,
    latch: Arc<IrqEventLatch>,
    committed: bool,
}

impl HctxIrqToken {
    pub(super) fn commit(&mut self) {
        if !self.committed {
            let mut latches = self.state.irq_latches.lock();
            debug_assert!(latches.len() < latches.capacity());
            latches.push(Arc::clone(&self.latch));
            self.committed = true;
        }
    }
}

impl Drop for HctxIrqToken {
    fn drop(&mut self) {
        if self.committed {
            let mut latches = self.state.irq_latches.lock();
            if let Some(index) = latches
                .iter()
                .position(|latch| Arc::ptr_eq(latch, &self.latch))
            {
                latches.swap_remove(index);
            }
        }
    }
}

impl PreparedHctx {
    pub(super) fn id(&self) -> usize {
        self.hctx.as_ref().expect("prepared hctx was consumed").id()
    }

    pub(super) fn hctx(&self) -> &Arc<Hctx> {
        self.hctx.as_ref().expect("prepared hctx was consumed")
    }

    pub(super) fn activate(mut self) -> ActivatedHctx {
        let hctx = self.hctx.take().expect("prepared hctx activated once");
        hctx.state.activation.store(HCTX_ACTIVE, Ordering::Release);
        ActivatedHctx { hctx }
    }

    pub(super) fn abort(mut self) -> Option<Box<dyn HardwareQueue>> {
        let hctx = self.hctx.take().expect("prepared hctx aborted once");
        hctx.state.activation.store(HCTX_ABORTED, Ordering::Release);
        hctx.state.activation_notification.notify();
        let thread = hctx.thread.lock().take();
        if let Some(thread) = thread {
            thread.join();
        }
        hctx.state.prepared_queue.lock().take()
    }
}

impl Drop for PreparedHctx {
    fn drop(&mut self) {
        if self.hctx.is_some() {
            let hctx = self.hctx.take().expect("prepared hctx drop owns worker");
            hctx.state.activation.store(HCTX_ABORTED, Ordering::Release);
            hctx.state.activation_notification.notify();
            let thread = hctx.thread.lock().take();
            if let Some(thread) = thread {
                thread.join();
            }
            if let Some(queue) = hctx.state.prepared_queue.lock().take() {
                // The controller has not confirmed a terminal state, so the
                // queue may still own DMA memory visible to hardware.
                core::mem::forget(queue);
            }
        }
    }
}

impl ActivatedHctx {
    pub(super) fn notify_worker(&self) {
        self.hctx.state.activation_notification.notify();
    }

    #[cfg(test)]
    pub(super) fn notify_and_into_arc(self) -> Arc<Hctx> {
        self.notify_worker();
        self.into_arc()
    }

    #[cfg(test)]
    pub(super) fn into_arc(self) -> Arc<Hctx> {
        self.hctx
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
    let mut register_retry_at = None;
    let mut register_deadline = None;
    let mut submission_blocked = false;

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
        if fatal_error.is_some() {
            break;
        }
        if irq_progress {
            // An acknowledged device event supersedes a timer selected from
            // the prior queue state. Reconcile with the state produced by the
            // IRQ-driven drain below.
            register_retry_at = None;
            register_deadline = None;
        }
        reconcile_register_retry(
            &*queue,
            &mut register_retry_at,
            &mut register_deadline,
            wall_time(),
        );
        let register_progress = advance_register_retry_if_due(
            &mut *queue,
            wall_time(),
            &mut RegisterRetryContext {
                controller: &*controller,
                pending: &mut pending,
                observer: &observer,
                retry_at: &mut register_retry_at,
                deadline: &mut register_deadline,
                state: &state,
                fatal_error: &mut fatal_error,
            },
        );
        if fatal_error.is_some() {
            break;
        }
        if irq_progress || register_progress {
            submission_blocked = false;
        }
        let submit_progress = if submission_blocked {
            submission::SubmissionProgress::default()
        } else {
            submit_available(
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
            )
        };
        submission_blocked |= submit_progress.queue_full;
        prune_closed_submission_channels(&state);

        if fatal_error.is_none() && pending_deadline_expired(&pending, wall_time()) {
            fatal_error = Some(BlkError::TimedOut);
            state.stopping.store(true, Ordering::Release);
        }
        if state.stopping.load(Ordering::Acquire) {
            break;
        }
        if irq_progress || register_progress || submit_progress.made_progress {
            continue;
        }
        let now = wall_time();
        let wake_at = [
            next_pending_deadline(&pending),
            register_retry_at,
            register_deadline,
        ]
        .into_iter()
        .flatten()
        .min();
        match wake_at {
            Some(deadline) if deadline <= now => continue,
            Some(deadline) => {
                state.notification.wait_timeout(deadline - now);
            }
            None => state.notification.wait(),
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
    let queue_shutdown_error = shutdown_result.err();
    let teardown_error = queue_shutdown_error.or(unexpected_completion.then_some(BlkError::Io));
    if fatal_error.is_none() {
        fatal_error = teardown_error;
    }
    if let Some(error) = fatal_error
        && let Some(observer) = observer.upgrade()
    {
        observer.hctx_failed(queue.id(), error);
    }
    while let Some(submission) = retry_submissions.pop_front() {
        reject_unsubmitted(submission, &observer);
    }
    let channels = core::mem::take(&mut *state.submission_channels.lock());
    for channel in channels {
        channel.close();
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
    if let Some(error) = queue_shutdown_error {
        warn!(
            "quarantining block queue {} after shutdown failed: {error:?}",
            queue.id(),
        );
        core::mem::forget(queue);
    } else {
        drop(queue);
    }
    *state.teardown_error.lock() = teardown_error;
    state.terminated.store(true, Ordering::Release);
    state.lifecycle_notification.notify();
}

fn prune_closed_submission_channels(state: &HctxState) {
    loop {
        let retired = {
            let mut channels = state.submission_channels.lock();
            let Some(index) = channels
                .iter()
                .position(|channel| channel.is_closed_and_empty())
            else {
                return;
            };
            channels.swap_remove(index)
        };
        drop(retired);
    }
}

fn reconcile_register_retry(
    queue: &dyn HardwareQueue,
    retry_at: &mut Option<Duration>,
    deadline: &mut Option<Duration>,
    now: Duration,
) {
    match queue.register_retry_after() {
        Some(delay) if retry_at.is_none() => {
            let delay = if delay.is_zero() {
                Duration::from_micros(1)
            } else {
                delay
            };
            *retry_at = Some(now.saturating_add(delay));
            *deadline = Some(now.saturating_add(QUEUE_REGISTER_TRANSITION_TIMEOUT));
        }
        None => {
            *retry_at = None;
            *deadline = None;
        }
        Some(_) => {}
    }
}

struct RegisterRetryContext<'a> {
    controller: &'a dyn ControllerEventPort,
    pending: &'a mut BTreeMap<RequestId, PendingRequest>,
    observer: &'a Weak<dyn HctxObserver>,
    retry_at: &'a mut Option<Duration>,
    deadline: &'a mut Option<Duration>,
    state: &'a HctxState,
    fatal_error: &'a mut Option<BlkError>,
}

fn advance_register_retry_if_due(
    queue: &mut dyn HardwareQueue,
    now: Duration,
    context: &mut RegisterRetryContext<'_>,
) -> bool {
    if context.fatal_error.is_some() {
        return false;
    }
    if context.deadline.is_some_and(|deadline| deadline <= now) {
        set_hctx_fatal(context.state, context.fatal_error, BlkError::TimedOut);
        return true;
    }
    if !context.retry_at.is_some_and(|retry_at| retry_at <= now) {
        return false;
    }
    *context.retry_at = None;
    let mut unexpected_completion = false;
    let retry_result = {
        let mut sink = HctxCompletionSink {
            pending: context.pending,
            observer: context.observer,
            override_error: None,
            unexpected_completion: &mut unexpected_completion,
        };
        queue.advance_register_retry(&mut sink)
    };
    if let Err(error) = retry_result {
        set_hctx_fatal(context.state, context.fatal_error, error);
        return true;
    }
    if unexpected_completion {
        set_hctx_fatal(context.state, context.fatal_error, BlkError::Io);
        return true;
    }
    if let Err(error) = refresh_queue_info(queue, context.state) {
        set_hctx_fatal(context.state, context.fatal_error, error);
        return true;
    }
    context.controller.post(ControllerEvent::RegisterRetry);
    if let Some(delay) = queue.register_retry_after() {
        let delay = if delay.is_zero() {
            Duration::from_micros(1)
        } else {
            delay
        };
        *context.retry_at = Some(now.saturating_add(delay));
    } else {
        *context.deadline = None;
    }
    true
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
            } else if let Err(error) = refresh_queue_info(queue, state) {
                set_hctx_fatal(state, fatal_error, error);
            }
            progressed = true;
        }
        // Queue-owned state must observe the acknowledged hardware event
        // before the controller reacts to the same IRQ. Initialization uses
        // this ordering to publish discovered geometry before Ready.
        // A terminal queue result owns teardown from this point onward. Do not
        // let the failed IRQ race its Watchdog by advancing or rearming the
        // controller after queue state has already declared the device dead.
        if fatal_error.is_some() {
            continue;
        }
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

fn refresh_queue_info(queue: &dyn HardwareQueue, state: &HctxState) -> Result<(), BlkError> {
    let observed = queue.info();
    state.queue_info.lock().observe(observed)
}

fn queue_info_fits_provisioned(provisioned: QueueInfo, observed: QueueInfo) -> bool {
    observed.id == provisioned.id
        && observed.limits.max_inflight > 0
        && observed.limits.max_submit_batch > 0
        && observed.limits.max_inflight <= provisioned.limits.max_inflight
        && observed.limits.max_submit_batch <= provisioned.limits.max_submit_batch
        && observed.limits.max_submit_batch <= observed.limits.max_inflight
}

struct QueueInfoEpoch {
    published: QueueInfo,
    provisioned_max_inflight: usize,
    provisioned_max_submit_batch: usize,
    frozen: bool,
}

impl QueueInfoEpoch {
    const fn new(published: QueueInfo) -> Self {
        Self {
            provisioned_max_inflight: published.limits.max_inflight,
            provisioned_max_submit_batch: published.limits.max_submit_batch,
            published,
            frozen: false,
        }
    }

    const fn published(&self) -> QueueInfo {
        self.published
    }

    fn observe(&mut self, observed: QueueInfo) -> Result<(), BlkError> {
        if self.frozen {
            if observed == self.published {
                return Ok(());
            }
            return Err(BlkError::InvalidRequest);
        }
        let provisioned = QueueInfo {
            limits: rdif_block::QueueLimits {
                max_inflight: self.provisioned_max_inflight,
                max_submit_batch: self.provisioned_max_submit_batch,
                ..self.published.limits
            },
            ..self.published
        };
        if !queue_info_fits_provisioned(provisioned, observed) {
            return Err(BlkError::InvalidRequest);
        }
        self.published = observed;
        Ok(())
    }

    fn freeze(&mut self) {
        self.frozen = true;
    }
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
