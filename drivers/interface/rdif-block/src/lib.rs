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
    SingleDeviceBundle, UnpublishedQueueQuarantine, validate_controller_devices,
};
pub use dma_api;
pub use error::{BlkError, IrqControlError, QueueContractError};
pub use info::{
    DEFAULT_REQUEST_TIMEOUT_NS, DeviceInfo, QueueExecution, QueueInfo, QueueKind, QueueLimits,
};
pub use init::{
    ControllerInit, ControllerInitEndpoint, InitError, InitInput, InitPoll, InitSchedule,
    InitialController,
};
pub use interface::{
    BInterface, BIrqControl, BIrqEndpoint, BQueue, BlockIrqSource, CompletionSink, IQueue,
    Interface, QuarantinedQueue, QueueCloseFailure, QueueHandle, ServiceProgress, ServiceRerun,
    ServiceRerunReason, validate_lifecycle_activation, validate_queue_activation,
    validate_queue_info, validate_request_identity, validate_submit_contract,
};
pub use irq::{
    AcknowledgedEvent, BlockIrqCapture, Event, IdList, IrqEventEpoch, IrqSourceInfo, IrqSourceList,
    QueueEventBatch,
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
pub use rdif_irq::{
    ContainmentCause, FaultContainment, IrqCapture, IrqEndpoint, IrqSourceControl,
    IrqSourceMaskState, IrqSourceState, MaskedSource, MaskedSourceError,
};
pub use request::{
    CompletedRequest, OwnedRequest, RequestFlags, RequestId, RequestOp, SubmitError, SubmitOutcome,
    validate_owned_request, validate_owned_request_shape,
};
