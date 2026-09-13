//! Owner scheduling entry points, runtime charging, and load balancing requests.

use super::{
    dispatch::OwnerDispatchCommit,
    switch::{OwnerRqScheduleOut, OwnerRqScheduledOut},
    *,
};
use crate::sched::{
    algorithm::{RtEligibility, SchedulerClass},
    system::cpu::{PreviousSwitchDisposition, PreviousSwitchOwnership, SchedulerRequestClaim},
};

fn realtime_current_remains_selected(
    transaction: &mut OwnerRqTxn<'_>,
    current_policy: SchedulePolicy,
) -> bool {
    if transaction.rt_is_effectively_throttled() {
        return false;
    }
    // rq->nr_running already includes a linked RT current. With no other
    // runnable entity, the class chain must select that current again; only
    // RT throttling can make idle win. Avoid rediscovering the same fact
    // through the priority bitmap and per-priority list length.
    if transaction.nr_queued() == 0 {
        return true;
    }
    let Some(priority) = current_policy.rt_priority().map(|priority| priority.get()) else {
        return false;
    };
    if transaction.highest_rt_priority() != Some(priority)
        || transaction.rt_count_at_priority(priority) != 1
    {
        return false;
    }

    // The unique highest-priority RT node is current. Linux next checks the
    // static class chain: only queued Stop or eligible Deadline work can
    // displace it. Do not mutate and roll back class queues merely to prove
    // that no higher class is selectable.
    !transaction.has_selectable_higher_class(SchedulerClass::Realtime, RtEligibility::Runnable)
}

fn owner_yield_kept_class(
    transaction: &mut OwnerRqTxn<'_>,
    current_policy: SchedulePolicy,
) -> Option<SchedulerClass> {
    let class = SchedulerClass::for_policy(current_policy);
    let keeps_current = match class {
        SchedulerClass::Fair => transaction.nr_queued() == 0,
        SchedulerClass::Realtime => {
            // Linux reads `rq->curr->sched_class` directly. The effective
            // policy belongs to the same rq-owned current dispatch, so class
            // selection must not rediscover its entity through the queued
            // generation index.
            realtime_current_remains_selected(transaction, current_policy)
        }
        SchedulerClass::Stop | SchedulerClass::Deadline => false,
    };
    if keeps_current { Some(class) } else { None }
}

struct RequestedPreemptionCommit {
    decision: ScheduleDecision,
    previous_urgency: Option<SchedulingUrgency>,
    next_urgency: SchedulingUrgency,
    dispatch: OwnerDispatchCommit,
    deadline_rq_observation: SchedulerDeadlineRqObservation,
}

struct RequestedPreemptionState {
    previous: Option<ThreadId>,
    previous_core: Option<PreviousSwitchOwnership>,
    previous_endpoint: Option<SwitchEndpoint>,
    previous_urgency: Option<SchedulingUrgency>,
    dispatch: OwnerDispatchCommit,
    migration: Option<PreparedMigrationDelivery>,
    now_ns: u64,
}

mod preemption;

mod balance;

mod accounting;

mod selection;

mod yield_entry;

mod yield_control;

mod yield_run_queue;
