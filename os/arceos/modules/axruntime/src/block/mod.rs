//! Linux-style block runtime primitives owned by ax-runtime.

mod activation;
mod activation_v13;
mod config;
mod controller;
mod event_ring;
mod handoff;
mod hctx;
pub mod hctx_model;
mod inline;
mod quarantine;
mod request;
mod service;
mod statistics;

pub use activation_v13::{
    ReadyControllerCloseFailure, ReadyControllerInstallation, V13ActivationError,
    V13BlockDeviceView, V13SubmitError, V13SubmitErrorKind, V13SubmittedRequest,
    activate_discovered_controllers_v13,
};
pub use config::{BlockRuntimeConfig, DEFAULT_REQUEST_WATCHDOG_NS};
pub use controller::{
    BlockController, BlockControllerError, BlockDeviceView, BlockHandoffError,
    activate_discovered_controllers, activate_discovered_controllers_with_config, block_io_stats,
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
pub use inline::InlineBlockDeviceView;
pub(crate) use request::{RequestState, RequestTag, RequestTagSet, TagError};
pub use service::BlockServiceError;
pub use statistics::BlockIoStats;
