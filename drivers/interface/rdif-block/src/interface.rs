use alloc::{boxed::Box, vec::Vec};

use crate::{
    BlkError, DeviceInfo, DriverGeneric, IrqHandler, IrqSourceList, QueueInfo, QueueLimits,
    Request, RequestId, RequestStatus,
};

pub trait Interface: DriverGeneric {
    fn device_info(&self) -> DeviceInfo;

    fn queue_limits(&self) -> QueueLimits;

    fn create_queue(&mut self) -> Option<Box<dyn IQueue>>;

    fn enable_irq(&self) {}

    fn disable_irq(&self) {}

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn irq_sources(&self) -> IrqSourceList {
        Vec::new()
    }

    fn take_irq_handler(&mut self, _source_id: usize) -> Option<Box<dyn IrqHandler>> {
        None
    }
}

pub trait CompletionSink {
    fn complete(&mut self, request: RequestId, result: Result<(), BlkError>);
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

    struct NoopIrq;

    impl IrqHandler for NoopIrq {
        fn handle_irq(&self) -> Event {
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

        let event = NoopIrq.handle_irq();
        assert!(event.queues.contains(1));
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
}
