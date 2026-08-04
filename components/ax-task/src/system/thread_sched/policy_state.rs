use super::*;

/// Requested, owner-applied, and PI-effective scheduling policy state.
///
/// Remote control paths update `requested` and `generation`. The runqueue
/// owner advances `applied_generation` and rebuilds `base_entity`; PI
/// propagation derives `effective` and `effective_entity` from that base.
#[derive(Debug)]
pub(in crate::system) struct ThreadPolicyState {
    pub(in crate::system) requested: SchedulePolicy,
    pub(in crate::system) applied: SchedulePolicy,
    pub(in crate::system) effective: SchedulePolicy,
    pub(in crate::system) generation: u64,
    pub(in crate::system) applied_generation: u64,
    pub(in crate::system) dispatch_generation: u64,
    pub(in crate::system) effective_entity: SchedulingEntity,
    pub(in crate::system) base_entity: SchedulingEntity,
}

impl ThreadPolicyState {
    pub(super) const fn new(policy: SchedulePolicy, entity: SchedulingEntity) -> Self {
        Self {
            requested: policy,
            applied: policy,
            effective: policy,
            generation: 1,
            applied_generation: 1,
            dispatch_generation: 1,
            effective_entity: entity,
            base_entity: entity,
        }
    }
}
