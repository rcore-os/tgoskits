#![no_std]

extern crate alloc;

mod error;
mod group;
mod hardware;
mod info;
mod irq;
mod planner;
mod request;

#[cfg(all(axtest, feature = "axtest"))]
pub mod axtest;

pub use dma_api;
pub use error::BlkError;
pub use group::{
    BBlockControllerGroup, BlockControllerGroup, BlockGroupMember, GroupControllerEvent,
    GroupControllerUpdate, GroupIrqEvent, GroupIrqSink, GroupIrqTarget, SharedHardIrqHandler,
    SharedIrqEndpoint,
};
pub use hardware::{
    BBlockController, BHardwareQueue, BatchSubmitDisposition, BatchSubmitResult, BlockController,
    CompletionSink, ControlEvent, ControllerEvent, ControllerState, ControllerUpdate,
    HardwareQueue, IrqEndpoint, SubmissionSink,
};
pub use info::{DeviceInfo, QueueInfo, QueueLimits};
pub use irq::{HardIrqHandler, IrqAck, IrqDisposition, IrqQueueMask};
pub use planner::{
    TransferChunk, TransferPlan, TransferPlanner, TransferRuntimeCaps, TransferSegment,
    TransferSegments,
};
pub use rdif_base::{DriverGeneric, KError, io};
pub use request::{
    BatchSubmitError, CompletedRequest, OwnedRequest, OwnedRequestBatch, RequestFlags, RequestId,
    RequestOp, SubmitError, validate_owned_request, validate_owned_request_shape,
};
