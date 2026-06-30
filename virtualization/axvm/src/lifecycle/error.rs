use alloc::string::{String, ToString};
use core::fmt;

use ax_errno::{AxError, ax_err_type};

use super::{StopReason, VmStatus};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VmLifecycleError {
    InvalidTransition {
        from: VmStatus,
        to: VmStatus,
        op: &'static str,
    },
    MissingResource(&'static str),
    VcpuExit(String),
    Destroy(String),
}

pub type VmLifecycleResult<T = ()> = core::result::Result<T, VmLifecycleError>;

impl VmLifecycleError {
    pub fn invalid_transition(from: VmStatus, to: VmStatus, op: &'static str) -> Self {
        Self::InvalidTransition { from, to, op }
    }

    pub fn into_ax_error(self) -> AxError {
        ax_err_type!(BadState, self.to_string())
    }
}

impl fmt::Display for VmLifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VmLifecycleError::InvalidTransition { from, to, op } => {
                write!(
                    f,
                    "invalid VM lifecycle transition during {op}: {from:?} -> {to:?}"
                )
            }
            VmLifecycleError::MissingResource(name) => write!(f, "missing VM resource: {name}"),
            VmLifecycleError::VcpuExit(err) => write!(f, "vCPU exited with error: {err}"),
            VmLifecycleError::Destroy(err) => write!(f, "VM destroy failed: {err}"),
        }
    }
}

impl From<StopReason> for VmLifecycleError {
    fn from(reason: StopReason) -> Self {
        match reason {
            StopReason::Clean => VmLifecycleError::VcpuExit(String::from("clean stop")),
            StopReason::SystemDown => VmLifecycleError::VcpuExit(String::from("guest system down")),
            StopReason::Forced => VmLifecycleError::VcpuExit(String::from("forced stop")),
            StopReason::Fault(message) => VmLifecycleError::VcpuExit(message),
        }
    }
}
