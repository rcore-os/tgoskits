use alloc::boxed::Box;
use core::fmt;

use crate::{IrqAffinity, IrqError, IrqExecution, IrqId, IrqScope, ShareMode, action::Action};

/// Move-only ownership of an IRQ action removed from a registry descriptor.
///
/// A detached action owns its handler and immutable registration policy. It is
/// disabled, has no invocation in flight, and does not occupy an IRQ
/// descriptor. Dropping this value destroys the handler without interacting
/// with an IRQ registry. Destruction may release heap-backed handler state and
/// must therefore remain in task context.
///
/// This token owns the framework action, not the device, controller route, or
/// a hardware-pending interrupt. Before an exclusive ownership transfer, the
/// caller must mask the device source, disable and synchronize the action, and
/// drain or acknowledge controller state according to the irqchip contract.
/// Fresh action IDs reject stale framework handles after reattachment; they do
/// not relabel an interrupt that was already pending in hardware.
#[must_use = "dropping a detached IRQ action destroys its handler"]
pub struct DetachedIrqAction {
    config: DetachedActionConfig,
    action: Box<Action>,
}

impl DetachedIrqAction {
    /// Returns the IRQ formerly owned by this action.
    pub const fn irq(&self) -> IrqId {
        self.config.irq
    }

    pub(crate) fn new(config: DetachedActionConfig, action: Box<Action>) -> Self {
        Self { config, action }
    }

    pub(crate) const fn config(&self) -> DetachedActionConfig {
        self.config
    }

    pub(crate) fn into_registered_raw(mut self, id: u64) -> *mut Action {
        self.action.prepare_for_reattach(id);
        Box::into_raw(self.action)
    }
}

impl fmt::Debug for DetachedIrqAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DetachedIrqAction")
            .field("irq", &self.config.irq)
            .field("scope", &self.config.scope)
            .field("affinity", &self.config.affinity)
            .field("execution", &self.config.execution)
            .field("share_mode", &self.config.share_mode)
            .finish_non_exhaustive()
    }
}

/// Failure to register a detached action while retaining unique ownership.
#[derive(Debug)]
#[must_use = "the contained detached action must be retried or explicitly dropped"]
pub struct ReattachIrqActionError {
    reason: IrqError,
    action: DetachedIrqAction,
}

impl ReattachIrqActionError {
    pub(crate) const fn new(reason: IrqError, action: DetachedIrqAction) -> Self {
        Self { reason, action }
    }

    /// Returns the registration failure without consuming the owned action.
    pub const fn reason(&self) -> IrqError {
        self.reason
    }

    /// Recovers the detached action for retry or explicit destruction.
    pub fn into_action(self) -> DetachedIrqAction {
        self.action
    }

    /// Splits the failure into its reason and retained action ownership.
    pub fn into_parts(self) -> (IrqError, DetachedIrqAction) {
        (self.reason, self.action)
    }
}

impl fmt::Display for ReattachIrqActionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to reattach IRQ action {:?}: {:?}",
            self.action.irq(),
            self.reason
        )
    }
}

impl core::error::Error for ReattachIrqActionError {}

#[derive(Clone, Copy)]
pub(crate) struct DetachedActionConfig {
    pub(crate) irq: IrqId,
    pub(crate) scope: IrqScope,
    pub(crate) affinity: IrqAffinity,
    pub(crate) execution: IrqExecution,
    pub(crate) share_mode: ShareMode,
}
