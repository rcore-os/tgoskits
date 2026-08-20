use super::*;

/// One not-yet-applied base-policy transaction.
///
/// Policy parameters and their Deadline admission reservation are published as
/// one value. The owner rq either consumes the complete transaction or leaves
/// it pending; no independently mutable requested-policy or desired-bandwidth
/// truth exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::system) struct PendingPolicyUpdate {
    pub(in crate::system) policy: SchedulePolicy,
    pub(in crate::system) reservation_scaled: u64,
    pub(in crate::system) generation: u64,
}

/// Owner-applied base policy plus at most one remote update transaction.
///
/// `active` is populated only while the complete task scheduling state is
/// detached from every rq and CPU. Base and inherited class entities travel
/// together inside that value; this task-control object never owns a second
/// parked copy.
#[derive(Debug)]
pub(in crate::system) struct ThreadPolicyState {
    pub(in crate::system) base: SchedulePolicy,
    update_generation: u64,
    pending: Option<PendingPolicyUpdate>,
    pub(in crate::system) dispatch_generation: u64,
    active: Option<ActiveSchedulingState>,
}

impl ThreadPolicyState {
    pub(super) fn new(policy: SchedulePolicy, entity: SchedulingEntity) -> Self {
        Self {
            base: policy,
            update_generation: 1,
            pending: None,
            dispatch_generation: 1,
            active: Some(ActiveSchedulingState::new(policy, entity)),
        }
    }

    pub(in crate::system) fn requested_policy(&self) -> SchedulePolicy {
        self.pending.map_or(self.base, |pending| pending.policy)
    }

    pub(in crate::system) const fn update_generation(&self) -> u64 {
        self.update_generation
    }

    pub(in crate::system) const fn pending_update(&self) -> Option<PendingPolicyUpdate> {
        self.pending
    }

    pub(in crate::system) fn prepare_update(
        &self,
        policy: SchedulePolicy,
        reservation_scaled: u64,
    ) -> Result<PendingPolicyUpdate, TaskError> {
        let generation = self
            .update_generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        Ok(PendingPolicyUpdate {
            policy,
            reservation_scaled,
            generation,
        })
    }

    pub(in crate::system) fn publish_update(&mut self, pending: PendingPolicyUpdate) {
        assert_eq!(
            pending.generation,
            self.update_generation
                .checked_add(1)
                .expect("validated policy generation cannot overflow")
        );
        self.update_generation = pending.generation;
        self.pending = Some(pending);
    }

    pub(in crate::system) fn commit_pending_update(&mut self) -> PendingPolicyUpdate {
        let pending = self
            .pending
            .take()
            .expect("owner policy transaction must retain one pending value");
        self.base = pending.policy;
        pending
    }

    pub(in crate::system) fn discard_pending_update(&mut self) {
        self.pending = None;
    }

    pub(in crate::system) fn active(&self) -> &ActiveSchedulingState {
        self.active
            .as_ref()
            .expect("detached task must own its active scheduling state")
    }

    pub(in crate::system) const fn active_option(&self) -> Option<&ActiveSchedulingState> {
        self.active.as_ref()
    }

    pub(in crate::system) fn active_mut(&mut self) -> &mut ActiveSchedulingState {
        self.active
            .as_mut()
            .expect("detached task must own its active scheduling state")
    }

    pub(in crate::system) fn take_active(&mut self) -> ActiveSchedulingState {
        self.active
            .take()
            .expect("active scheduling state must have exactly one owner")
    }

    pub(in crate::system) fn install_active(&mut self, active: ActiveSchedulingState) {
        assert!(
            self.active.replace(active).is_none(),
            "active scheduling state cannot have two owners"
        );
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(in crate::system) const fn owns_active(&self) -> bool {
        self.active.is_some()
    }
}
