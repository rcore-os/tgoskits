//! Structured failures produced by deterministic VM resource planning.

use alloc::string::String;
use core::fmt;

use axdevice_base::{InterruptControllerId, ItsId};

use super::ResourceSlot;

/// Namespace carried by resource conflict and exhaustion errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceNamespace {
    /// Guest MMIO address space.
    Mmio,
    /// x86 port-I/O address space.
    Pio,
    /// Inputs of one VM-local interrupt controller.
    ControllerInput(InterruptControllerId),
    /// Physical interrupt sources in the host interrupt domain.
    HostIrq,
    /// DeviceID or EventID namespace of one ITS.
    Its {
        /// Controller owning the ITS.
        controller: InterruptControllerId,
        /// VM-local ITS instance.
        its: ItsId,
    },
    /// LPI namespace of one controller.
    Lpi(InterruptControllerId),
    /// Stable device identifiers within one VM plan.
    Device,
}

impl fmt::Display for ResourceNamespace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mmio => formatter.write_str("mmio"),
            Self::Pio => formatter.write_str("pio"),
            Self::ControllerInput(controller) => {
                write!(formatter, "controller-input({})", controller.value())
            }
            Self::HostIrq => formatter.write_str("host-irq"),
            Self::Its { controller, its } => {
                write!(formatter, "its({}:{})", controller.value(), its.value())
            }
            Self::Lpi(controller) => write!(formatter, "lpi({})", controller.value()),
            Self::Device => formatter.write_str("device"),
        }
    }
}

/// Structured planning failure with both owners and the affected namespace.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ResourcePlanningError {
    /// Two requests cannot share the same resource.
    #[error(
        "{namespace} resource {resource} requested by {requester} conflicts with existing owner \
         {existing_owner}: {detail}"
    )]
    Conflict {
        /// Resource namespace.
        namespace: ResourceNamespace,
        /// Numeric range, input, or identifier.
        resource: String,
        /// Existing owner.
        existing_owner: String,
        /// New requester.
        requester: String,
        /// Semantic reason for the conflict.
        detail: &'static str,
    },
    /// No fitting resource remains in the architecture auto-allocation pool.
    #[error("{namespace} auto pool is exhausted while allocating slot {slot} for {requester}")]
    Exhausted {
        /// Resource namespace.
        namespace: ResourceNamespace,
        /// New requester.
        requester: String,
        /// Model-defined slot.
        slot: ResourceSlot,
    },
    /// A fixed resource lies outside the architecture fixed-resource allowlist.
    #[error(
        "fixed {namespace} resource {resource} for {requester} slot {slot} is outside the fixed \
         allowlist"
    )]
    FixedNotAllowed {
        /// Resource namespace.
        namespace: ResourceNamespace,
        /// Requested value or range.
        resource: String,
        /// New requester.
        requester: String,
        /// Model-defined slot.
        slot: ResourceSlot,
    },
}

pub(crate) fn conflict(
    namespace: ResourceNamespace,
    resource: String,
    existing_owner: &str,
    requester: &str,
    detail: &'static str,
) -> ResourcePlanningError {
    ResourcePlanningError::Conflict {
        namespace,
        resource,
        existing_owner: existing_owner.into(),
        requester: requester.into(),
        detail,
    }
}
