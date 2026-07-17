#![no_std]

extern crate alloc;

mod bundle;
mod error;
mod info;
mod init;
mod interface;
mod irq;
mod lifecycle;
mod planner;
mod request;

pub use bundle::{
    BControllerBundle, BundleError, ControllerBundle, LogicalDevice, LogicalDeviceId,
    LogicalDeviceIds, LogicalDeviceParts, MAX_CONTROLLER_QUEUES, MAX_LOGICAL_DEVICES,
    SingleDeviceBundle, validate_controller_devices,
};
pub use dma_api;
pub use error::{BlkError, QueueContractError};
pub use info::{
    DEFAULT_REQUEST_TIMEOUT_NS, DeviceInfo, DispatchMode, QueueInfo, QueueKind, QueueLimits,
};
pub use init::{
    ControllerInit, ControllerInitEndpoint, InitError, InitInput, InitIrqProgress, InitPoll,
    InitSchedule, InitialController,
};
pub use interface::{
    BInterface, BIrqHandler, BQueue, CompletionSink, IQueue, Interface, QueueHandle,
    ServiceContinuation, ServiceContinuationReason, ServiceProgress, validate_lifecycle_activation,
    validate_queue_activation, validate_queue_info, validate_request_identity,
    validate_submit_contract,
};
pub use irq::{
    AcknowledgedEvent, CompletionHint, CompletionIds, CompletionList, DeferredIrqProgress, Event,
    IdList, IrqEventEpoch, IrqHandler, IrqOutcome, IrqSourceInfo, IrqSourceList,
    MAX_BATCH_COMPLETION_IDS, MAX_COMPLETION_HINTS, QueueEventBatch,
};
pub use lifecycle::{
    ControllerEpoch, ControllerReady, DmaQuiesced, InterruptLifecycle, LifecycleEndpoint,
    LifecycleKind, RecoveryCause, validate_lifecycle_identity,
};
pub use planner::{
    TransferChunk, TransferPlan, TransferPlanner, TransferRuntimeCaps, TransferSegment,
    TransferSegments,
};
pub use rdif_base::{DriverGeneric, KError, io};
pub use request::{
    CompletedRequest, OwnedRequest, RequestFlags, RequestId, RequestOp, SubmitError, SubmitOutcome,
    validate_owned_request, validate_owned_request_shape,
};
