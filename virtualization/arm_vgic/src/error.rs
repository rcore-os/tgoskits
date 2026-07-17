//! Typed errors reported by the virtual Generic Interrupt Controller.

use alloc::string::String;

use axdevice_base::{AccessWidth, DeviceError};

/// Result type returned by VGIC operations.
pub type VgicResult<T = ()> = Result<T, VgicError>;

/// Errors reported by the virtual Generic Interrupt Controller.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VgicError {
    /// An IRQ identifier is outside the supported range.
    #[error("VGIC IRQ {irq} is outside the supported range 0..{max}")]
    InvalidIrq {
        /// The rejected IRQ identifier.
        irq: usize,
        /// The exclusive upper bound for valid IRQ identifiers.
        max: usize,
    },
    /// An operation requiring an SPI received an SGI, PPI, or special INTID.
    #[error("VGIC IRQ {irq} is not a shared peripheral interrupt")]
    NotSpi {
        /// The rejected interrupt identifier.
        irq: usize,
    },
    /// Another ownership transition prevents this operation from starting.
    #[error("VGIC resource is busy during {operation}")]
    Busy {
        /// The operation that could not start.
        operation: &'static str,
    },
    /// A revocation token no longer names the active ownership generation.
    #[error(
        "stale VGIC SPI revocation generation {generation}; active generation is \
         {active_generation}"
    )]
    StaleRevocation {
        /// Generation carried by the revocation token.
        generation: u64,
        /// Currently active generation, or zero when no revocation is active.
        active_generation: u64,
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
            | VgicError::NotSpi { .. }
            | VgicError::InvalidAccess { .. } => Self::InvalidInput {
                operation: "access ARM VGIC",
                detail: alloc::format!("{error}"),
            },
            VgicError::Busy { operation } => Self::ResourceBusy {
                operation,
                resource: "VGIC SPI ownership".into(),
            },
            VgicError::StaleRevocation { .. } => Self::InvalidState {
                operation: "revoke ARM VGIC SPI ownership",
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
