use alloc::{boxed::Box, vec::Vec};

use crate::{
    BlkError, DeviceInfo, DriverGeneric, IrqHandler, IrqSourceList, OwnedRequest, PollError,
    QueueInfo, QueueLimits, Request, RequestId, RequestPoll, RequestStatus, SubmitError,
};

pub type BInterface = Box<dyn Interface>;
pub type BQueue = Box<dyn IQueue>;
pub type BOwnedQueue = Box<dyn IQueueOwned>;
pub type BIrqHandler = Box<dyn IrqHandler>;

pub trait Interface: DriverGeneric {
    fn device_info(&self) -> DeviceInfo;

    fn queue_limits(&self) -> QueueLimits;

    fn create_queue(&mut self) -> Option<BQueue>;

    fn create_owned_queue(&mut self) -> Option<QueueHandle> {
        None
    }

    fn enable_irq(&self) {}

    fn disable_irq(&self) {}

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn irq_sources(&self) -> IrqSourceList {
        Vec::new()
    }

    fn take_irq_handler(&mut self, _source_id: usize) -> Option<BIrqHandler> {
        None
    }
}

pub trait CompletionSink {
    fn complete(&mut self, request: RequestId, result: Result<(), BlkError>);
}

/// A request queue that owns DMA backing for every in-flight request.
pub trait IQueueOwned: Send + 'static {
    fn id(&self) -> usize;

    fn info(&self) -> QueueInfo;

    fn submit_request(&mut self, request: OwnedRequest) -> Result<RequestId, SubmitError>;

    fn poll_request(&mut self, request: RequestId) -> Result<RequestPoll, PollError>;

    fn cancel_request(&mut self, request: RequestId) -> Result<RequestPoll, PollError>;

    fn shutdown(&mut self) {}
}

pub struct QueueHandle {
    queue: Option<BOwnedQueue>,
}

impl QueueHandle {
    pub fn new(queue: BOwnedQueue) -> Self {
        Self { queue: Some(queue) }
    }

    pub fn id(&self) -> usize {
        self.queue
            .as_ref()
            .expect("owned queue handle must contain queue")
            .id()
    }

    pub fn info(&self) -> QueueInfo {
        self.queue
            .as_ref()
            .expect("owned queue handle must contain queue")
            .info()
    }

    pub fn submit_request(&mut self, request: OwnedRequest) -> Result<RequestId, SubmitError> {
        self.queue
            .as_mut()
            .expect("owned queue handle must contain queue")
            .submit_request(request)
    }

    pub fn poll_request(&mut self, request: RequestId) -> Result<RequestPoll, PollError> {
        self.queue
            .as_mut()
            .expect("owned queue handle must contain queue")
            .poll_request(request)
    }

    pub fn cancel_request(&mut self, request: RequestId) -> Result<RequestPoll, PollError> {
        self.queue
            .as_mut()
            .expect("owned queue handle must contain queue")
            .cancel_request(request)
    }

    pub fn shutdown(&mut self) {
        if let Some(queue) = self.queue.as_mut() {
            queue.shutdown();
        }
    }
}

impl Drop for QueueHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A request queue for one block device hardware/software queue.
///
/// # Safety
///
/// Implementers may access `Request` segments after `submit_request` returns
/// and until the matching `poll_request` returns `RequestStatus::Complete` or
/// an error. They must not access any segment before `submit_request` is called
/// or after completion/error has been reported, and request IDs must not alias
/// two concurrently pending requests in a way that extends this lifetime.
pub unsafe trait IQueue: Send + 'static {
    fn id(&self) -> usize;

    fn info(&self) -> QueueInfo;

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError>;

    fn poll_request(&mut self, request: RequestId) -> Result<RequestStatus, BlkError>;

    /// Poll a set of in-flight requests and report observed terminal results.
    ///
    /// Implementers must report per-request completion or failure through
    /// `sink.complete`. The return value describes the batch query itself:
    /// `Err` means this poll attempt did not reliably observe request states
    /// and must not be interpreted as terminal status for any request that was
    /// not reported to the sink.
    ///
    /// `BlkError::Retry` is submit-side backpressure only. `poll_request` and
    /// `poll_completions` must not return `Retry`; pending requests are
    /// represented by omitting them from the sink in this method.
    fn poll_completions(
        &mut self,
        requests: &[RequestId],
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        for &request in requests {
            match self.poll_request(request) {
                Ok(RequestStatus::Pending) => {}
                Ok(RequestStatus::Complete) => sink.complete(request, Ok(())),
                Err(err) => sink.complete(request, Err(err)),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::{DeviceInfo, Event, QueueLimits, RequestOp};

    struct NoopIrq {
        calls: usize,
    }

    impl IrqHandler for NoopIrq {
        fn handle_irq(&mut self) -> Event {
            self.calls += 1;
            let mut event = Event::none();
            event.queues.insert(1);
            event
        }
    }

    struct Queue;

    // SAFETY: This test queue never stores request segments beyond
    // `submit_request` and reports completion immediately.
    unsafe impl IQueue for Queue {
        fn id(&self) -> usize {
            1
        }

        fn info(&self) -> QueueInfo {
            QueueInfo {
                id: 1,
                device: DeviceInfo::new(8, 512),
                limits: QueueLimits::simple(512, u64::MAX),
            }
        }

        fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
            assert!(matches!(request.op, RequestOp::Read | RequestOp::Write));
            Ok(RequestId::new(1))
        }

        fn poll_request(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
            Ok(RequestStatus::Complete)
        }
    }

    #[test]
    fn block_api_uses_unified_queue_and_irq_events() {
        fn assert_queue<T: IQueue>() {}
        fn assert_irq_handler<T: IrqHandler>() {}

        assert_queue::<Queue>();
        assert_irq_handler::<NoopIrq>();

        let mut irq = NoopIrq { calls: 0 };
        let event = irq.handle_irq();
        assert!(event.queues.contains(1));
        assert_eq!(irq.calls, 1);
    }

    #[derive(Default)]
    struct RecordingSink {
        completions: Vec<(RequestId, Result<(), BlkError>)>,
    }

    impl CompletionSink for RecordingSink {
        fn complete(&mut self, request_id: RequestId, result: Result<(), BlkError>) {
            self.completions.push((request_id, result));
        }
    }

    struct BatchQueue;

    // SAFETY: This test queue never stores request segments and reports
    // synthetic completion status from the requested ID only.
    unsafe impl IQueue for BatchQueue {
        fn id(&self) -> usize {
            0
        }

        fn info(&self) -> QueueInfo {
            QueueInfo {
                id: 0,
                device: DeviceInfo::new(8, 512),
                limits: QueueLimits::simple(512, u64::MAX),
            }
        }

        fn submit_request(&mut self, _request: Request<'_>) -> Result<RequestId, BlkError> {
            Ok(RequestId::new(0))
        }

        fn poll_request(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
            match usize::from(request) {
                1 => Ok(RequestStatus::Complete),
                2 => Ok(RequestStatus::Pending),
                3 => Err(BlkError::Io),
                _ => Err(BlkError::InvalidRequest),
            }
        }
    }

    #[test]
    fn default_batch_completion_polls_pending_ids_and_reports_terminal_results() {
        let mut queue = BatchQueue;
        let mut sink = RecordingSink::default();
        let ids = [RequestId::new(1), RequestId::new(2), RequestId::new(3)];

        queue.poll_completions(&ids, &mut sink).unwrap();

        assert_eq!(
            sink.completions,
            [
                (RequestId::new(1), Ok(())),
                (RequestId::new(3), Err(BlkError::Io)),
            ]
        );
    }

    #[test]
    fn simple_queue_limits_are_single_inflight() {
        let limits = QueueLimits::simple(512, u64::MAX);

        assert_eq!(limits.max_inflight, 1);
    }

    struct CountingIrq {
        count: usize,
    }

    impl IrqHandler for CountingIrq {
        fn handle_irq(&mut self) -> Event {
            self.count += 1;
            Event::from_queue_bits(self.count as u64)
        }
    }

    #[test]
    fn boxed_irq_handler_is_move_only_and_mutable() {
        let mut handler: BIrqHandler = Box::new(CountingIrq { count: 0 });

        assert_eq!(handler.handle_irq().queues.bits(), 1);
        assert_eq!(handler.handle_irq().queues.bits(), 2);
    }

    struct OwnedQueue;

    impl IQueueOwned for OwnedQueue {
        fn id(&self) -> usize {
            2
        }

        fn info(&self) -> QueueInfo {
            QueueInfo {
                id: 2,
                device: DeviceInfo::new(8, 512),
                limits: QueueLimits::simple(512, u64::MAX),
            }
        }

        fn submit_request(&mut self, request: OwnedRequest) -> Result<RequestId, SubmitError> {
            Err(SubmitError::new(BlkError::Retry, request))
        }

        fn poll_request(&mut self, _request: RequestId) -> Result<RequestPoll, PollError> {
            Ok(RequestPoll::Pending)
        }

        fn cancel_request(&mut self, _request: RequestId) -> Result<RequestPoll, PollError> {
            Err(PollError::UnknownRequest)
        }
    }

    #[test]
    fn owned_queue_submit_error_returns_request_ownership() {
        let mut handle = QueueHandle::new(Box::new(OwnedQueue));
        let request = OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: Default::default(),
        };

        let err = handle.submit_request(request).unwrap_err();

        assert_eq!(err.error, BlkError::Retry);
        assert!(matches!(err.request().op, RequestOp::Flush));
    }
}
