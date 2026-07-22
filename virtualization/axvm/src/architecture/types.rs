//! Architecture-neutral vCPU contexts and normalized runtime actions.

use crate::StopReason;

/// Scheduler effects selected after an architecture-local vCPU exit.
#[derive(Debug)]
pub(crate) struct VcpuRunAction {
    scheduling: VcpuScheduling,
    stop_reason: Option<StopReason>,
}

/// Host scheduler transition requested after a vCPU has been unbound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VcpuScheduling {
    /// Re-enter the vCPU on the next runtime-loop iteration.
    Resume,
    /// Block until an event wakes the vCPU task.
    WaitForEvent,
    /// Rotate the current host run queue before resuming the vCPU.
    Yield,
}

impl VcpuScheduling {
    /// Scheduler disposition used after an operation wakes another vCPU.
    ///
    /// Keeping this as a named value lets the architecture-neutral runtime
    /// recognize the disposition even on targets that never request it.
    pub(crate) const YIELD: Self = Self::Yield;
}

impl VcpuRunAction {
    pub(crate) const fn new(scheduling: VcpuScheduling, stop_reason: Option<StopReason>) -> Self {
        Self {
            scheduling,
            stop_reason,
        }
    }

    pub(crate) const fn resume() -> Self {
        Self::new(VcpuScheduling::Resume, None)
    }

    pub(crate) const fn wait_for_event() -> Self {
        Self::new(VcpuScheduling::WaitForEvent, None)
    }

    pub(crate) const fn scheduling(&self) -> VcpuScheduling {
        self.scheduling
    }

    pub(crate) fn into_stop_reason(self) -> Option<StopReason> {
        self.stop_reason
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
pub(crate) struct HypercallExit {
    pub(crate) nr: u64,
    pub(crate) args: [u64; 6],
}

#[cfg(test)]
mod tests {
    use super::{VcpuRunAction, VcpuScheduling};

    #[test]
    fn scheduler_actions_keep_wait_and_yield_distinct() {
        assert_eq!(
            VcpuRunAction::wait_for_event().scheduling(),
            VcpuScheduling::WaitForEvent
        );
        assert_eq!(
            VcpuRunAction::new(VcpuScheduling::YIELD, None).scheduling(),
            VcpuScheduling::YIELD
        );
    }
}
