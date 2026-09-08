//! Architecture-independent vCPU wake and guest-exit requests.
//!
//! Architecture interrupt controllers remain the sole owners of pending
//! interrupt state. This module supplies a generation-bound thread wake,
//! optional sticky entry requests, and guest-mode ownership for conditional
//! remote doorbells.

use std::sync::Arc;

use crate::vcpu::VcpuRunState;
#[cfg(target_arch = "x86_64")]
use crate::{host::task::WakeResult, vcpu::HardIrqExitClaim};

/// Result of publishing one vCPU kick from hard-IRQ context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(target_arch = "x86_64")]
pub(crate) enum HardIrqKick {
    /// The task wake or the local host IRQ is sufficient.
    Complete,
    /// Task-context code must publish logical unblock state, refresh the
    /// runtime target, and send any remote guest-exit doorbell.
    Defer,
}

/// Pre-bound capability for one generation of a vCPU runtime task.
///
/// The handle contains no VM or runtime registry reference, so
/// [`Self::kick_from_hard_irq`] is bounded and safe to invoke after an
/// architecture backend has published its authoritative pending state.
#[derive(Clone)]
pub(crate) struct VcpuKickHandle {
    run_state: Arc<VcpuRunState>,
    wake: crate::host::task::ThreadWakeHandle,
}

impl VcpuKickHandle {
    pub(crate) fn new(
        run_state: Arc<VcpuRunState>,
        wake: crate::host::task::ThreadWakeHandle,
    ) -> Self {
        Self { run_state, wake }
    }

    /// Publishes a kick without performing a potentially blocking host IPI.
    ///
    /// A local hard IRQ has already forced the running guest to the host. A
    /// remote running guest, outside-guest waiter, or stale task-generation
    /// wake handle is deferred to the VM-owned kick worker.
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn kick_from_hard_irq(&self, current_cpu: usize) -> HardIrqKick {
        let wake = self.wake.wake();
        if matches!(wake, WakeResult::Exited | WakeResult::Unavailable) {
            return HardIrqKick::Defer;
        }
        match self.run_state.claim_hard_irq_exit(current_cpu) {
            HardIrqExitClaim::OutsideGuest | HardIrqExitClaim::RemoteGuest => HardIrqKick::Defer,
            HardIrqExitClaim::LocalGuest | HardIrqExitClaim::AlreadyClaimed => {
                HardIrqKick::Complete
            }
        }
    }

    /// Kicks the vCPU from task context and returns a remote CPU doorbell.
    ///
    /// The caller must send the returned IPI only after releasing runtime
    /// registry locks. A stale CPU is harmless: migration requires leaving
    /// guest mode. Controller-owned pending state must be published before
    /// this call; the kick itself does not manufacture a generic request.
    pub(crate) fn kick_from_task(&self, current_cpu: usize) -> Option<usize> {
        let _ = self.wake.wake();
        self.run_state.request_exit(current_cpu)
    }

    /// Publishes a sticky request for work that has no backend pending state.
    pub(crate) fn publish_entry_request(&self) {
        self.run_state.publish_exit_request();
    }
}
