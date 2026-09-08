use super::*;

/// Mutually exclusive GRUB activity owned with one rq assignment.
///
/// The zero-lag timestamp exists only in the non-contending state. This makes
/// it impossible for wake, timer expiry, policy replacement, or CPU hotplug to
/// publish the old independent `activity` and `zero_lag` fields out of sync.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeadlineBandwidthActivity {
    Inactive,
    Contending,
    NonContending { zero_lag: SchedulerTimestamp },
}

/// Linux `dl_rq::{this_bw,running_bw}` membership of one admitted task.
#[derive(Debug)]
pub(in crate::system) struct DeadlineBandwidthState {
    reservation_owner: Option<CpuId>,
    reservation_scaled: u64,
    activity: DeadlineBandwidthActivity,
}

impl DeadlineBandwidthState {
    const fn new(reservation_scaled: u64) -> Self {
        Self {
            reservation_owner: None,
            reservation_scaled,
            activity: DeadlineBandwidthActivity::Inactive,
        }
    }

    pub(in crate::system) const fn reservation_owner(&self) -> Option<CpuId> {
        self.reservation_owner
    }

    pub(in crate::system) const fn reservation_scaled(&self) -> u64 {
        self.reservation_scaled
    }

    pub(in crate::system) const fn activity(&self) -> DeadlineActivity {
        match self.activity {
            DeadlineBandwidthActivity::Inactive => DeadlineActivity::Inactive,
            DeadlineBandwidthActivity::Contending => DeadlineActivity::ActiveContending,
            DeadlineBandwidthActivity::NonContending { .. } => {
                DeadlineActivity::ActiveNonContending
            }
        }
    }

    pub(in crate::system) const fn is_active(&self) -> bool {
        !matches!(self.activity, DeadlineBandwidthActivity::Inactive)
    }

    pub(in crate::system) const fn is_contending(&self) -> bool {
        matches!(self.activity, DeadlineBandwidthActivity::Contending)
    }

    pub(in crate::system) const fn zero_lag(&self) -> Option<SchedulerTimestamp> {
        match self.activity {
            DeadlineBandwidthActivity::NonContending { zero_lag } => Some(zero_lag),
            DeadlineBandwidthActivity::Inactive | DeadlineBandwidthActivity::Contending => None,
        }
    }

    pub(in crate::system) fn attach(&mut self, owner: CpuId) {
        assert!(
            self.reservation_owner.replace(owner).is_none(),
            "one Deadline reservation cannot belong to two runqueues"
        );
    }

    pub(in crate::system) fn detach(&mut self, owner: CpuId) {
        assert_eq!(
            self.reservation_owner.take(),
            Some(owner),
            "Deadline reservation detach must name its owner rq"
        );
    }

    pub(in crate::system) fn activate_contending(&mut self) {
        self.activity = DeadlineBandwidthActivity::Contending;
    }

    pub(in crate::system) fn mark_non_contending(&mut self, zero_lag: SchedulerTimestamp) {
        assert!(
            self.is_contending(),
            "only a contending Deadline reservation can become non-contending"
        );
        self.activity = DeadlineBandwidthActivity::NonContending { zero_lag };
    }

    pub(in crate::system) fn deactivate(&mut self) {
        self.activity = DeadlineBandwidthActivity::Inactive;
    }

    /// Installs a new policy reservation after the old rq membership was
    /// detached. Admission was already committed by the pending policy
    /// transaction, so owner apply changes no root-domain counter here.
    pub(in crate::system) fn replace_detached_reservation(&mut self, reservation_scaled: u64) {
        assert!(
            self.reservation_owner.is_none(),
            "Deadline policy replacement requires detached rq bandwidth"
        );
        self.reservation_scaled = reservation_scaled;
        self.activity = DeadlineBandwidthActivity::Inactive;
    }
}

/// Deadline admission, runqueue ownership, and typed timer registrations.
#[derive(Debug)]
pub(in crate::system) struct ThreadDeadlineState {
    pub(in crate::system) server: crate::DeadlineServer,
    pub(in crate::system) bandwidth: DeadlineBandwidthState,
    pub(in crate::system) cbs_timer: Option<TaskDeadlineRegistration>,
    pub(in crate::system) zero_lag_timer: Option<TaskDeadlineRegistration>,
    pub(in crate::system) overrun_events: u64,
}

impl ThreadDeadlineState {
    pub(super) fn new(server: crate::DeadlineServer, reservation: u64) -> Self {
        Self {
            server,
            bandwidth: DeadlineBandwidthState::new(reservation),
            cbs_timer: None,
            zero_lag_timer: None,
            overrun_events: 0,
        }
    }
}
