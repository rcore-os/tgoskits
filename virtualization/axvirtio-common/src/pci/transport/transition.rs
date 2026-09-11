use alloc::sync::Arc;

use axdevice_base::DeviceResult;

use super::{ActivityPermit, QueueNotifyOutcome};
use crate::pci::{InterruptTransition, VirtioPciInterruptCoordinator};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InterruptPublicationKind {
    Queue,
    Configuration,
}

/// Queue notification result whose activity permit remains alive until the
/// endpoint has published or deliberately suppressed the completion interrupt.
pub struct QueueNotification {
    pub(super) outcome: QueueNotifyOutcome,
    pub(super) publication: InterruptPublicationRequest,
}

impl QueueNotification {
    /// Returns the device-core result.
    pub const fn outcome(&self) -> QueueNotifyOutcome {
        self.outcome
    }

    /// Returns whether publishing this notification requires an endpoint IRQ
    /// transition permit.
    pub const fn requires_interrupt_publication(&self) -> bool {
        self.publication.requires_irq_permit()
    }

    /// Returns the queue configuration generation covered by this terminal
    /// notification, if processing was admitted.
    pub const fn generation(&self) -> Option<VirtioQueueGeneration> {
        self.publication.generation()
    }

    /// Explicitly ends the activity lifetime without publishing an ISR bit.
    pub fn complete(self) {
        self.publication.cancel();
    }

    /// Publishes the completion ISR and line transition after endpoint IRQ
    /// admission, then releases queue activity.
    pub fn publish<F>(self, publish_transition: F) -> DeviceResult
    where
        F: FnMut(InterruptTransition) -> DeviceResult,
    {
        self.publication.publish(publish_transition)
    }

    /// Consumes this notification and returns its pending ISR publication.
    pub fn into_interrupt_publication(self) -> InterruptPublicationRequest {
        self.publication
    }
}

/// ISR publication retained until the endpoint has acquired its IRQ permit.
pub struct InterruptPublicationRequest {
    kind: Option<InterruptPublicationKind>,
    activity: Option<ActivityPermit>,
    interrupts: Arc<VirtioPciInterruptCoordinator>,
}

impl core::fmt::Debug for InterruptPublicationRequest {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InterruptPublicationRequest")
            .field("kind", &self.kind)
            .field("has_activity", &self.activity.is_some())
            .finish_non_exhaustive()
    }
}

impl InterruptPublicationRequest {
    pub(super) fn new(
        interrupts: Arc<VirtioPciInterruptCoordinator>,
        kind: Option<InterruptPublicationKind>,
        activity: Option<ActivityPermit>,
    ) -> Self {
        Self {
            kind,
            activity,
            interrupts,
        }
    }

    /// Returns whether ISR publication must be admitted by the endpoint.
    pub const fn requires_irq_permit(&self) -> bool {
        self.kind.is_some()
    }

    /// Returns the queue generation protected by the activity permit.
    pub const fn generation(&self) -> Option<VirtioQueueGeneration> {
        match &self.activity {
            Some(activity) => Some(activity.generation),
            None => None,
        }
    }

    /// Records the ISR bit and executes all resulting line transitions.
    ///
    /// The caller must hold the endpoint IRQ transition permit before calling
    /// this method. A failed line operation leaves the ISR state retryable and
    /// is returned to the guest-facing dispatcher.
    pub fn publish<F>(mut self, mut publish_transition: F) -> DeviceResult
    where
        F: FnMut(InterruptTransition) -> DeviceResult,
    {
        let Some(kind) = self.kind.take() else {
            self.activity.take();
            return Ok(());
        };
        let mut transition = match kind {
            InterruptPublicationKind::Queue => self.interrupts.record_queue_completion(true),
            InterruptPublicationKind::Configuration => self.interrupts.record_config_change(),
        };
        loop {
            if let Err(error) = publish_transition(transition) {
                self.interrupts.complete_transition(transition, false);
                self.activity.take();
                return Err(error);
            }
            transition = self.interrupts.complete_transition(transition, true);
            if transition == InterruptTransition::None {
                self.activity.take();
                return Ok(());
            }
        }
    }

    /// Cancels publication and releases queue activity without recording an ISR bit.
    pub fn cancel(mut self) {
        self.activity.take();
    }
}

/// Immutable Command.INTx transition intent bound to one VirtIO queue
/// generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterruptTransitionIntent {
    transition: InterruptTransition,
    generation: VirtioQueueGeneration,
}

impl InterruptTransitionIntent {
    pub(super) const fn new(
        transition: InterruptTransition,
        generation: VirtioQueueGeneration,
    ) -> Self {
        Self {
            transition,
            generation,
        }
    }

    /// Returns the physical transition that may be published for this intent.
    pub const fn transition(self) -> InterruptTransition {
        self.transition
    }

    /// Returns the VirtIO queue generation captured with this intent.
    pub const fn generation(self) -> VirtioQueueGeneration {
        self.generation
    }
}

/// Identity of one queue configuration lifetime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtioQueueGeneration(pub(super) u64);

impl VirtioQueueGeneration {
    /// Creates a generation token from a value captured by a transport.
    pub const fn from_value(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric generation for diagnostics and tests.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Interrupt transition intent retained until its endpoint callback finishes.
pub struct InterruptTransitionRequest {
    transition: InterruptTransition,
    activity: Option<ActivityPermit>,
    interrupts: Arc<VirtioPciInterruptCoordinator>,
}

impl core::fmt::Debug for InterruptTransitionRequest {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InterruptTransitionRequest")
            .field("transition", &self.transition)
            .field("has_activity", &self.activity.is_some())
            .finish_non_exhaustive()
    }
}

impl InterruptTransitionRequest {
    pub(super) fn new(
        interrupts: Arc<VirtioPciInterruptCoordinator>,
        transition: InterruptTransition,
        activity: Option<ActivityPermit>,
    ) -> Self {
        Self {
            transition,
            activity,
            interrupts,
        }
    }

    pub(super) fn without_activity(
        interrupts: Arc<VirtioPciInterruptCoordinator>,
        transition: InterruptTransition,
    ) -> Self {
        Self::new(interrupts, transition, None)
    }

    /// Returns the physical transition that the endpoint must publish.
    pub const fn transition(&self) -> InterruptTransition {
        self.transition
    }
}

impl Drop for InterruptTransitionRequest {
    fn drop(&mut self) {
        self.interrupts.cancel_transition(self.transition);
        self.activity.take();
    }
}
