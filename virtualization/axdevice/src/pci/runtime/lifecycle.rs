use alloc::{sync::Arc, vec::Vec};

use ax_sync::SpinLock;
use axdevice_base::DeviceId;

use super::{
    super::PciRootState,
    ORPHANED_IRQ_WITHDRAWALS,
    endpoint::{EndpointIrqTransitionPermit, PciFunction},
    routing::{EndpointAdmission, EndpointRouter},
};
use crate::{DeviceManagerError, DeviceManagerResult, DeviceNodeId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BindingLifecycleState {
    Running,
    Binding,
    Resetting,
    ResetFailed,
    Withdrawing,
    Stopping,
    Dead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WithdrawalStartError {
    Busy,
    Terminal,
}

#[derive(Clone, Copy)]
enum LifecycleCompletion {
    Restore(BindingLifecycleState),
    Reset,
    Stop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleOwner {
    Binding,
    Reset,
    Withdrawal,
    Stop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HandoffPhase {
    Open,
    Draining,
    Sealed,
    Publishing,
}

pub(super) struct LifecycleSlot {
    pub(super) state: BindingLifecycleState,
    owner: Option<LifecycleOwner>,
    phase: HandoffPhase,
    // Teardown intent remains latched until the stop successor reaches Dead.
    stop_requested: bool,
    pub(super) pending_withdrawals: Vec<DeviceId>,
}

impl LifecycleSlot {
    const fn new() -> Self {
        Self {
            state: BindingLifecycleState::Running,
            owner: None,
            phase: HandoffPhase::Open,
            stop_requested: false,
            pending_withdrawals: Vec::new(),
        }
    }
}

/// Owns one logical lifecycle operation without holding the state lock.
pub(super) struct LifecycleOperation<'a> {
    binding: &'a PciRootBinding,
    completion: LifecycleCompletion,
    owner: LifecycleOwner,
    claimed: bool,
    completed: bool,
}

impl LifecycleOperation<'_> {
    pub(super) fn finish_restore(mut self) -> DeviceManagerResult {
        let LifecycleCompletion::Restore(state) = self.completion else {
            return Err(DeviceManagerError::InvalidState {
                operation: "complete PCI lifecycle operation",
                detail: "restore completion was requested for a different operation".into(),
            });
        };
        self.complete(state, false, false)
    }

    pub(super) fn finish_reset(mut self) -> DeviceManagerResult {
        self.complete(BindingLifecycleState::Running, true, true)
    }

    pub(super) fn finish_reset_failure(mut self) -> DeviceManagerResult {
        if let Err(error) = self.binding.router.close_admissions_and_drain() {
            warn!("PCI reset failure cleanup could not drain routed endpoint activity: {error}");
        }
        self.complete(BindingLifecycleState::ResetFailed, false, false)
    }

    pub(super) fn finish_stop(mut self) -> DeviceManagerResult {
        self.wait_for_claim()?;
        self.complete(BindingLifecycleState::Dead, false, false)
    }

    pub(super) fn wait_for_claim(&mut self) -> DeviceManagerResult {
        if self.claimed {
            return Ok(());
        }

        debug_assert_eq!(self.owner, LifecycleOwner::Stop);
        loop {
            let mut slot = self.binding.lifecycle.lock_irqsave();
            if slot.state == BindingLifecycleState::Dead {
                return Err(DeviceManagerError::InvalidState {
                    operation: "claim PCI root teardown lifecycle owner",
                    detail: "PCI root binding is already dead".into(),
                });
            }
            if slot.owner.is_none() && slot.phase == HandoffPhase::Open && slot.stop_requested {
                slot.state = BindingLifecycleState::Stopping;
                slot.owner = Some(LifecycleOwner::Stop);
                slot.phase = HandoffPhase::Draining;
                self.claimed = true;
                return Ok(());
            }
            drop(slot);
            core::hint::spin_loop();
        }
    }

    fn complete(
        &mut self,
        success_state: BindingLifecycleState,
        reopen_admissions: bool,
        notify_reset_handoff: bool,
    ) -> DeviceManagerResult {
        let mut first_error = None;
        let mut sealed = false;
        let mut completion_hook_called = false;
        let mut admission_published = false;
        let mut stop_superseded = false;

        self.wait_for_claim()?;

        loop {
            if !sealed {
                if let Err(error) = self.binding.drain_pending_binding_withdrawals()
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }

                if notify_reset_handoff && matches!(self.completion, LifecycleCompletion::Reset) {
                    self.binding.notify_reset_handoff();
                }

                let ready_to_seal = {
                    let mut slot = self.binding.lifecycle.lock_irqsave();
                    debug_assert_eq!(slot.owner, Some(self.owner));
                    if slot.pending_withdrawals.is_empty() {
                        if reopen_admissions && first_error.is_none() && !slot.stop_requested {
                            // Publish Running while retaining the owner. The
                            // sealed phase prevents a second owner from
                            // entering during admission publication.
                            slot.state = success_state;
                            slot.phase = HandoffPhase::Sealed;
                            true
                        } else {
                            if reopen_admissions && first_error.is_none() && slot.stop_requested {
                                stop_superseded = true;
                            }
                            slot.state = if slot.stop_requested {
                                BindingLifecycleState::Stopping
                            } else if matches!(self.completion, LifecycleCompletion::Reset) {
                                BindingLifecycleState::ResetFailed
                            } else {
                                success_state
                            };
                            slot.phase = HandoffPhase::Sealed;
                            true
                        }
                    } else {
                        slot.phase = HandoffPhase::Draining;
                        false
                    }
                };
                if !ready_to_seal {
                    continue;
                }
                sealed = true;
            }

            if !completion_hook_called {
                self.binding.notify_completion_closing();
                completion_hook_called = true;
            }

            if reopen_admissions
                && !admission_published
                && first_error.is_none()
                && !stop_superseded
            {
                // The completion hook may have caused a lease drop. Drain it
                // before publishing admission so the final pending check and
                // publication are one sealed handoff.
                if let Err(error) = self.binding.drain_pending_binding_withdrawals() {
                    first_error = Some(error);
                    self.binding
                        .router
                        .close_admissions_and_drain()
                        .unwrap_or_else(|close_error| {
                            warn!(
                                "PCI reset failure cleanup could not drain routed endpoint \
                                 activity: {close_error}"
                            );
                        });
                    let mut slot = self.binding.lifecycle.lock_irqsave();
                    debug_assert_eq!(slot.owner, Some(self.owner));
                    slot.state = BindingLifecycleState::ResetFailed;
                    slot.phase = HandoffPhase::Sealed;
                    continue;
                }
                let publish = {
                    let mut slot = self.binding.lifecycle.lock_irqsave();
                    debug_assert_eq!(slot.owner, Some(self.owner));
                    if !slot.pending_withdrawals.is_empty() {
                        false
                    } else if slot.stop_requested {
                        stop_superseded = true;
                        slot.state = BindingLifecycleState::Stopping;
                        slot.phase = HandoffPhase::Sealed;
                        false
                    } else {
                        // Reserve publication while retaining the owner. A
                        // stop request arriving after this point is a
                        // successor teardown and cannot cancel the reset.
                        slot.phase = HandoffPhase::Publishing;
                        true
                    }
                };
                if !publish {
                    continue;
                }
                self.binding.notify_admission_open();
                self.binding.router.open_admissions();
                admission_published = true;
                continue;
            }

            if reopen_admissions && !admission_published && first_error.is_some() {
                // A reset failure before publication is fail-closed.  The
                // owner remains sealed while the final deferred queue drain
                // runs, so no fresh operation can observe a half-reset root.
                if let Err(error) = self.binding.router.close_admissions_and_drain() {
                    warn!(
                        "PCI reset failure cleanup could not drain routed endpoint activity: \
                         {error}"
                    );
                }
                let mut slot = self.binding.lifecycle.lock_irqsave();
                debug_assert_eq!(slot.owner, Some(self.owner));
                slot.state = if slot.stop_requested {
                    stop_superseded = true;
                    BindingLifecycleState::Stopping
                } else {
                    BindingLifecycleState::ResetFailed
                };
                slot.phase = HandoffPhase::Sealed;
            }

            if let Err(error) = self.binding.drain_pending_binding_withdrawals() {
                if admission_published {
                    // Late cleanup belongs to the successor owner and cannot
                    // roll back a reset already published as Running.
                    warn!("PCI lifecycle successor withdrawal remains pending: {error}");
                } else if first_error.is_none() {
                    first_error = Some(error);
                }
            }

            let committed = {
                let mut slot = self.binding.lifecycle.lock_irqsave();
                debug_assert_eq!(slot.owner, Some(self.owner));
                if slot.pending_withdrawals.is_empty() {
                    slot.phase = HandoffPhase::Open;
                    slot.owner = None;
                    if matches!(self.completion, LifecycleCompletion::Stop) {
                        slot.state = BindingLifecycleState::Dead;
                        slot.stop_requested = false;
                    } else if slot.stop_requested {
                        slot.state = BindingLifecycleState::Stopping;
                    }
                    true
                } else {
                    slot.phase = HandoffPhase::Draining;
                    false
                }
            };
            if !committed {
                continue;
            }

            self.completed = true;
            return if admission_published {
                Ok(())
            } else if stop_superseded && first_error.is_none() {
                Err(DeviceManagerError::InvalidState {
                    operation: "complete PCI reset lifecycle operation",
                    detail: "PCI root teardown superseded reset before admission publication"
                        .into(),
                })
            } else {
                first_error.map_or(Ok(()), Err)
            };
        }
    }
}

impl Drop for LifecycleOperation<'_> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }

        // Drop is only an abort guard. Normal paths use `complete`, while an
        // unwind must close reset admissions and release any deferred route
        // withdrawal owner without ever reopening partially-reset state.
        if matches!(self.completion, LifecycleCompletion::Reset) {
            let _ = self.binding.router.close_admissions_and_drain();
        }
        let fallback = match self.completion {
            LifecycleCompletion::Restore(state) => state,
            // A reset that unwinds before publishing its result must remain
            // fail-closed instead of reopening partially reset state.
            LifecycleCompletion::Reset => BindingLifecycleState::ResetFailed,
            LifecycleCompletion::Stop => BindingLifecycleState::Dead,
        };
        if let Err(error) = self.complete(fallback, false, false) {
            warn!("PCI lifecycle abort could not complete deferred handoff: {error}");
        }
    }
}

/// Host-owned root binding published as a typed bundle service.
pub struct PciRootBinding {
    pub(super) host: DeviceNodeId,
    pub(super) root: Arc<PciRootState>,
    pub(super) router: Arc<EndpointRouter>,
    pub(super) lifecycle: SpinLock<LifecycleSlot>,
    pub(super) pending_irq_withdrawals: SpinLock<Vec<PendingIrqWithdrawal>>,
    #[cfg(test)]
    reset_handoff_hook: SpinLock<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    deferred_withdrawal_hook: SpinLock<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    admission_open_hook: SpinLock<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    completion_closing_hook: SpinLock<Option<Arc<dyn Fn() + Send + Sync>>>,
}

pub(super) struct PendingIrqWithdrawal {
    pub(super) device: DeviceId,
    pub(super) function: Arc<dyn PciFunction>,
    pub(super) admission: Arc<EndpointAdmission>,
}

impl PciRootBinding {
    /// Creates a binding service for one resolved host root.
    pub fn new(host: DeviceNodeId, root: Arc<PciRootState>) -> Self {
        Self {
            host,
            root,
            router: Arc::new(EndpointRouter::new()),
            lifecycle: SpinLock::new(LifecycleSlot::new()),
            pending_irq_withdrawals: SpinLock::new(Vec::new()),
            #[cfg(test)]
            reset_handoff_hook: SpinLock::new(None),
            #[cfg(test)]
            deferred_withdrawal_hook: SpinLock::new(None),
            #[cfg(test)]
            admission_open_hook: SpinLock::new(None),
            #[cfg(test)]
            completion_closing_hook: SpinLock::new(None),
        }
    }

    #[cfg(test)]
    pub(super) fn set_reset_handoff_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.reset_handoff_hook.lock_irqsave() = Some(hook);
    }

    #[cfg(test)]
    pub(super) fn set_deferred_withdrawal_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.deferred_withdrawal_hook.lock_irqsave() = Some(hook);
    }

    #[cfg(test)]
    pub(super) fn set_admission_open_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.admission_open_hook.lock_irqsave() = Some(hook);
    }

    #[cfg(test)]
    pub(super) fn set_completion_closing_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.completion_closing_hook.lock_irqsave() = Some(hook);
    }

    #[cfg(test)]
    pub(super) fn lifecycle_owner_is_reset(&self) -> bool {
        self.lifecycle.lock_irqsave().owner == Some(LifecycleOwner::Reset)
    }

    #[cfg(test)]
    pub(super) fn stop_requested(&self) -> bool {
        self.lifecycle.lock_irqsave().stop_requested
    }

    #[cfg(test)]
    fn notify_reset_handoff(&self) {
        let hook = self.reset_handoff_hook.lock_irqsave().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(not(test))]
    fn notify_reset_handoff(&self) {}

    #[cfg(test)]
    fn notify_deferred_withdrawal(&self) {
        let hook = self.deferred_withdrawal_hook.lock_irqsave().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(not(test))]
    fn notify_deferred_withdrawal(&self) {}

    #[cfg(test)]
    fn notify_admission_open(&self) {
        let hook = self.admission_open_hook.lock_irqsave().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(not(test))]
    fn notify_admission_open(&self) {}

    #[cfg(test)]
    fn notify_completion_closing(&self) {
        let hook = self.completion_closing_hook.lock_irqsave().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(not(test))]
    fn notify_completion_closing(&self) {}

    /// Returns the host graph identity publishing this service.
    pub const fn host(&self) -> &DeviceNodeId {
        &self.host
    }

    /// Retries deferred endpoint binding and endpoint-owned IRQ withdrawals.
    ///
    /// A pending withdrawal keeps the endpoint owner and its closed admission
    /// alive. Rebinding the same device is rejected until this method drains
    /// the owner-side cleanup successfully.
    pub fn retry_irq_withdrawals(&self) -> DeviceManagerResult {
        let operation = self.begin_withdrawal_operation()?;
        let deferred_result = self.drain_pending_binding_withdrawals();
        let irq_result = retry_pending_irq_withdrawals(&self.pending_irq_withdrawals);
        let completion_result = operation.finish_restore();
        deferred_result.and(irq_result).and(completion_result)
    }

    /// Retries endpoint IRQ withdrawals orphaned by a previous root teardown.
    ///
    /// The orphan queue retains each endpoint owner and closed admission until
    /// this method succeeds. A failed retry leaves the entry fail-closed for a
    /// later owner or teardown supervisor to retry.
    pub fn retry_orphaned_irq_withdrawals() -> DeviceManagerResult {
        retry_pending_irq_withdrawals(&ORPHANED_IRQ_WITHDRAWALS)
    }

    pub(super) fn begin_binding_operation(&self) -> DeviceManagerResult<LifecycleOperation<'_>> {
        self.begin_owner(
            LifecycleOwner::Binding,
            BindingLifecycleState::Binding,
            "bind PCI endpoint route",
        )
    }

    pub(super) fn begin_reset_operation(&self) -> DeviceManagerResult<LifecycleOperation<'_>> {
        self.begin_owner(
            LifecycleOwner::Reset,
            BindingLifecycleState::Resetting,
            "reset PCI root binding",
        )
    }

    pub(super) fn begin_withdrawal_operation(&self) -> DeviceManagerResult<LifecycleOperation<'_>> {
        self.try_begin_withdrawal_operation().map_err(|error| {
            let detail = match error {
                WithdrawalStartError::Busy => {
                    "PCI root binding lifecycle operation is already in progress"
                }
                WithdrawalStartError::Terminal => "PCI root binding is stopping or dead",
            };
            DeviceManagerError::InvalidState {
                operation: "withdraw PCI endpoint IRQ",
                detail: detail.into(),
            }
        })
    }

    pub(super) fn try_begin_withdrawal_operation(
        &self,
    ) -> Result<LifecycleOperation<'_>, WithdrawalStartError> {
        let mut slot = self.lifecycle.lock_irqsave();
        let previous = slot.state;
        if slot.stop_requested
            || !matches!(
                previous,
                BindingLifecycleState::Running | BindingLifecycleState::ResetFailed
            )
            || slot.owner.is_some()
            || slot.phase != HandoffPhase::Open
        {
            return Err(match previous {
                BindingLifecycleState::Stopping | BindingLifecycleState::Dead => {
                    WithdrawalStartError::Terminal
                }
                _ if slot.stop_requested => WithdrawalStartError::Terminal,
                _ => WithdrawalStartError::Busy,
            });
        }
        slot.state = BindingLifecycleState::Withdrawing;
        slot.owner = Some(LifecycleOwner::Withdrawal);
        slot.phase = HandoffPhase::Draining;
        Ok(LifecycleOperation {
            binding: self,
            completion: LifecycleCompletion::Restore(previous),
            owner: LifecycleOwner::Withdrawal,
            claimed: true,
            completed: false,
        })
    }

    pub(super) fn try_begin_withdrawal_or_defer(
        &self,
        device: DeviceId,
    ) -> Option<LifecycleOperation<'_>> {
        let mut slot = self.lifecycle.lock_irqsave();
        let previous = slot.state;
        if previous == BindingLifecycleState::Dead {
            return None;
        }
        if slot.stop_requested
            || !matches!(
                previous,
                BindingLifecycleState::Running | BindingLifecycleState::ResetFailed
            )
            || slot.owner.is_some()
            || slot.phase != HandoffPhase::Open
        {
            // The lifecycle lock covers both observing the current owner and
            // enqueuing the deferred withdrawal.  A completing owner cannot
            // clear its slot between these two operations.
            slot.pending_withdrawals.push(device);
            drop(slot);
            self.notify_deferred_withdrawal();
            return None;
        }
        slot.state = BindingLifecycleState::Withdrawing;
        slot.owner = Some(LifecycleOwner::Withdrawal);
        slot.phase = HandoffPhase::Draining;
        Some(LifecycleOperation {
            binding: self,
            completion: LifecycleCompletion::Restore(previous),
            owner: LifecycleOwner::Withdrawal,
            claimed: true,
            completed: false,
        })
    }

    pub(super) fn begin_stop_operation(&self) -> LifecycleOperation<'_> {
        let mut slot = self.lifecycle.lock_irqsave();
        slot.stop_requested = true;
        let claimed = if slot.owner.is_none()
            && slot.phase == HandoffPhase::Open
            && slot.state != BindingLifecycleState::Dead
        {
            slot.state = BindingLifecycleState::Stopping;
            slot.owner = Some(LifecycleOwner::Stop);
            slot.phase = HandoffPhase::Draining;
            true
        } else {
            false
        };
        LifecycleOperation {
            binding: self,
            completion: LifecycleCompletion::Stop,
            owner: LifecycleOwner::Stop,
            claimed,
            completed: false,
        }
    }

    fn begin_owner(
        &self,
        owner: LifecycleOwner,
        next_state: BindingLifecycleState,
        operation: &'static str,
    ) -> DeviceManagerResult<LifecycleOperation<'_>> {
        let mut slot = self.lifecycle.lock_irqsave();
        if slot.stop_requested
            || slot.owner.is_some()
            || slot.phase != HandoffPhase::Open
            || !slot.pending_withdrawals.is_empty()
        {
            return Err(DeviceManagerError::InvalidState {
                operation,
                detail: "PCI root binding lifecycle operation is already in progress".into(),
            });
        }
        if slot.state != BindingLifecycleState::Running {
            return Err(DeviceManagerError::InvalidState {
                operation,
                detail: "PCI root binding is not running".into(),
            });
        }
        slot.state = next_state;
        slot.owner = Some(owner);
        slot.phase = HandoffPhase::Draining;
        let completion = match owner {
            LifecycleOwner::Binding => LifecycleCompletion::Restore(BindingLifecycleState::Running),
            LifecycleOwner::Reset => LifecycleCompletion::Reset,
            LifecycleOwner::Withdrawal | LifecycleOwner::Stop => {
                unreachable!("owner completion kind does not use begin_owner")
            }
        };
        Ok(LifecycleOperation {
            binding: self,
            completion,
            owner,
            claimed: true,
            completed: false,
        })
    }

    pub(super) fn take_pending_binding_withdrawals(&self) -> Vec<DeviceId> {
        core::mem::take(&mut self.lifecycle.lock_irqsave().pending_withdrawals)
    }
}

pub(super) fn retry_pending_irq_withdrawals(
    pending_storage: &SpinLock<Vec<PendingIrqWithdrawal>>,
) -> DeviceManagerResult {
    let pending = core::mem::take(&mut *pending_storage.lock_irqsave());
    let mut remaining = Vec::new();
    let mut first_error = None;
    for withdrawal in pending {
        let mut permit = EndpointIrqTransitionPermit { _private: () };
        let result = withdrawal.admission.wait_for_irq_permits().and_then(|()| {
            withdrawal
                .function
                .withdraw_irq(&mut permit)
                .map_err(DeviceManagerError::Device)
        });
        if let Err(error) = result {
            if first_error.is_none() {
                first_error = Some(error);
            }
            remaining.push(withdrawal);
        }
    }
    // A root teardown may transfer another owner while callbacks run. Merge
    // with the current queue instead of replacing it, preserving both the
    // retry results and owners arriving concurrently.
    pending_storage.lock_irqsave().extend(remaining);
    first_error.map_or(Ok(()), Err)
}

pub(super) fn transfer_pending_irq_withdrawals(
    pending_storage: &SpinLock<Vec<PendingIrqWithdrawal>>,
) {
    let pending = core::mem::take(&mut *pending_storage.lock_irqsave());
    if pending.is_empty() {
        return;
    }
    ORPHANED_IRQ_WITHDRAWALS.lock_irqsave().extend(pending);
    warn!("PCI endpoint IRQ withdrawals transferred to the fail-closed orphan queue");
}
