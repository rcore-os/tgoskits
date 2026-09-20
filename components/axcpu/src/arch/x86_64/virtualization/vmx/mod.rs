//! Intel VMX control formats and machine operations.

mod fields;
mod vmcs;

pub use fields::*;
pub use vmcs::Vmcs;

mod context;
pub use context::{SyscallRegisters, VmxEntryContext, VmxEntryFailure};

mod controls;
pub use controls::{VmxControlMemory, VmxControls};

mod exit;
pub use exit::{
    ApicAccessExitInfo, ApicAccessExitType, CrAccessInfo, VmxExitInfo, VmxExitReason,
    VmxInstructionError, VmxInterruptInfo, VmxInterruptionType, VmxIoExitInfo,
};

mod host;

mod instructions;
pub use instructions::{EptInvalidation, invalidate_ept};

mod paging;
pub(crate) use paging::EptCapabilities;
pub use paging::EptPointer;

mod control_flags;
pub use control_flags::{VmxControl, VmxControlCapabilities, VmxControlError};
