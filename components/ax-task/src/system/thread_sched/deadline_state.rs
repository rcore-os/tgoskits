use super::*;

/// Deadline admission, runqueue ownership, and typed timer registrations.
#[derive(Debug)]
pub(in crate::system) struct ThreadDeadlineState {
    pub(in crate::system) activity: DeadlineActivity,
    pub(in crate::system) bandwidth_cpu: Option<CpuId>,
    pub(in crate::system) cleanup_pending: bool,
    pub(in crate::system) bandwidth_scaled: u64,
    pub(in crate::system) active_reservation: u64,
    pub(in crate::system) desired_reservation: u64,
    pub(in crate::system) zero_lag_ns: u64,
    pub(in crate::system) cbs_timer: Option<TaskDeadlineRegistration>,
    pub(in crate::system) zero_lag_timer: Option<TaskDeadlineRegistration>,
    pub(in crate::system) replenish_pending: bool,
    pub(in crate::system) overrun_events: u64,
}

impl ThreadDeadlineState {
    pub(super) const fn new(reservation: u64) -> Self {
        Self {
            activity: DeadlineActivity::Inactive,
            bandwidth_cpu: None,
            cleanup_pending: false,
            bandwidth_scaled: reservation,
            active_reservation: reservation,
            desired_reservation: reservation,
            zero_lag_ns: 0,
            cbs_timer: None,
            zero_lag_timer: None,
            replenish_pending: false,
            overrun_events: 0,
        }
    }
}
