//! Linux blk-mq style request runtime for rdif-block v0.13.

mod gates;
mod io_port;
mod mq;
mod owner;
mod service;
mod software_ctx;
mod table;

pub(super) use io_port::RuntimeIoDomainPort;
pub(super) use mq::{
    DomainRequestRuntime, DomainSubmitEndpoint, RequestRuntimeBuildError, build_published_devices,
};
pub use mq::{V13BlockDeviceView, V13SubmitError, V13SubmitErrorKind, V13SubmittedRequest};
pub(super) use owner::{
    DomainRequestLifecycleError, DomainRequestOwner, DomainRequestServiceError,
};
