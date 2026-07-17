//! Linux-style block runtime primitives owned by ax-runtime.

mod activation;
mod controller;
mod event_ring;
mod handoff;
mod hctx;
pub mod hctx_model;
mod request;
mod service;
mod statistics;

pub use controller::{
    BlockController, BlockControllerError, BlockDeviceView, BlockHandoffError,
    activate_discovered_controllers, block_io_stats,
};
pub(crate) use event_ring::{EventRing, RingFull};
pub use handoff::{
    BlockControllerIdentity, BlockHandoffCommitFailure, BlockHandoffReturnFailure,
    GuestAccessRevoked, GuestOwnedBlockControllers, GuestPassthroughRegion, HostPciEndpoint,
    HostPhysicalRange, HostPhysicalRangeError, HostRunningBlockControllers, PreparedBlockHandoff,
    QuarantinedBlockControllers, StorageGuestKey, prepare_runtime_controllers_for_passthrough,
};
pub use hctx::{
    HardwareQueue, HardwareQueueError, QuiescedHardwareQueue, RuntimeSubmitError, SubmittedRequest,
};
pub use hctx_model::{
    DispatchArbiter, DispatchSource, HctxCause, HctxControl, HctxPhase, HctxTerminalGate,
    HctxTransition, HctxTransitionError, ServiceBatch, ServiceBudget, ServiceBudgetError,
    ServiceContinuation, ServiceStage,
};
pub(crate) use hctx_model::{HctxAccessGate, HctxAccessPermit};
pub(crate) use request::{RequestState, RequestTag, RequestTagSet, TagError};
pub use service::BlockServiceError;
pub use statistics::BlockIoStats;
