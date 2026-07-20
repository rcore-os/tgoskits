//! Typed errors reported by the virtual platform-level interrupt controller.

use alloc::string::String;

use axdevice_base::{AccessWidth, DeviceError};

/// Result type returned by virtual PLIC operations.
pub type VplicResult<T = ()> = Result<T, VplicError>;

/// Errors reported by the virtual platform-level interrupt controller.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VplicError {
    /// The virtual PLIC memory-region size was not provided.
    #[error("vPLIC memory-region size is required")]
    MissingRegionSize,
    /// The virtual PLIC register range overflows the guest address space.
    #[error("vPLIC register range overflows the guest address space")]
    AddressOverflow,
    /// The configured memory region does not cover all PLIC contexts.
    #[error(
        "vPLIC region [{base:#x}, {region_end:#x}) does not cover required end {required_end:#x}"
    )]
    InsufficientRegion {
        /// Base guest physical address of the region.
        base: usize,
        /// Exclusive end of the configured region.
        region_end: usize,
        /// Exclusive end required by the configured contexts.
        required_end: usize,
    },
    /// A PLIC source identifier is outside the valid range.
    #[error("vPLIC source ID {source_id} is outside the valid range 1..{max}")]
    InvalidSource {
        /// The rejected source identifier.
        source_id: usize,
        /// The exclusive upper bound for source identifiers.
        max: usize,
    },
    /// A source is not assigned to this virtual PLIC.
    #[error("vPLIC source ID {source_id} is not assigned to this controller")]
    SourceNotAssigned {
        /// The unassigned source identifier.
        source_id: usize,
    },
    /// A context identifier is outside the configured range.
    #[error("vPLIC context ID {context} is outside the configured range 0..{contexts}")]
    InvalidContext {
        /// The rejected context identifier.
        context: usize,
        /// The number of configured contexts.
        contexts: usize,
    },
    /// A register access uses an unsupported width.
    #[error("invalid vPLIC access width: expected {expected:?}, got {actual:?}")]
    InvalidAccessWidth {
        /// The required register access width.
        expected: AccessWidth,
        /// The requested register access width.
        actual: AccessWidth,
    },
    /// A register operation is unsupported.
    #[error("unsupported vPLIC {operation} at register offset {offset:#x}")]
    UnsupportedRegister {
        /// Whether the access is a read or write.
        operation: &'static str,
        /// The unsupported register offset.
        offset: usize,
    },
    /// A host PLIC or MMIO backend operation failed.
    #[error("vPLIC backend operation {operation} failed: {detail}")]
    Backend {
        /// The backend operation that failed.
        operation: &'static str,
        /// Diagnostic detail from the backend.
        detail: String,
    },
}

impl From<VplicError> for DeviceError {
    fn from(error: VplicError) -> Self {
        match error {
            VplicError::InvalidSource { .. }
            | VplicError::SourceNotAssigned { .. }
            | VplicError::InvalidContext { .. }
            | VplicError::InvalidAccessWidth { .. }
            | VplicError::MissingRegionSize
            | VplicError::AddressOverflow
            | VplicError::InsufficientRegion { .. } => Self::InvalidInput {
                operation: "access RISC-V vPLIC",
                detail: alloc::format!("{error}"),
            },
            VplicError::UnsupportedRegister { .. } => Self::Unsupported {
                operation: "access RISC-V vPLIC",
                detail: alloc::format!("{error}"),
            },
            VplicError::Backend { .. } => Self::Backend {
                operation: "access RISC-V vPLIC",
                detail: alloc::format!("{error}"),
            },
        }
    }
}
