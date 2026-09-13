use alloc::boxed::Box;
use core::fmt;

use crate::VsockError;

/// Result of a bounded hard-IRQ callback for one vsock device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VsockHardIrqResult {
    /// The device did not publish an interrupt status.
    Spurious,
    /// The interrupt was acknowledged by an already-running poll cycle.
    Handled,
    /// Task-context event draining must run.
    Schedule,
    /// The transport was owned by task context; the worker must ACK and drain.
    ProbeDeferred,
}

/// Hard-IRQ capability separated from the task-context vsock interface.
pub trait VsockHardIrqHandler: Send {
    /// Acknowledge the device and report whether the fixed worker must run.
    ///
    /// Implementations must not block, allocate, parse packets, or wake socket
    /// waiters directly.
    fn handle_irq(&mut self) -> VsockHardIrqResult;
}

/// Move-only hard-IRQ endpoint for one vsock device.
pub struct VsockHardIrqEndpoint {
    handler: Box<dyn VsockHardIrqHandler>,
}

impl VsockHardIrqEndpoint {
    /// Creates an endpoint from its driver-owned hard-IRQ capability.
    pub fn new(handler: Box<dyn VsockHardIrqHandler>) -> Self {
        Self { handler }
    }

    /// Runs one bounded hard-IRQ callback.
    pub fn handle_irq(&mut self) -> VsockHardIrqResult {
        self.handler.handle_irq()
    }
}

impl fmt::Debug for VsockHardIrqEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VsockHardIrqEndpoint")
            .finish_non_exhaustive()
    }
}

/// Result of the task-context IRQ completion window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VsockRearmResult {
    /// The transport has no event pending after the race-closing recheck.
    Idle,
    /// An interrupt or deferred ACK arrived while the worker was draining.
    WorkPending,
}

/// Task-context IRQ control paired with [`VsockHardIrqEndpoint`].
pub trait VsockPollIrqControl: Send {
    /// Starts a logically quiesced drain cycle and consumes the triggering ACK.
    fn quiesce(&mut self) -> Result<(), VsockError>;

    /// Completes a drain cycle and closes the IRQ-versus-sleep race.
    fn rearm_and_check(&mut self) -> Result<VsockRearmResult, VsockError>;

    /// Prevents future device IRQ work before transport ownership is released.
    fn shutdown(&mut self) -> Result<(), VsockError>;
}

/// Driver-owned IRQ capabilities transferred together exactly once.
pub struct VsockIrqEndpoints {
    hard_irq: VsockHardIrqEndpoint,
    control: Box<dyn VsockPollIrqControl>,
}

impl VsockIrqEndpoints {
    /// Creates the paired hard-IRQ and task-context control capabilities.
    pub fn new(hard_irq: VsockHardIrqEndpoint, control: Box<dyn VsockPollIrqControl>) -> Self {
        Self { hard_irq, control }
    }

    /// Transfers the paired capabilities to the IRQ runtime.
    pub fn into_parts(self) -> (VsockHardIrqEndpoint, Box<dyn VsockPollIrqControl>) {
        (self.hard_irq, self.control)
    }
}

impl fmt::Debug for VsockIrqEndpoints {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VsockIrqEndpoints").finish_non_exhaustive()
    }
}
