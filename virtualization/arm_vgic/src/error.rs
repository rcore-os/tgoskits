//! Typed GICv3 failures.

use alloc::string::String;

use axdevice_base::DeviceError;
use axvm_types::AccessWidth;

use crate::IntId;

/// Result returned by GICv3 domain operations.
pub type VgicResult<T = ()> = Result<T, VgicError>;

/// Static `Display` reason for one register width requirement.
///
/// Register validation runs on the raw-lock path, so a width mismatch reports
/// a static reason instead of formatting the expected width.
pub(crate) const fn width_requirement(expected: AccessWidth) -> &'static str {
    match expected {
        AccessWidth::Byte => "register requires an 8-bit access",
        AccessWidth::Word => "register requires a 16-bit access",
        AccessWidth::Dword => "register requires a 32-bit access",
        AccessWidth::Qword => "register requires a 64-bit access",
    }
}

/// GICv3 register block involved in an access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisterRegion {
    /// Distributor register frame.
    Distributor,
    /// GICv2 memory-mapped CPU-interface frame.
    CpuInterface,
    /// One Redistributor register frame.
    Redistributor,
    /// Interrupt Translation Service register frame.
    Its,
}

/// Failure category preserved across native-state and task-service boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateErrorKind {
    /// Another owner or an in-flight operation retains the resource.
    ResourceBusy,
    /// The required binding or CPU interface does not exist.
    NotFound,
    /// The requested operation is unavailable for this backing or mode.
    Unsupported,
    /// The operation violates the current delivery state.
    InvalidState,
    /// An index or required operand is invalid.
    InvalidInput,
}

/// Additional allocation-free facts about a native-state failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeStateDetail {
    /// The operation, vCPU, and interrupt identify the failure completely.
    None,
    /// List-register index involved in a failure.
    ListRegister(usize),
    /// Canonical and observed list-register backing disagree.
    BackingMismatch {
        /// Backing retained by canonical state.
        owned: crate::ListRegisterBacking,
        /// Backing observed during hardware save.
        observed: crate::ListRegisterBacking,
    },
    /// A binding names a different guest interrupt.
    InterruptMismatch {
        /// Interrupt being delivered.
        requested: IntId,
        /// Interrupt owned by the binding.
        owned: IntId,
    },
    /// A hardware delivery names a different host source.
    HostMismatch {
        /// Source retained by the binding.
        owned: crate::PhysicalIrqId,
        /// Source observed in the hardware record.
        observed: crate::PhysicalIrqId,
    },
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
    #[error("invalid {region:?} {operation} at offset {offset:#x} with width {width:?}: {reason}")]
    InvalidAccess {
        /// Register block being accessed.
        region: RegisterRegion,
        /// Whether the access is a read or write.
        operation: &'static str,
        /// Offset from the frame base.
        offset: u64,
        /// Requested access width.
        width: AccessWidth,
        /// Static rejection reason.
        reason: &'static str,
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
    /// A hard-IRQ delivery exhausted its preallocated queue capacity.
    #[error("vCPU {vcpu} has no preallocated delivery slot for {intid:?}")]
    DeliveryQueueFull {
        /// Target virtual CPU.
        vcpu: usize,
        /// Interrupt that could not be queued without allocating.
        intid: IntId,
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
    #[error("GICv3 backend operation {operation} failed: {source}")]
    Backend {
        /// Backend operation.
        operation: &'static str,
        /// Allocation-free backend failure.
        source: crate::GicV3BackendError,
    },
    /// A task-context host service failed (never produced in a native scope).
    #[error("GIC host service {operation} failed: {detail}")]
    HostService {
        /// Task-side host operation.
        operation: &'static str,
        /// Diagnostic formatted only after native contexts have retired.
        detail: String,
    },
    /// A native (raw-lock) GICv3 state operation failed without allocating.
    ///
    /// This variant carries only `Copy` facts, so it can be produced while the
    /// canonical raw lock is held, including on a CPU-pinned path.
    #[error(
        "native GICv3 {kind:?} during {operation} for vCPU {vcpu:?} (intid {intid:?}): {reason} \
         ({detail:?})"
    )]
    NativeState {
        /// Operation that failed.
        operation: &'static str,
        /// vCPU whose state was involved.
        vcpu: Option<usize>,
        /// Interrupt involved, when the failure names one.
        intid: Option<IntId>,
        /// Static rejection reason.
        reason: &'static str,
        /// Semantic error category, independent of diagnostic wording.
        kind: StateErrorKind,
        /// Additional source and ownership facts, when needed.
        detail: NativeStateDetail,
    },
}

impl From<VgicError> for DeviceError {
    fn from(error: VgicError) -> Self {
        let detail = alloc::format!("{error}");
        match error {
            VgicError::NativeState {
                operation, kind, ..
            } => match kind {
                StateErrorKind::ResourceBusy => Self::ResourceBusy {
                    operation,
                    resource: detail,
                },
                StateErrorKind::Unsupported => Self::Unsupported { operation, detail },
                StateErrorKind::NotFound | StateErrorKind::InvalidState => {
                    Self::InvalidState { operation, detail }
                }
                StateErrorKind::InvalidInput => Self::InvalidInput { operation, detail },
            },
            VgicError::Unsupported { operation, .. } => Self::Unsupported { operation, detail },
            VgicError::Backend { operation, .. } | VgicError::HostService { operation, .. } => {
                Self::Backend { operation, detail }
            }
            VgicError::ResourceConflict { resource, .. } => Self::ResourceBusy {
                operation: "access ARM VGIC",
                resource: alloc::format!("{resource}: {detail}"),
            },
            VgicError::ResourceNotFound { operation, .. } => {
                Self::InvalidState { operation, detail }
            }
            VgicError::GuestMemory { operation, .. } => Self::Backend { operation, detail },
            _ => Self::InvalidInput {
                operation: "access ARM VGIC",
                detail,
            },
        }
    }
}
