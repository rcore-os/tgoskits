use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use std::{
    sync::{Mutex, mpsc},
    thread,
    time::Instant,
};

use rdif_block::{
    ControlEvent, ControllerEvent, DeviceInfo, DriverGeneric, HardIrqHandler, IrqAck, IrqQueueMask,
    QueueLimits,
};

use super::{submission::collect_submission_batch, *};
use crate::block::runtime::{completion::CompletionSubscription, irq::BlockIrqAction};

mod progress;
mod queue_info;
mod submission;

#[derive(Default)]
struct QueueCounters {
    submitted: AtomicUsize,
    committed: AtomicUsize,
    drained: AtomicUsize,
    shutdown: AtomicUsize,
    dropped: AtomicUsize,
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

struct FailingDrainQueue {
    counters: Arc<QueueCounters>,
}

struct BlockingShutdownQueue {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl DriverGeneric for BlockingShutdownQueue {
    fn name(&self) -> &str {
        "blocking-shutdown"
    }
}

impl HardwareQueue for BlockingShutdownQueue {
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
        Ok(())
    }

    fn shutdown(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        self.entered.send(()).map_err(|_| BlkError::Io)?;
        self.release.recv().map_err(|_| BlkError::Io)
    }
}

impl DriverGeneric for FailingDrainQueue {
    fn name(&self) -> &str {
        "failing-drain"
    }
}

impl HardwareQueue for FailingDrainQueue {
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
        self.counters.drained.fetch_add(1, Ordering::AcqRel);
        Err(BlkError::Io)
    }

    fn shutdown(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        self.counters.shutdown.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
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

impl Drop for UnderreportedAcceptanceQueue {
    fn drop(&mut self) {
        self.counters.dropped.fetch_add(1, Ordering::AcqRel);
    }
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

struct QueueZeroControlIrq;

impl HardIrqHandler for QueueZeroControlIrq {
    fn ack(&mut self) -> IrqAck {
        IrqAck::masked_needs_rearm(IrqQueueMask::from_queue(0), ControlEvent::new(0, 1))
    }
}

fn test_queue_info(depth: usize) -> QueueInfo {
    let mut limits = QueueLimits::simple(
        512,
        dma_api::DmaDeviceInfo::new(
            dma_api::DmaDomainId::Direct,
            dma_api::DmaCoherency::NonCoherent,
            dma_api::DmaConstraints::new(u64::MAX),
        ),
    );
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
