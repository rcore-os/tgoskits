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
}

#[cfg(test)]
mod tests {
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
}
