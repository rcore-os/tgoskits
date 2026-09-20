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
        self.run_state.request_unblock();
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

    /// Publishes an unblock request, wakes only this vCPU, and requests a
    /// remote guest exit when required, matching KVM's task-context kick.
    ///
    /// Call only after releasing runtime registry and interrupt queue locks.
    /// Controller pending state must already be published. The unblock request
    /// survives a kick before wait registration; ax-task's sticky park protocol
    /// closes the predicate-to-block window. A stale CPU is harmless because
    /// migration also requires leaving guest mode.
    pub(crate) fn kick_from_task(&self) {
        self.run_state.request_unblock();
        let _ = self.wake.wake();
        if let Some(cpu_id) = self
            .run_state
            .request_exit(crate::host::task::current_cpu_id())
        {
            crate::host::task::send_ipi(cpu_id);
        }
    }

    /// Publishes a sticky request for work that has no backend pending state.
    pub(crate) fn publish_entry_request(&self) {
        self.run_state.publish_exit_request();
    }
}
