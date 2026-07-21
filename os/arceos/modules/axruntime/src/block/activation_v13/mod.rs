//! Staged rdif-block v0.13 controller activation.

mod domain;
mod domain_evidence;
mod domain_reclaim;
mod initialization;
mod owner;
mod plan;
mod reinit;
mod request_runtime;
mod selection;
pub(crate) mod shutdown;
mod slot;
mod source;
mod startup;
mod topology;

pub use owner::{
    ReadyControllerCloseFailure, ReadyControllerInstallation, activate_discovered_controllers_v13,
};
pub use plan::*;
pub use request_runtime::{
    V13BlockDeviceView, V13SubmitError, V13SubmitErrorKind, V13SubmittedRequest,
};
use source::*;
pub use topology::*;
