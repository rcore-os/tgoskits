//! Hypercall error contract.

use alloc::string::String;

use crate::HyperCallCode;

/// Result type returned by hypercall operations.
pub type HyperCallResult<T = usize> = Result<T, HyperCallError>;

/// Error returned when a raw value is not a supported hypercall code.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid hypercall code: {0:#x}")]
pub struct InvalidHyperCallCode(
    /// The invalid numeric hypercall code.
    pub u32,
);

/// Errors reported while decoding or executing a hypercall.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HyperCallError {
    /// A raw hypercall code is not defined by the interface.
    #[error(transparent)]
    InvalidCode(#[from] InvalidHyperCallCode),
    /// The hypercall code is valid but not implemented by the hypervisor.
    #[error("hypercall {code:?} is unsupported: {detail}")]
    Unsupported {
        /// The unsupported hypercall code.
        code: HyperCallCode,
        /// Diagnostic detail describing the unsupported operation.
        detail: String,
    },
    /// A hypercall argument is invalid.
    #[error("hypercall {code:?} received invalid parameter {parameter}: {detail}")]
    InvalidParameter {
        /// The hypercall being executed.
        code: HyperCallCode,
        /// The invalid parameter name.
        parameter: &'static str,
        /// Diagnostic detail describing the invalid value.
        detail: String,
    },
    /// Current hypervisor or resource state does not allow the operation.
    #[error("hypercall {code:?} cannot run in the current state: {detail}")]
    InvalidState {
        /// The hypercall being executed.
        code: HyperCallCode,
        /// Diagnostic detail describing the current state.
        detail: String,
    },
    /// A resource required by the hypercall does not exist.
    #[error("hypercall {code:?} could not find resource {resource}: {detail}")]
    ResourceNotFound {
        /// The hypercall being executed.
        code: HyperCallCode,
        /// The missing resource.
        resource: String,
        /// Diagnostic detail identifying the failed lookup.
        detail: String,
    },
    /// A resource requested by the hypercall conflicts with existing state.
    #[error("hypercall {code:?} resource conflict for {resource}: {detail}")]
    ResourceConflict {
        /// The hypercall being executed.
        code: HyperCallCode,
        /// The conflicting resource.
        resource: String,
        /// Diagnostic detail identifying the conflict.
        detail: String,
    },
    /// Memory allocation for the hypercall failed.
    #[error("hypercall {code:?} ran out of memory while {operation}")]
    OutOfMemory {
        /// The hypercall being executed.
        code: HyperCallCode,
        /// The allocation operation that failed.
        operation: &'static str,
    },
    /// Reading from or writing to guest memory failed.
    #[error("hypercall {code:?} failed to {operation} at guest address {address:#x}: {detail}")]
    GuestMemoryAccess {
        /// The hypercall being executed.
        code: HyperCallCode,
        /// The guest-memory operation that failed.
        operation: &'static str,
        /// Guest physical address supplied by the caller.
        address: usize,
        /// Diagnostic detail from the VM memory layer.
        detail: String,
    },
    /// An internal hypervisor operation failed.
    #[error("hypercall {code:?} internal operation {operation} failed: {detail}")]
    Internal {
        /// The hypercall being executed.
        code: HyperCallCode,
        /// The internal operation that failed.
        operation: &'static str,
        /// Diagnostic detail from the implementation.
        detail: String,
    },
}
