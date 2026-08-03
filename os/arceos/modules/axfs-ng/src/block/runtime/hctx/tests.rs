use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use std::{sync::Mutex, thread, time::Instant};

use rdif_block::{
    ControlEvent, ControllerEvent, DeviceInfo, DriverGeneric, HardIrqHandler, IrqAck, IrqQueueMask,
    QueueLimits,
};

use super::{submission::collect_submission_batch, *};
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
    fn request_completed(&self, _op: RequestOp, _block_count: u32, _result: Result<(), BlkError>) {
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

    fn call(&self, event: ControllerEvent) -> Result<rdif_block::ControllerState, BlkError> {
        self.events.lock().unwrap().push(event);
        Ok(
            if matches!(
                event,
                ControllerEvent::Shutdown | ControllerEvent::Watchdog { .. }
            ) {
                rdif_block::ControllerState::Shutdown
            } else {
                rdif_block::ControllerState::Ready
            },
        )
    }
}

struct NeverCompletesQueue {
    counters: Arc<QueueCounters>,
    next_id: usize,
    pending: Vec<RequestId>,
}

struct RegisterRetryQueue {
    retry_after: Option<Duration>,
    retries: Arc<AtomicUsize>,
    drains: Arc<AtomicUsize>,
}

struct CapabilityRefreshQueue {
    counters: Arc<QueueCounters>,
    initialized: Arc<AtomicBool>,
}

impl DriverGeneric for CapabilityRefreshQueue {
    fn name(&self) -> &str {
        "capability-refresh"
    }
}

impl HardwareQueue for CapabilityRefreshQueue {
    fn id(&self) -> usize {
        0
    }

    fn info(&self) -> QueueInfo {
        let initialized = self.initialized.load(Ordering::Acquire);
        let mut info = test_queue_info(1);
        info.limits.supports_flush = initialized;
        info.limits.max_blocks_per_request = if initialized { 8192 } else { 256 };
        info
    }

    fn submit_batch_owned(
        &mut self,
        _requests: &mut OwnedRequestBatch,
        _sink: &mut dyn SubmissionSink,
    ) -> rdif_block::BatchSubmitResult {
        rdif_block::BatchSubmitResult::new(0, BatchSubmitDisposition::Continue)
    }

    fn commit_submissions(&mut self) -> Result<(), BlkError> {
        Ok(())
    }

    fn drain_completions(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        self.initialized.store(true, Ordering::Release);
        self.counters.drained.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn shutdown(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        Ok(())
    }
}

impl DriverGeneric for RegisterRetryQueue {
    fn name(&self) -> &str {
        "register-retry"
    }
}

impl HardwareQueue for RegisterRetryQueue {
    fn id(&self) -> usize {
        0
    }

    fn info(&self) -> QueueInfo {
        test_queue_info(1)
    }

    fn submit_batch_owned(
        &mut self,
        _requests: &mut OwnedRequestBatch,
        _sink: &mut dyn SubmissionSink,
    ) -> rdif_block::BatchSubmitResult {
        rdif_block::BatchSubmitResult::new(0, BatchSubmitDisposition::Continue)
    }

    fn commit_submissions(&mut self) -> Result<(), BlkError> {
        Ok(())
    }

    fn drain_completions(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        self.drains.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn register_retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    fn advance_register_retry(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        self.retries.fetch_add(1, Ordering::AcqRel);
        self.retry_after = None;
        Ok(())
    }

    fn shutdown(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        Ok(())
    }
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

#[test]
fn negotiated_queue_depth_may_shrink_but_never_grow() {
    assert!(queue_info_fits_provisioned(
        test_queue_info(32),
        test_queue_info(8)
    ));
    assert!(queue_info_fits_provisioned(
        test_queue_info(32),
        test_queue_info(32)
    ));
    assert!(!queue_info_fits_provisioned(
        test_queue_info(8),
        test_queue_info(32)
    ));
}

fn flush_submission() -> (CompletionSubscription, Submission) {
    flush_submission_at(0)
}

fn flush_submission_at(lba: u64) -> (CompletionSubscription, Submission) {
    let (subscription, completion) = CompletionSubscription::pair().unwrap();
    (
        subscription,
        Submission {
            request: OwnedRequest {
                op: RequestOp::Flush,
                lba,
                block_count: 0,
                data: None,
                flags: RequestFlags::NONE,
            },
            completion,
        },
    )
}

#[test]
fn register_retry_advances_only_register_state_and_posts_controller_event() {
    crate::os::task::install_test_runtime_ops();
    let ops = runtime_ops().unwrap();
    let state = HctxState {
        info: IrqMutex::new(test_queue_info(1)),
        submission_channels: IrqMutex::new(Vec::new()),
        notification: ops.notification(),
        lifecycle_notification: ops.notification(),
        irq_latches: IrqMutex::new(Vec::new()),
        quiescing: AtomicBool::new(false),
        quiesced: AtomicBool::new(false),
        stopping: AtomicBool::new(false),
        terminated: AtomicBool::new(false),
    };
    let retries = Arc::new(AtomicUsize::new(0));
    let drains = Arc::new(AtomicUsize::new(0));
    let mut queue = RegisterRetryQueue {
        retry_after: Some(Duration::from_millis(2)),
        retries: Arc::clone(&retries),
        drains: Arc::clone(&drains),
    };
    let controller = TestControllerPort::default();
    let now = Duration::from_secs(10);
    let mut retry_at = None;
    let mut deadline = None;
    let mut fatal_error = None;
    let mut pending = BTreeMap::new();
    let observer: Arc<dyn HctxObserver> = Arc::new(TestObserver::default());
    let observer = Arc::downgrade(&observer);

    reconcile_register_retry(&queue, &mut retry_at, &mut deadline, now);
    assert_eq!(retry_at, Some(now + Duration::from_millis(2)));
    assert_eq!(deadline, Some(now + QUEUE_REGISTER_TRANSITION_TIMEOUT));
    assert!(!advance_register_retry_if_due(
        &mut queue,
        now + Duration::from_millis(1),
        &mut RegisterRetryContext {
            controller: &controller,
            pending: &mut pending,
            observer: &observer,
            retry_at: &mut retry_at,
            deadline: &mut deadline,
            state: &state,
            fatal_error: &mut fatal_error,
        },
    ));
    assert!(advance_register_retry_if_due(
        &mut queue,
        now + Duration::from_millis(2),
        &mut RegisterRetryContext {
            controller: &controller,
            pending: &mut pending,
            observer: &observer,
            retry_at: &mut retry_at,
            deadline: &mut deadline,
            state: &state,
            fatal_error: &mut fatal_error,
        },
    ));

    assert_eq!(retries.load(Ordering::Acquire), 1);
    assert_eq!(drains.load(Ordering::Acquire), 0);
    assert_eq!(fatal_error, None);
    assert_eq!(
        controller.events.lock().unwrap().as_slice(),
        [ControllerEvent::RegisterRetry]
    );
}

#[test]
fn retry_backlog_does_not_starve_fresh_cpu_channel_submissions() {
    crate::os::task::install_test_runtime_ops();
    let ops = runtime_ops().unwrap();
    let notification = ops.notification();
    let channel =
        Arc::new(BoundedChannel::with_item_notification(4, Arc::clone(&notification)).unwrap());
    let state = HctxState {
        info: IrqMutex::new(test_queue_info(2)),
        submission_channels: IrqMutex::new(vec![Arc::clone(&channel)]),
        notification,
        lifecycle_notification: ops.notification(),
        irq_latches: IrqMutex::new(Vec::new()),
        quiescing: AtomicBool::new(false),
        quiesced: AtomicBool::new(false),
        stopping: AtomicBool::new(false),
        terminated: AtomicBool::new(false),
    };
    let (_fresh_subscription, fresh) = flush_submission_at(100);
    assert!(channel.send(fresh, false).is_ok());
    let (_first_retry_subscription, first_retry) = flush_submission_at(1);
    let (_second_retry_subscription, second_retry) = flush_submission_at(2);
    let mut retries = VecDeque::from([first_retry, second_retry]);
    let mut next_channel = 0;
    let mut prefer_retry = true;
    let mut batch = VecDeque::with_capacity(2);

    collect_submission_batch(
        &state,
        &mut retries,
        &mut next_channel,
        &mut prefer_retry,
        2,
        &mut batch,
    );
    let lbas: Vec<_> = batch
        .into_iter()
        .map(|submission| submission.request.lba)
        .collect();

    assert_eq!(lbas, [1, 100]);
    assert_eq!(retries.front().unwrap().request.lba, 2);
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
fn irq_drain_refreshes_hctx_queue_capabilities() {
    crate::os::task::install_test_runtime_ops();
    let counters = Arc::new(QueueCounters::default());
    let initialized = Arc::new(AtomicBool::new(false));
    let observer: Arc<dyn HctxObserver> = Arc::new(TestObserver::default());
    let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
    let queue = CapabilityRefreshQueue {
        counters: Arc::clone(&counters),
        initialized: Arc::clone(&initialized),
    };
    let hctx = Hctx::start(Box::new(queue), 0, Arc::downgrade(&observer), controller).unwrap();

    let initial = hctx.info();
    assert!(!initial.limits.supports_flush);
    assert_eq!(initial.limits.max_blocks_per_request, 256);

    let target = hctx.irq_target(0);
    let mut action = BlockIrqAction::new(Box::new(QueueZeroIrq), vec![target]);
    assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);
    let deadline = Instant::now() + Duration::from_secs(1);
    while !hctx.info().limits.supports_flush {
        assert!(
            Instant::now() < deadline,
            "maintenance task did not publish identified queue capabilities"
        );
        thread::yield_now();
    }

    let refreshed = hctx.info();
    assert_eq!(counters.drained.load(Ordering::Acquire), 1);
    assert!(refreshed.limits.supports_flush);
    assert_eq!(refreshed.limits.max_blocks_per_request, 8192);
    assert_eq!(refreshed.id, initial.id);
    assert_eq!(refreshed.limits.max_inflight, initial.limits.max_inflight);
    assert_eq!(
        refreshed.limits.max_submit_batch,
        initial.limits.max_submit_batch
    );
    hctx.stop();
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
