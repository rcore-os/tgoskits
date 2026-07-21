//! Linear registration, service, and teardown state.

use alloc::sync::Arc;
use core::{
    marker::PhantomData,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use thiserror::Error;

const PUBLISH_CLOSED: usize = 1usize << (usize::BITS - 1);
const PUBLISHER_MASK: usize = !PUBLISH_CLOSED;

/// Externally observable maintenance-domain lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum MaintenanceState {
    /// The owner may mint capabilities, but device IRQs must remain disabled.
    Registering = 0,
    /// The owner and registered local IRQ callbacks may exchange events.
    Live        = 1,
    /// New publications are rejected while IRQ actions are disabled and drained.
    Closing     = 2,
    /// Every IRQ capability is gone and accepted events are being consumed.
    Draining    = 3,
    /// The mailbox is empty and owner-local resources may be reclaimed.
    Closed      = 4,
    /// An owner vanished without completing the linear close protocol.
    Quarantined = 5,
}

impl MaintenanceState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Registering,
            1 => Self::Live,
            2 => Self::Closing,
            3 => Self::Draining,
            4 => Self::Closed,
            5 => Self::Quarantined,
            _ => panic!("invalid maintenance lifecycle state {raw}"),
        }
    }
}

/// Lifecycle transition or teardown error.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MaintenanceLifecycleError {
    /// The requested transition is not valid from the current state.
    #[error("maintenance transition requires {expected:?}, observed {actual:?}")]
    InvalidState {
        /// Required source state.
        expected: MaintenanceState,
        /// Observed state.
        actual: MaintenanceState,
    },
    /// IRQ actions or callback-local capabilities still exist.
    #[error("maintenance domain still owns {0} IRQ action or callback capabilities")]
    IrqCapabilitiesLive(usize),
    /// A task or hard-IRQ publication that began before cutoff has not returned.
    #[error("maintenance domain still has {0} active publishers")]
    PublishersActive(usize),
    /// The owner must consume all accepted mailbox evidence before closing.
    #[error("maintenance mailbox still contains accepted evidence")]
    MailboxPending,
    /// An IRQ capability counter would exceed its representable range.
    #[error("maintenance IRQ capability counter exhausted")]
    CapabilityOverflow,
}

/// Proof that a domain completed IRQ teardown and mailbox draining.
///
/// This proof is owner-thread local and is accepted by [`crate::maintenance::LocalOwnerCell`]
/// when reclaiming its pinned device state.
#[derive(Debug)]
pub struct MaintenanceClosed {
    pub(super) lifecycle: Arc<MaintenanceLifecycle>,
    pub(super) _not_send: PhantomData<*mut ()>,
}

impl MaintenanceClosed {
    /// Returns the terminal state represented by this proof.
    pub const fn state(&self) -> MaintenanceState {
        MaintenanceState::Closed
    }
}

#[derive(Debug)]
pub(super) struct MaintenanceLifecycle {
    state: AtomicU8,
    publish_gate: AtomicUsize,
    irq_capabilities: AtomicUsize,
}

impl MaintenanceLifecycle {
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicU8::new(MaintenanceState::Registering as u8),
            publish_gate: AtomicUsize::new(0),
            irq_capabilities: AtomicUsize::new(0),
        }
    }

    pub(super) fn state(&self) -> MaintenanceState {
        MaintenanceState::from_raw(self.state.load(Ordering::Acquire))
    }

    pub(super) fn register_irq_capability(&self) -> Result<(), MaintenanceLifecycleError> {
        self.register_irq_capability_in(MaintenanceState::Registering)
    }

    pub(super) fn register_live_irq_capability(&self) -> Result<(), MaintenanceLifecycleError> {
        self.register_irq_capability_in(MaintenanceState::Live)
    }

    fn register_irq_capability_in(
        &self,
        expected: MaintenanceState,
    ) -> Result<(), MaintenanceLifecycleError> {
        let actual = self.state();
        if actual != expected {
            return Err(MaintenanceLifecycleError::InvalidState { expected, actual });
        }
        let previous = self.irq_capabilities.fetch_add(1, Ordering::AcqRel);
        if previous == usize::MAX {
            self.irq_capabilities.fetch_sub(1, Ordering::Release);
            return Err(MaintenanceLifecycleError::CapabilityOverflow);
        }
        let actual = self.state();
        if actual == expected {
            Ok(())
        } else {
            self.release_irq_capability();
            Err(MaintenanceLifecycleError::InvalidState { expected, actual })
        }
    }

    pub(super) fn release_irq_capability(&self) {
        let previous = self.irq_capabilities.fetch_sub(1, Ordering::AcqRel);
        assert!(previous != 0, "maintenance IRQ capability underflow");
    }

    pub(super) fn activate(&self) -> Result<(), MaintenanceLifecycleError> {
        self.transition(MaintenanceState::Registering, MaintenanceState::Live)
    }

    pub(super) fn begin_close(&self) -> Result<(), MaintenanceLifecycleError> {
        match self.state() {
            MaintenanceState::Live => {
                self.transition(MaintenanceState::Live, MaintenanceState::Closing)?;
                self.publish_gate.fetch_or(PUBLISH_CLOSED, Ordering::AcqRel);
                Ok(())
            }
            MaintenanceState::Closing | MaintenanceState::Draining | MaintenanceState::Closed => {
                Ok(())
            }
            actual => Err(MaintenanceLifecycleError::InvalidState {
                expected: MaintenanceState::Live,
                actual,
            }),
        }
    }

    pub(super) fn abort_registration(&self) {
        if self
            .state
            .compare_exchange(
                MaintenanceState::Registering as u8,
                MaintenanceState::Closing as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.publish_gate.fetch_or(PUBLISH_CLOSED, Ordering::AcqRel);
        }
    }

    pub(super) fn quarantine(&self) {
        self.quarantine_atomic();
    }

    /// Atomically closes publication and rejects future IRQ endpoint access.
    ///
    /// This transition performs no allocation, blocking operation, or callback
    /// and is therefore safe in the hard-IRQ fail-closed path.
    pub(super) fn quarantine_from_irq(&self) {
        self.quarantine_atomic();
    }

    fn quarantine_atomic(&self) {
        self.publish_gate.fetch_or(PUBLISH_CLOSED, Ordering::AcqRel);
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            if matches!(
                MaintenanceState::from_raw(observed),
                MaintenanceState::Closed | MaintenanceState::Quarantined
            ) {
                return;
            }
            match self.state.compare_exchange_weak(
                observed,
                MaintenanceState::Quarantined as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => observed = actual,
            }
        }
    }

    pub(super) fn try_begin_draining(&self) -> Result<(), MaintenanceLifecycleError> {
        let state = self.state();
        if state == MaintenanceState::Draining || state == MaintenanceState::Closed {
            return Ok(());
        }
        if state != MaintenanceState::Closing {
            return Err(MaintenanceLifecycleError::InvalidState {
                expected: MaintenanceState::Closing,
                actual: state,
            });
        }
        let capabilities = self.irq_capabilities.load(Ordering::Acquire);
        if capabilities != 0 {
            return Err(MaintenanceLifecycleError::IrqCapabilitiesLive(capabilities));
        }
        let publishers = self.active_publishers();
        if publishers != 0 {
            return Err(MaintenanceLifecycleError::PublishersActive(publishers));
        }
        self.transition(MaintenanceState::Closing, MaintenanceState::Draining)
    }

    pub(super) fn finish_close(
        &self,
        mailbox_pending: bool,
    ) -> Result<(), MaintenanceLifecycleError> {
        if mailbox_pending {
            return Err(MaintenanceLifecycleError::MailboxPending);
        }
        self.transition(MaintenanceState::Draining, MaintenanceState::Closed)
    }

    pub(super) fn begin_publish(&self) -> Result<MaintenancePublisher<'_>, PublishClosed> {
        self.begin_publish_after_initial_check(|| {})
    }

    fn begin_publish_after_initial_check(
        &self,
        after_initial_check: impl FnOnce(),
    ) -> Result<MaintenancePublisher<'_>, PublishClosed> {
        let mut observed = self.publish_gate.load(Ordering::Acquire);
        let mut after_initial_check = Some(after_initial_check);
        loop {
            if observed & PUBLISH_CLOSED != 0 || self.state() != MaintenanceState::Live {
                return Err(PublishClosed);
            }
            if let Some(after_initial_check) = after_initial_check.take() {
                after_initial_check();
            }
            let publishers = observed & PUBLISHER_MASK;
            assert!(
                publishers != PUBLISHER_MASK,
                "maintenance publisher overflow"
            );
            match self.publish_gate.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    let publisher = MaintenancePublisher { lifecycle: self };
                    // The state and close bit are distinct atomics. A producer
                    // may have read Live immediately before the owner changes
                    // it to Closing, then reserve against the still-open gate.
                    // Revalidate after reservation so that interleaving rolls
                    // the count back instead of admitting post-cutoff work.
                    if self.state() != MaintenanceState::Live
                        || self.publish_gate.load(Ordering::Acquire) & PUBLISH_CLOSED != 0
                    {
                        drop(publisher);
                        return Err(PublishClosed);
                    }
                    return Ok(publisher);
                }
                Err(actual) => observed = actual,
            }
        }
    }

    pub(super) fn permits_control_access(&self) -> bool {
        !matches!(
            self.state(),
            MaintenanceState::Closed | MaintenanceState::Quarantined
        )
    }

    pub(super) fn permits_action_enable(&self) -> bool {
        self.state() == MaintenanceState::Live
    }

    pub(super) fn permits_irq_access(&self) -> bool {
        matches!(
            self.state(),
            MaintenanceState::Live | MaintenanceState::Closing
        )
    }

    /// Reports whether a local hard-IRQ callback may publish a new snapshot.
    ///
    /// IRQ publishers do not enter the task-producer CAS gate. Their action
    /// capability remains counted until the owner disables and synchronizes
    /// the action, which proves every publication that observed `Live` has
    /// returned before the owner can enter `Draining`.
    pub(super) fn permits_irq_publication(&self) -> bool {
        self.state() == MaintenanceState::Live
    }

    pub(super) fn permits_service_access(&self) -> bool {
        matches!(
            self.state(),
            MaintenanceState::Live | MaintenanceState::Closing | MaintenanceState::Draining
        )
    }

    fn active_publishers(&self) -> usize {
        self.publish_gate.load(Ordering::Acquire) & PUBLISHER_MASK
    }

    fn transition(
        &self,
        expected: MaintenanceState,
        next: MaintenanceState,
    ) -> Result<(), MaintenanceLifecycleError> {
        self.state
            .compare_exchange(
                expected as u8,
                next as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|actual| MaintenanceLifecycleError::InvalidState {
                expected,
                actual: MaintenanceState::from_raw(actual),
            })
    }
}

pub(super) struct MaintenancePublisher<'lifecycle> {
    lifecycle: &'lifecycle MaintenanceLifecycle,
}

impl Drop for MaintenancePublisher<'_> {
    fn drop(&mut self) {
        let previous = self.lifecycle.publish_gate.fetch_sub(1, Ordering::Release);
        assert!(
            previous & PUBLISHER_MASK != 0,
            "maintenance publisher underflow"
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PublishClosed;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_waits_for_irq_capabilities_and_inflight_publishers() {
        let lifecycle = MaintenanceLifecycle::new();
        lifecycle.register_irq_capability().unwrap();
        lifecycle.activate().unwrap();
        let publisher = lifecycle.begin_publish().unwrap();
        lifecycle.begin_close().unwrap();

        assert_eq!(
            lifecycle.try_begin_draining(),
            Err(MaintenanceLifecycleError::IrqCapabilitiesLive(1))
        );
        lifecycle.release_irq_capability();
        assert_eq!(
            lifecycle.try_begin_draining(),
            Err(MaintenanceLifecycleError::PublishersActive(1))
        );
        drop(publisher);
        lifecycle.try_begin_draining().unwrap();
        lifecycle.finish_close(false).unwrap();
        assert_eq!(lifecycle.state(), MaintenanceState::Closed);
    }

    #[test]
    fn publish_cannot_enter_after_close_cutoff() {
        let lifecycle = MaintenanceLifecycle::new();
        lifecycle.activate().unwrap();
        assert!(lifecycle.permits_irq_publication());
        lifecycle.begin_close().unwrap();
        assert!(!lifecycle.permits_irq_publication());
        assert_eq!(lifecycle.begin_publish().err(), Some(PublishClosed));
    }

    #[test]
    fn stale_live_observation_cannot_publish_after_close_transition() {
        let lifecycle = MaintenanceLifecycle::new();
        lifecycle.activate().unwrap();

        let result = lifecycle.begin_publish_after_initial_check(|| {
            assert_eq!(
                lifecycle.state.compare_exchange(
                    MaintenanceState::Live as u8,
                    MaintenanceState::Closing as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ),
                Ok(MaintenanceState::Live as u8)
            );
        });

        assert_eq!(result.err(), Some(PublishClosed));
        assert_eq!(lifecycle.active_publishers(), 0);
    }

    #[test]
    fn close_waits_for_a_capability_registered_while_live() {
        let lifecycle = MaintenanceLifecycle::new();
        lifecycle.activate().unwrap();
        lifecycle.register_live_irq_capability().unwrap();
        lifecycle.begin_close().unwrap();

        assert_eq!(
            lifecycle.try_begin_draining(),
            Err(MaintenanceLifecycleError::IrqCapabilitiesLive(1))
        );
        lifecycle.release_irq_capability();
        lifecycle.try_begin_draining().unwrap();
        lifecycle.finish_close(false).unwrap();
    }

    #[test]
    fn irq_action_enable_is_permitted_only_after_owner_activation() {
        let lifecycle = MaintenanceLifecycle::new();
        assert!(!lifecycle.permits_action_enable());

        lifecycle.activate().unwrap();
        assert!(lifecycle.permits_action_enable());

        lifecycle.begin_close().unwrap();
        assert!(!lifecycle.permits_action_enable());
    }
}
