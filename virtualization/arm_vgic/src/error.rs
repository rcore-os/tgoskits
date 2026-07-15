//! Typed GICv3 failures.

use alloc::string::String;

use axvm_types::AccessWidth;

use crate::IntId;

/// Result returned by GICv3 domain operations.
pub type VgicResult<T = ()> = Result<T, VgicError>;

/// GICv3 register block involved in an access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisterRegion {
    /// Distributor register frame.
    Distributor,
    /// One Redistributor register frame.
    Redistributor,
    /// Interrupt Translation Service register frame.
    Its,
}

/// Errors reported by the virtual GICv3 model.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VgicError {
    /// A raw INTID is reserved or outside the configured range.
    #[error("INTID {raw} is not valid for this GICv3 instance")]
    InvalidIntId {
        /// Rejected architectural INTID.
        raw: u32,
    },
    /// A typed INTID has the wrong class for an operation.
    #[error("INTID {intid:?} cannot be used for {operation}")]
    WrongIntIdClass {
        /// Rejected typed INTID.
        intid: IntId,
        /// Operation requiring another class.
        operation: &'static str,
    },
    /// Controller configuration violates a GICv3 invariant.
    #[error("invalid GICv3 configuration: {detail}")]
    InvalidConfig {
        /// Configuration invariant that was violated.
        detail: String,
    },
    /// A register access has an invalid address, alignment, or width.
    #[error("invalid {region:?} {operation} at offset {offset:#x} with width {width:?}: {detail}")]
    InvalidAccess {
        /// Register block being accessed.
        region: RegisterRegion,
        /// Whether the access is a read or write.
        operation: &'static str,
        /// Offset from the frame base.
        offset: u64,
        /// Requested access width.
        width: AccessWidth,
        /// Rejected alignment or register constraint.
        detail: String,
    },
    /// A requested interrupt state transition is not architecturally valid.
    #[error("invalid state transition for {intid:?} during {operation}: {detail}")]
    InvalidStateTransition {
        /// Interrupt being changed.
        intid: IntId,
        /// Transition operation.
        operation: &'static str,
        /// Current-state diagnostic.
        detail: String,
    },
    /// A VM-local resource is already owned.
    #[error("GICv3 resource conflict for {resource}: {detail}")]
    ResourceConflict {
        /// Conflicting resource kind.
        resource: &'static str,
        /// Ownership diagnostic.
        detail: String,
    },
    /// A required mapping or vCPU association does not exist.
    #[error("GICv3 resource {resource} was not found during {operation}")]
    ResourceNotFound {
        /// Missing resource diagnostic.
        resource: String,
        /// Operation requiring it.
        operation: &'static str,
    },
    /// Guest-memory access for an ITS queue failed.
    #[error("ITS guest-memory {operation} at {address:#x} for {length} bytes failed: {detail}")]
    GuestMemory {
        /// Memory operation.
        operation: &'static str,
        /// Guest physical address.
        address: u64,
        /// Byte count.
        length: usize,
        /// Memory-capability diagnostic.
        detail: String,
    },
    /// An ITS command is malformed or invalid for current mappings.
    #[error("invalid ITS command {opcode:#x} at queue offset {offset:#x}: {detail}")]
    InvalidItsCommand {
        /// Command opcode.
        opcode: u8,
        /// Command-queue offset.
        offset: u64,
        /// Command diagnostic.
        detail: String,
    },
    /// One command submission exceeds the configured work budget.
    #[error("ITS command submission exceeds budget {budget} at queue offset {offset:#x}")]
    ItsCommandBudgetExceeded {
        /// Maximum commands processed per submission.
        budget: usize,
        /// First unprocessed queue offset.
        offset: u64,
    },
    /// A selected mode or backend lacks a requested capability.
    #[error("unsupported GICv3 operation {operation}: {detail}")]
    Unsupported {
        /// Unsupported operation.
        operation: &'static str,
        /// Capability diagnostic.
        detail: String,
    },
    /// A checked physical GIC/ITS or CPU-interface backend failed.
    #[error("GICv3 backend operation {operation} failed: {detail}")]
    Backend {
        /// Backend operation.
        operation: &'static str,
        /// Backend diagnostic.
        detail: String,
    },
}
