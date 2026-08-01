//! Typed errors reported by the virtual Generic Interrupt Controller.

use alloc::string::String;

use axdevice_base::{AccessWidth, DeviceError};

/// Result type returned by VGIC operations.
pub type VgicResult<T = ()> = Result<T, VgicError>;

/// Errors reported by the virtual Generic Interrupt Controller.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VgicError {
    /// An SPI identifier is outside the architectural non-special range.
    #[error("virtual SPI INTID {value} is outside the supported range 32..=1019")]
    InvalidSpiIntId {
        /// Rejected INTID.
        value: usize,
    },
    /// A vCPU identifier cannot be represented by the controller.
    #[error("virtual CPU identifier {value} exceeds u32::MAX")]
    InvalidVcpuId {
        /// Rejected identifier.
        value: usize,
    },
    /// A route attempted to register an INTID more than once.
    #[error("virtual SPI INTID {intid} already has a route")]
    DuplicateSpiRoute {
        /// Duplicated INTID.
        intid: u32,
    },
    /// A runtime operation was attempted before route registration was sealed.
    #[error("virtual SPI controller routes are not sealed")]
    NotReady,
    /// No registered route owns the requested INTID.
    #[error("virtual SPI INTID {intid} is not registered")]
    UnregisteredSpi {
        /// Unknown INTID.
        intid: u32,
    },
    /// A line operation did not match the registered trigger mode.
    #[error("virtual SPI INTID {intid} uses {actual:?} triggering, not {expected:?}")]
    TriggerMismatch {
        /// Affected INTID.
        intid: u32,
        /// Required trigger mode.
        expected: axdevice_base::InterruptTriggerMode,
        /// Registered trigger mode.
        actual: axdevice_base::InterruptTriggerMode,
    },
    /// The delivery epoch counter cannot allocate another unique instance.
    #[error("virtual SPI delivery epoch space is exhausted")]
    DeliveryEpochExhausted,
    /// A resident LR observation does not match the durable owner or epoch.
    #[error("virtual SPI INTID {intid} resident owner or epoch does not match")]
    ResidentMismatch {
        /// Affected INTID.
        intid: u32,
    },
    /// Controller state does not permit the requested transition.
    #[error("invalid virtual SPI state for {operation}: {detail}")]
    BadState {
        /// Rejected operation.
        operation: &'static str,
        /// Diagnostic detail.
        detail: String,
    },
    /// An IRQ identifier is outside the supported range.
    #[error("VGIC IRQ {irq} is outside the supported range 0..{max}")]
    InvalidIrq {
        /// The rejected IRQ identifier.
        irq: usize,
        /// The exclusive upper bound for valid IRQ identifiers.
        max: usize,
    },
    /// A register access has an invalid address or width.
    #[error("invalid VGIC {operation} at offset {offset:#x} with width {width:?}")]
    InvalidAccess {
        /// Whether the access is a read or write.
        operation: &'static str,
        /// Register offset from the controller base.
        offset: usize,
        /// Width of the register access.
        width: AccessWidth,
    },
    /// A register or controller operation is unsupported.
    #[error("unsupported VGIC operation {operation}: {detail}")]
    Unsupported {
        /// The unsupported operation.
        operation: &'static str,
        /// Diagnostic detail describing the limitation.
        detail: String,
    },
    /// A host GIC or MMIO backend operation failed.
    #[error("VGIC backend operation {operation} failed: {detail}")]
    Backend {
        /// The backend operation that failed.
        operation: &'static str,
        /// Diagnostic detail from the backend.
        detail: String,
    },
}

impl From<VgicError> for DeviceError {
    fn from(error: VgicError) -> Self {
        match error {
            VgicError::InvalidIrq { .. }
            | VgicError::InvalidSpiIntId { .. }
            | VgicError::InvalidVcpuId { .. }
            | VgicError::InvalidAccess { .. }
            | VgicError::TriggerMismatch { .. } => Self::InvalidInput {
                operation: "access ARM VGIC",
                detail: alloc::format!("{error}"),
            },
            VgicError::NotReady
            | VgicError::DuplicateSpiRoute { .. }
            | VgicError::UnregisteredSpi { .. }
            | VgicError::DeliveryEpochExhausted
            | VgicError::ResidentMismatch { .. }
            | VgicError::BadState { .. } => Self::InvalidState {
                operation: "access ARM VGIC",
                detail: alloc::format!("{error}"),
            },
            VgicError::Unsupported { .. } => Self::Unsupported {
                operation: "access ARM VGIC",
                detail: alloc::format!("{error}"),
            },
            VgicError::Backend { .. } => Self::Backend {
                operation: "access ARM VGIC",
                detail: alloc::format!("{error}"),
            },
        }
    }
}
