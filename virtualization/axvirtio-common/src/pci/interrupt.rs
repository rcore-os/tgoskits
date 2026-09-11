//! Level-triggered VirtIO PCI interrupt state.
//!
//! The coordinator owns only VirtIO ISR and suppression state.  It returns
//! transition intents to its caller; the endpoint context is responsible for
//! executing those intents through an admitted `EndpointIrqTransitionPermit`.

use ax_sync::SpinLock;

/// A physical interrupt transition requested by the coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptTransition {
    /// No physical line transition is needed.
    None,
    /// Assert the level-triggered line.
    Assert,
    /// Deassert the level-triggered line.
    Deassert,
}

/// ISR value captured by a read, together with the resulting line intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterruptReadOutcome {
    /// The value returned to the guest.
    pub value: u8,
    /// The line transition to execute after the read is committed.
    pub transition: InterruptTransition,
}

#[derive(Default)]
struct InterruptState {
    isr: u8,
    asserted: bool,
    disabled: bool,
    needs_resync: bool,
    transition_in_flight: Option<bool>,
}

/// VirtIO PCI ISR and level-INTx state machine.
pub struct VirtioPciInterruptCoordinator {
    state: SpinLock<InterruptState>,
}

impl VirtioPciInterruptCoordinator {
    /// Creates an idle, interrupt-enabled coordinator.
    pub const fn new() -> Self {
        Self {
            state: SpinLock::new(InterruptState {
                isr: 0,
                asserted: false,
                disabled: false,
                needs_resync: false,
                transition_in_flight: None,
            }),
        }
    }

    /// Returns whether any ISR bit is pending.
    pub fn pending(&self) -> bool {
        self.state.lock().isr != 0
    }

    /// Returns whether the logical line is currently asserted.
    pub fn asserted(&self) -> bool {
        self.state.lock().asserted
    }

    /// Returns whether the last physical transition needs to be retried.
    pub fn needs_resync(&self) -> bool {
        self.state.lock().needs_resync
    }

    /// Records a used-ring completion and returns a line transition intent.
    pub fn record_queue_completion(&self, notify: bool) -> InterruptTransition {
        self.record(1, notify)
    }

    /// Records a device-configuration change and returns a line transition intent.
    pub fn record_config_change(&self) -> InterruptTransition {
        self.record(2, true)
    }

    /// Suppresses one stale queue completion while preserving configuration
    /// change and other independently pending ISR state. `transition`
    /// identifies the queue-owned line intent, if it had one.
    pub fn suppress_queue_completion(
        &self,
        transition: InterruptTransition,
    ) -> InterruptTransition {
        let mut state = self.state.lock();
        state.isr &= !1;
        let target = match transition {
            InterruptTransition::Assert => Some(true),
            InterruptTransition::Deassert => Some(false),
            InterruptTransition::None => None,
        };
        if target.is_some() && state.transition_in_flight == target {
            // Suppression cancels the stale queue-owned transition. Retain a
            // retry request whenever the current logical line is mismatched,
            // including when a configuration ISR bit remains pending.
            state.transition_in_flight = None;
            let desired = !state.disabled && state.isr != 0;
            state.needs_resync = state.asserted != desired;
            return InterruptTransition::None;
        }
        requested_transition(&mut state)
    }

    /// Reads and clears the ISR bits, returning the line transition intent.
    pub fn read_isr(&self) -> InterruptReadOutcome {
        let mut state = self.state.lock();
        let value = state.isr;
        state.isr = 0;
        let transition = requested_transition(&mut state);
        InterruptReadOutcome { value, transition }
    }

    /// Applies the PCI Command.INTx Disable state and returns a line intent.
    pub fn set_disabled(&self, disabled: bool) -> InterruptTransition {
        let mut state = self.state.lock();
        state.disabled = disabled;
        requested_transition(&mut state)
    }

    /// Commits the result of a transition executed by the endpoint context.
    ///
    /// A failed physical operation leaves the logical state retryable; the
    /// caller decides whether that failure also fails the guest-facing access.
    /// A successful transition returns a follow-up intent when another
    /// concurrent ISR/state change made the current physical target stale.
    pub fn complete_transition(
        &self,
        transition: InterruptTransition,
        success: bool,
    ) -> InterruptTransition {
        if transition == InterruptTransition::None {
            return InterruptTransition::None;
        }
        let mut state = self.state.lock();
        let target = match transition {
            InterruptTransition::Assert => true,
            InterruptTransition::Deassert => false,
            InterruptTransition::None => return InterruptTransition::None,
        };
        if state.transition_in_flight != Some(target) {
            return InterruptTransition::None;
        }
        state.transition_in_flight = None;
        if success {
            state.asserted = target;
            state.needs_resync = false;
            requested_transition(&mut state)
        } else {
            state.needs_resync = true;
            InterruptTransition::None
        }
    }

    /// Suppresses a stale endpoint transition without recording a line error.
    pub fn suppress_transition(&self, transition: InterruptTransition) {
        if transition == InterruptTransition::None {
            return;
        }
        let mut state = self.state.lock();
        let target = matches!(transition, InterruptTransition::Assert);
        if state.transition_in_flight == Some(target) {
            state.transition_in_flight = None;
            let desired = !state.disabled && state.isr != 0;
            state.needs_resync = state.asserted != desired;
        }
    }

    /// Suppresses a transition whose VirtIO queue generation is stale.
    ///
    /// A stale generation is not a physical-line failure. Preserve any
    /// existing retry state and only release the matching in-flight intent.
    pub(super) fn suppress_stale_transition(&self, transition: InterruptTransition) {
        if transition == InterruptTransition::None {
            return;
        }
        let mut state = self.state.lock();
        let target = matches!(transition, InterruptTransition::Assert);
        if state.transition_in_flight == Some(target) {
            state.transition_in_flight = None;
        }
    }

    /// Cancels a transition that was admitted but never executed by its
    /// caller. The physical line remains at `asserted`, so keep the logical
    /// mismatch retryable for the next synchronization point.
    pub fn cancel_transition(&self, transition: InterruptTransition) {
        if transition == InterruptTransition::None {
            return;
        }
        let mut state = self.state.lock();
        let target = matches!(transition, InterruptTransition::Assert);
        if state.transition_in_flight == Some(target) {
            state.transition_in_flight = None;
            let desired = !state.disabled && state.isr != 0;
            state.needs_resync = state.asserted != desired;
        }
    }

    /// Returns the next transition needed to synchronize the physical line.
    pub fn resynchronize(&self) -> InterruptTransition {
        let mut state = self.state.lock();
        requested_transition(&mut state)
    }

    /// Clears all state and returns the line intent needed to leave idle.
    pub fn reset(&self) -> InterruptTransition {
        let mut state = self.state.lock();
        let transition = if state.asserted || state.needs_resync {
            InterruptTransition::Deassert
        } else {
            InterruptTransition::None
        };
        *state = InterruptState {
            disabled: state.disabled,
            asserted: state.asserted,
            ..InterruptState::default()
        };
        if transition == InterruptTransition::Deassert {
            // Keep the owner-side reset deassertion in the same completion
            // protocol as an ordinary ISR transition. A failed physical
            // operation must remain retryable through `resynchronize`.
            state.transition_in_flight = Some(false);
        }
        transition
    }

    fn record(&self, bit: u8, notify: bool) -> InterruptTransition {
        let mut state = self.state.lock();
        state.isr |= bit;
        if notify {
            requested_transition(&mut state)
        } else {
            InterruptTransition::None
        }
    }
}

fn requested_transition(state: &mut InterruptState) -> InterruptTransition {
    if state.transition_in_flight.is_some() {
        return InterruptTransition::None;
    }
    let desired = !state.disabled && state.isr != 0;
    if desired == state.asserted {
        state.needs_resync = false;
        return InterruptTransition::None;
    }
    state.transition_in_flight = Some(desired);
    state.needs_resync = false;
    if desired {
        InterruptTransition::Assert
    } else {
        InterruptTransition::Deassert
    }
}

impl Default for VirtioPciInterruptCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_is_suppressed_until_interrupts_are_enabled() {
        let coordinator = VirtioPciInterruptCoordinator::new();
        assert_eq!(coordinator.set_disabled(true), InterruptTransition::None);
        assert_eq!(
            coordinator.record_queue_completion(true),
            InterruptTransition::None
        );
        assert!(coordinator.pending());
        assert_eq!(coordinator.set_disabled(false), InterruptTransition::Assert);
        let assert_transition = coordinator.resynchronize();
        assert_eq!(assert_transition, InterruptTransition::None);
        coordinator.complete_transition(InterruptTransition::Assert, true);
        assert_eq!(
            coordinator.read_isr(),
            InterruptReadOutcome {
                value: 1,
                transition: InterruptTransition::Deassert,
            }
        );
        coordinator.complete_transition(InterruptTransition::Deassert, true);
    }

    #[test]
    fn failed_line_transition_is_retryable_without_losing_isr_state() {
        let coordinator = VirtioPciInterruptCoordinator::new();
        let assert_transition = coordinator.record_config_change();
        assert_eq!(assert_transition, InterruptTransition::Assert);
        coordinator.complete_transition(assert_transition, false);
        assert!(coordinator.needs_resync());
        let retry_assert = coordinator.resynchronize();
        assert_eq!(retry_assert, InterruptTransition::Assert);
        coordinator.complete_transition(retry_assert, true);

        let read = coordinator.read_isr();
        assert_eq!(read.value, 2);
        coordinator.complete_transition(read.transition, false);
        let deassert = coordinator.resynchronize();
        assert_eq!(deassert, InterruptTransition::Deassert);
        coordinator.complete_transition(deassert, true);
        assert!(!coordinator.needs_resync());
    }

    #[test]
    fn stale_queue_suppression_preserves_config_change() {
        let coordinator = VirtioPciInterruptCoordinator::new();
        assert_eq!(
            coordinator.record_config_change(),
            InterruptTransition::Assert
        );
        assert_eq!(
            coordinator.record_queue_completion(true),
            InterruptTransition::None
        );
        assert_eq!(
            coordinator.suppress_queue_completion(InterruptTransition::None),
            InterruptTransition::None
        );
        assert_eq!(coordinator.read_isr().value, 2);
    }

    #[test]
    fn stale_queue_suppression_releases_a_pending_deassertion() {
        let coordinator = VirtioPciInterruptCoordinator::new();
        let assert_transition = coordinator.record_config_change();
        coordinator.complete_transition(assert_transition, true);
        assert_eq!(
            coordinator.set_disabled(true),
            InterruptTransition::Deassert
        );
        coordinator.complete_transition(InterruptTransition::Deassert, false);

        assert_eq!(
            coordinator.record_queue_completion(true),
            InterruptTransition::Deassert
        );
        assert_eq!(
            coordinator.suppress_queue_completion(InterruptTransition::Deassert),
            InterruptTransition::None
        );
        assert_eq!(coordinator.resynchronize(), InterruptTransition::Deassert);
    }

    #[test]
    fn stale_queue_assert_suppression_releases_config_assertion() {
        let coordinator = VirtioPciInterruptCoordinator::new();
        assert_eq!(
            coordinator.record_queue_completion(true),
            InterruptTransition::Assert
        );
        assert_eq!(
            coordinator.record_config_change(),
            InterruptTransition::None
        );
        assert_eq!(
            coordinator.suppress_queue_completion(InterruptTransition::Assert),
            InterruptTransition::None
        );
        assert_eq!(coordinator.resynchronize(), InterruptTransition::Assert);
    }

    #[test]
    fn completion_reconciles_a_read_that_raced_with_assertion() {
        let coordinator = VirtioPciInterruptCoordinator::new();
        let assert_transition = coordinator.record_queue_completion(true);
        assert_eq!(assert_transition, InterruptTransition::Assert);
        let read = coordinator.read_isr();
        assert_eq!(read.value, 1);
        assert_eq!(read.transition, InterruptTransition::None);

        let deassert_transition = coordinator.complete_transition(assert_transition, true);
        assert_eq!(deassert_transition, InterruptTransition::Deassert);
        coordinator.complete_transition(deassert_transition, true);
        assert!(!coordinator.asserted());
    }

    #[test]
    fn reset_deassertion_remains_retryable_after_a_line_failure() {
        let coordinator = VirtioPciInterruptCoordinator::new();
        let assert_transition = coordinator.record_config_change();
        coordinator.complete_transition(assert_transition, true);

        let reset_transition = coordinator.reset();
        assert_eq!(reset_transition, InterruptTransition::Deassert);
        coordinator.complete_transition(reset_transition, false);
        assert!(coordinator.needs_resync());

        let retry = coordinator.resynchronize();
        assert_eq!(retry, InterruptTransition::Deassert);
        coordinator.complete_transition(retry, true);
        assert!(!coordinator.needs_resync());
    }

    #[test]
    fn stale_transition_suppression_preserves_existing_retry_state() {
        let coordinator = VirtioPciInterruptCoordinator::new();
        let assert_transition = coordinator.record_queue_completion(true);
        assert_eq!(assert_transition, InterruptTransition::Assert);
        coordinator.complete_transition(assert_transition, false);
        assert!(coordinator.needs_resync());

        coordinator.suppress_stale_transition(InterruptTransition::Assert);
        assert!(coordinator.needs_resync());
    }
}
