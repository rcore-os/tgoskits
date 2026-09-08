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
#[derive(Debug)]
pub(in crate::system) struct ThreadPolicyState {
    pub(in crate::system) base: SchedulePolicy,
    update_generation: u64,
    pending: Option<PendingPolicyUpdate>,
    pub(in crate::system) dispatch_generation: u64,
}

impl ThreadPolicyState {
    pub(super) const fn new(policy: SchedulePolicy) -> Self {
        Self {
            base: policy,
            update_generation: 1,
            pending: None,
            dispatch_generation: 1,
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
}
