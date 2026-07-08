#![no_std]

extern crate alloc;

mod error;
mod info;
mod interface;
mod irq;
mod planner;
mod request;

pub use dma_api;
pub use error::BlkError;
pub use info::{DeviceInfo, QueueInfo, QueueLimits};
pub use interface::{
    BInterface, BIrqHandler, BOwnedQueue, BQueue, CompletionSink, IQueue, IQueueOwned, Interface,
    QueueHandle,
};
pub use irq::{
    CompletionHint, CompletionIds, CompletionList, Event, IdList, IrqHandler, IrqSourceInfo,
    IrqSourceList, MAX_BATCH_COMPLETION_IDS, MAX_COMPLETION_HINTS,
};
pub use planner::{
    TransferChunk, TransferPlan, TransferPlanner, TransferRuntimeCaps, TransferSegment,
    TransferSegments,
};
pub use rdif_base::{DriverGeneric, KError, io};
pub use request::{
    Buffer, CompletedRequest, OwnedRequest, PollError, Request, RequestFlags, RequestId, RequestOp,
    RequestPoll, RequestStatus, Segment, SubmitError, validate_owned_request,
    validate_owned_request_shape, validate_request, validate_request_shape,
};
