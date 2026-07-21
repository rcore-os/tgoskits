use rdif_block::{
    AcceptedRequest, BlkError, HardwareNotVisible, InterruptSubmitQueue, OwnedRequest,
    RequestFlags, RequestId, RequestOp, UnacceptedRequest,
};

struct RejectingInterruptQueue;

impl InterruptSubmitQueue for RejectingInterruptQueue {
    fn submit_owned(
        &mut self,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<AcceptedRequest, UnacceptedRequest> {
        Err(UnacceptedRequest::new(id, BlkError::Retry, request))
    }
}

fn flush_request() -> OwnedRequest {
    OwnedRequest {
        op: RequestOp::Flush,
        lba: 0,
        block_count: 0,
        data: None,
        flags: RequestFlags::SYNC,
    }
}

#[test]
fn interrupt_rejection_returns_ownership_and_hardware_not_visible_proof() {
    let id = RequestId::new(9);
    let mut queue = RejectingInterruptQueue;

    let rejected = queue.submit_owned(id, flush_request()).unwrap_err();
    let (returned_id, error, request, proof) = rejected.into_parts();

    assert_eq!(returned_id, id);
    assert_eq!(error, BlkError::Retry);
    assert_eq!(request.op, RequestOp::Flush);
    assert_eq!(request.flags, RequestFlags::SYNC);
    consume_not_visible(proof);
}

#[test]
fn legacy_submit_error_can_enter_the_linear_unaccepted_surface() {
    let id = RequestId::new(11);
    let rejected =
        rdif_block::SubmitError::new(id, BlkError::Offline, flush_request()).into_unaccepted();

    assert_eq!(rejected.id(), id);
    assert_eq!(rejected.error(), BlkError::Offline);
    assert_eq!(rejected.request().op, RequestOp::Flush);
}

fn consume_not_visible(_proof: HardwareNotVisible) {}
