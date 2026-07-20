//! Interrupt state shared by Distributor and Redistributors.

use crate::{GicAffinity, IntId, InterruptState, Priority, TriggerMode};

#[derive(Clone, Debug)]
pub(crate) struct InterruptRecord {
    intid: IntId,
    enabled: bool,
    pending: bool,
    active: bool,
    inflight: bool,
    redelivery_pending: bool,
    line_asserted: bool,
    priority: Priority,
    trigger: TriggerMode,
    route: Option<GicAffinity>,
}

impl InterruptRecord {
    pub(crate) const fn new(intid: IntId, trigger: TriggerMode) -> Self {
        Self {
            intid,
            enabled: matches!(intid, IntId::Sgi(_)),
            pending: false,
            active: false,
            inflight: false,
            redelivery_pending: false,
            line_asserted: false,
            priority: Priority::DEFAULT,
            trigger,
            route: None,
        }
    }

    pub(crate) const fn intid(&self) -> IntId {
        self.intid
    }

    pub(crate) const fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if enabled && self.line_asserted {
            self.pending = true;
        }
    }

    pub(crate) const fn pending(&self) -> bool {
        self.pending
    }

    pub(crate) fn set_pending(&mut self, pending: bool) {
        self.pending = pending;
        if !pending {
            self.redelivery_pending = false;
        }
    }

    pub(crate) const fn active(&self) -> bool {
        self.active
    }

    pub(crate) fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    pub(crate) const fn state(&self) -> InterruptState {
        match (self.pending, self.active) {
            (false, false) => InterruptState::Inactive,
            (true, false) => InterruptState::Pending,
            (false, true) => InterruptState::Active,
            (true, true) => InterruptState::ActivePending,
        }
    }

    pub(crate) fn set_state(&mut self, state: InterruptState) {
        (self.pending, self.active) = match state {
            InterruptState::Inactive => (false, false),
            InterruptState::Pending => (true, false),
            InterruptState::Active => (false, true),
            InterruptState::ActivePending => (true, true),
        };
    }

    pub(crate) fn mark_inflight(&mut self) {
        self.inflight = true;
    }

    pub(crate) fn synchronize_inflight(&mut self, state: InterruptState) -> InterruptState {
        self.set_state(state);
        if self.redelivery_pending {
            self.pending = true;
        }
        let merged = self.state();
        if merged == InterruptState::ActivePending {
            // The pending delivery is now represented by the LR. Keeping a
            // software copy would enqueue the same interrupt again when that
            // LR is retired.
            self.redelivery_pending = false;
        }
        merged
    }

    pub(crate) fn cancel_inflight(&mut self) {
        self.inflight = false;
        self.redelivery_pending = false;
    }

    pub(crate) fn finish_inflight(&mut self) {
        self.inflight = false;
        self.active = false;
        self.pending =
            self.redelivery_pending || (self.trigger == TriggerMode::Level && self.line_asserted);
        self.redelivery_pending = false;
    }

    pub(crate) const fn priority(&self) -> Priority {
        self.priority
    }

    pub(crate) fn set_priority(&mut self, priority: Priority) {
        self.priority = priority;
    }

    pub(crate) const fn trigger(&self) -> TriggerMode {
        self.trigger
    }

    pub(crate) fn set_trigger(&mut self, trigger: TriggerMode) {
        self.trigger = trigger;
    }

    pub(crate) const fn route(&self) -> Option<GicAffinity> {
        self.route
    }

    pub(crate) fn set_route(&mut self, route: GicAffinity) {
        self.route = Some(route);
    }

    pub(crate) fn clear_route(&mut self) {
        self.route = None;
    }

    pub(crate) fn set_level(&mut self, asserted: bool) {
        let rising_edge = asserted && !self.line_asserted;
        self.line_asserted = asserted;
        if asserted {
            self.pending = true;
            if self.inflight && self.active && rising_edge {
                self.redelivery_pending = true;
            }
        } else if self.trigger == TriggerMode::Level && !self.active {
            self.pending = false;
            self.redelivery_pending = false;
        }
    }

    pub(crate) fn pulse(&mut self) {
        self.pending = true;
        // The hardware LR may already have transitioned from Pending to
        // Active while the VM-local snapshot still reports Pending. Preserve
        // one additional edge for reconciliation whenever an LR is in flight.
        if self.inflight {
            self.redelivery_pending = true;
        }
    }

    pub(crate) fn complete(&mut self) {
        self.active = false;
        if self.trigger == TriggerMode::Level && self.line_asserted {
            self.pending = true;
        }
    }

    pub(crate) const fn deliverable(&self) -> bool {
        self.enabled && self.pending
    }
}
