//! Architecture-independent vCPU wake and guest-exit requests.
//!
//! Architecture interrupt controllers remain the sole owners of canonical IRQ
//! state. This module only publishes the fixed execution signal, wakes the
//! pre-bound thread, and sends the remote guest-exit doorbell selected by that
//! signal.

use super::queue::VcpuWakeTarget;

/// Wakes one pre-bound vCPU target from ordinary task context.
///
/// Canonical controller or queue state must already be published. This helper
/// never owns a queue lock: the caller takes a target snapshot first, then all
/// fence, wake, and IPI operations happen outside that raw guard.
pub(crate) fn kick_target(target: &VcpuWakeTarget) {
    let signals = target.signals();
    signals.request_unblock();
    signals.publish_exit_request();
    if let Some(cpu_id) = signals.request_exit(crate::host::task::current_cpu_id()) {
        target.wake();
        crate::host::task::send_ipi(cpu_id);
    } else {
        target.wake();
    }
}
