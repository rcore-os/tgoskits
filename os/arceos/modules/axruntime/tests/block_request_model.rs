extern crate alloc;

use core::pin::Pin;

use ax_runtime::block::{HardwareQueue, HardwareQueueError, RuntimeSubmitError, SubmittedRequest};
use rdif_block::{CompletedRequest, OwnedRequest};

#[path = "../src/block/request.rs"]
mod request;

#[test]
fn inline_identity_never_enters_the_hardware_tag_namespace() {
    assert_eq!(
        request::RequestTag::from_request_id(rdif_block::RequestId::INLINE),
        Err(request::TagError::InlineIdentity)
    );
}

#[test]
fn interrupt_queue_submit_always_returns_a_waitable_request() {
    let submit: fn(
        Pin<&'static HardwareQueue>,
        OwnedRequest,
    ) -> Result<SubmittedRequest, RuntimeSubmitError> = HardwareQueue::submit_owned;
    let _ = submit;
}

#[test]
fn cancellation_is_requested_before_the_generation_wait_is_consumed() {
    let cancel: fn(&SubmittedRequest) -> Result<bool, HardwareQueueError> =
        SubmittedRequest::request_cancel;
    let wait: fn(SubmittedRequest) -> Result<CompletedRequest, HardwareQueueError> =
        SubmittedRequest::wait;
    let _ = (cancel, wait);
}
