//! Architecture-neutral vCPU contexts and normalized runtime actions.

use axvm_types::{AccessWidth, GuestPhysAddr};

use crate::StopReason;

/// Scheduler effects selected after an architecture-local vCPU exit.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct VcpuRunAction {
    pub(crate) event_wait: VcpuEventWait,
    pub(crate) stop_reason: Option<StopReason>,
    pub(crate) resets_vm: bool,
    pub(crate) exits_vcpu: bool,
}

impl VcpuRunAction {
    /// Keeps a powered-down vCPU asleep until a lifecycle event resumes it.
    #[cfg(target_arch = "aarch64")]
    pub(crate) const fn cpu_down() -> Self {
        Self {
            event_wait: VcpuEventWait::Block,
            stop_reason: None,
            resets_vm: false,
            exits_vcpu: false,
        }
    }
}

/// How the runtime resumes a vCPU that yielded for an event.
///
/// `Poll` is reserved for ordinary guest-idle exits. A vCPU that was powered
/// down must stay on the shared wait path so that it cannot repeatedly enter
/// the guest before a lifecycle wake, such as PSCI `CPU_ON`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VcpuEventWait {
    /// Continue the run loop without an event wait.
    None,
    /// Block on the runtime wait queue until a lifecycle or device event arrives.
    Block,
    /// Poll timer and virtual-device state between guest entries.
    Poll,
}

impl VcpuEventWait {
    /// Returns whether this action uses the runtime's shared event wait queue.
    #[cfg(any(not(feature = "rt-poll-idle"), not(target_arch = "aarch64"), test))]
    pub(crate) const fn uses_shared_wait(self) -> bool {
        match self {
            Self::None => false,
            Self::Block => true,
            Self::Poll => !cfg!(feature = "rt-poll-idle"),
        }
    }
}

/// Result of handling one exit while the vCPU is still bound to the host CPU.
#[derive(Debug)]
pub(crate) enum BoundVcpuExit<D> {
    /// The exit was handled completely; re-enter the guest in the current run slice.
    Continue,
    /// The run slice is complete and can return this scheduler action after unbind.
    Complete(VcpuRunAction),
    /// Finish architecture-local work after unbinding the vCPU.
    Defer(D),
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
