//! Architecture-neutral vCPU contexts and normalized runtime actions.

use axvm_types::{AccessWidth, GuestPhysAddr};

use crate::StopReason;

/// Scheduler effects selected after an architecture-local vCPU exit.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct VcpuRunAction {
    pub(crate) waits_for_event: bool,
    pub(crate) stop_reason: Option<StopReason>,
    pub(crate) resets_vm: bool,
    pub(crate) exits_vcpu: bool,
}

/// Outcome of one architecture-neutral vCPU run attempt.
pub(crate) enum VcpuRunOutcome {
    /// Hardware guest entry completed and produced a scheduler action.
    Entered(VcpuRunAction),
    /// A concurrent request canceled entry after architecture state was
    /// loaded. The outer task loop must observe device and lifecycle state
    /// before trying again.
    EntryCanceled,
}

/// Result of handling one exit while the vCPU is still bound to the host CPU.
#[derive(Debug)]
pub(crate) enum BoundVcpuExit<D> {
    /// A request canceled hardware entry after architecture state was loaded.
    EntryCanceled,
    /// The exit was handled completely; re-enter the guest in the current run slice.
    Continue,
    /// The run slice is complete and can return this scheduler action after unbind.
    Complete(VcpuRunAction),
    /// Finish architecture-local work after unbinding the vCPU.
    Defer(D),
    /// Finish a potentially blocking hypercall after unbinding the vCPU.
    DeferHypercall(crate::runtime::hvc::DeferredHyperCall),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MmioReadExit {
    pub(crate) addr: GuestPhysAddr,
    pub(crate) width: AccessWidth,
    pub(crate) reg: usize,
    pub(crate) reg_width: AccessWidth,
    pub(crate) signed_ext: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MmioWriteExit {
    pub(crate) addr: GuestPhysAddr,
    pub(crate) width: AccessWidth,
    pub(crate) data: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct HypercallExit {
    pub(crate) nr: u64,
    pub(crate) args: [u64; 6],
}
