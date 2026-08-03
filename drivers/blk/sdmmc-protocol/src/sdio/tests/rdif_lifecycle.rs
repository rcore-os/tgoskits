use core::{num::NonZeroUsize, time::Duration};

use rdif_block::{
    BatchSubmitDisposition, BlockController, CompletedRequest, CompletionSink, ControllerEvent,
    ControllerState, OwnedRequest, OwnedRequestBatch, RequestFlags, RequestId, RequestOp,
    SubmissionSink,
};

use super::*;
use crate::rdif::{BlockConfig, BlockDevice};

#[test]
fn controller_teardown_is_idempotent_after_watchdog_shutdown() {
    let host = MockHost::new(Vec::new());
    let config = BlockConfig::dma("sdmmc-test", 1, test_device_dma());
    let mut controller = BlockDevice::new(SdioSdmmc::new(host), config);

    let start = controller
        .advance(ControllerEvent::Start { target_queues: 1 })
        .unwrap();
    assert_eq!(start.controller_state(), ControllerState::Ready);
    assert_eq!(
        controller
            .advance(ControllerEvent::Watchdog { queue_id: 0 })
            .unwrap()
            .controller_state(),
        ControllerState::Shutdown
    );

    assert_eq!(
        controller
            .advance(ControllerEvent::QuiesceIrqs)
            .unwrap()
            .controller_state(),
        ControllerState::Shutdown
    );
    assert_eq!(
        controller
            .advance(ControllerEvent::Shutdown)
            .unwrap()
            .controller_state(),
        ControllerState::Shutdown
    );
}

#[derive(Default)]
struct AcceptedIds(Vec<RequestId>);

impl SubmissionSink for AcceptedIds {
    fn accepted(&mut self, id: RequestId) {
        self.0.push(id);
    }
}

#[derive(Default)]
struct CompletedRequests(Vec<CompletedRequest>);

impl CompletionSink for CompletedRequests {
    fn complete(&mut self, request: CompletedRequest) {
        self.0.push(request);
    }
}

#[test]
fn queue_surfaces_register_retry_requested_after_a_data_irq() {
    let mut host = MockHost::new(Vec::from([ok_r1()]));
    host.complete_after_irq_register_retry = true;
    let config = BlockConfig::dma("sdmmc-test", 8, test_device_dma());
    let mut controller = BlockDevice::new(SdioSdmmc::new(host), config);
    let mut start = controller
        .advance(ControllerEvent::Start { target_queues: 1 })
        .unwrap();
    let mut queue = start.take_queues().remove(0);
    let data = dma_api::CpuDmaBuffer::new_zero(
        test_device_dma(),
        NonZeroUsize::new(512).unwrap(),
        512,
        dma_api::DmaDirection::ToDevice,
    )
    .unwrap()
    .prepare_for_device();
    let mut batch = OwnedRequestBatch::from_iter([OwnedRequest {
        op: RequestOp::Write,
        lba: 0,
        block_count: 1,
        data: Some(data),
        flags: RequestFlags::NONE,
    }]);
    let mut accepted = AcceptedIds::default();

    let result = queue.submit_batch_owned(&mut batch, &mut accepted);
    assert_eq!(result.accepted(), 1);
    assert_eq!(result.disposition(), BatchSubmitDisposition::Continue);
    queue.commit_submissions().unwrap();

    let mut completed = CompletedRequests::default();
    queue.drain_completions(&mut completed).unwrap();

    assert!(completed.0.is_empty());
    assert_eq!(
        queue.register_retry_after(),
        Some(Duration::from_millis(1)),
        "a protocol register wait created during IRQ drain must be scheduled by the runtime"
    );
    queue.advance_register_retry(&mut completed).unwrap();
    assert_eq!(completed.0.len(), 1);
    assert_eq!(completed.0[0].result, Ok(()));
}
