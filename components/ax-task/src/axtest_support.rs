//! Focused kernel-test entry points for cross-module ownership regressions.

use alloc::sync::Arc;

use crate::{
    ActiveSchedulingState, CpuId, RqTaskMetadata, SchedulePolicy, SchedulingEntity, TaskSystem,
    TaskSystemConfig, ThreadCore, ThreadId,
    system::{
        CpuRunQueueState, CurrentClassState, CurrentDispatch, CurrentDispatchState, RqTaskTime,
        ThreadSchedCell,
    },
};

/// Observes whether a FIFO handoff derives a task-local RT-quota clockevent.
#[doc(hidden)]
pub fn axtest_fifo_switch_rt_deadline() -> (bool, u64, u64) {
    let config = TaskSystemConfig::new(1).with_rt_bandwidth(1_000, 950);
    let system = TaskSystem::new(config).expect("test task system must be valid");
    let cpu = system
        .create_cpu_local(CpuId::new(0))
        .expect("test CPU-local scheduler must be created");

    cpu.as_ref()
        .get_ref()
        .exercise_fifo_switch_rt_deadline_for_test()
}

/// Observes wake residue left behind by waking a runnable thread.
#[doc(hidden)]
pub fn axtest_runnable_wake_park_cleanliness() -> (bool, bool) {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).expect("test task system must be valid");
    system.exercise_runnable_wake_park_cleanliness_for_test()
}

/// Observes root-domain visibility of a throttled RT runqueue.
#[doc(hidden)]
pub fn axtest_throttled_rt_rq_overload_publication() -> (bool, bool) {
    TaskSystem::throttled_rt_rq_keeps_overload_publication()
}

/// Observes clock sampling when an active root RT period is activated again.
#[doc(hidden)]
pub fn axtest_active_rt_bandwidth_reactivation_clock_samples() -> (usize, bool, bool) {
    crate::scheduler::exercise_active_rt_bandwidth_reactivation_clock_samples()
}

/// Observes which RR task an ordinary runtime update leaves at the class head.
#[doc(hidden)]
pub fn axtest_rr_runtime_update_outcome() -> (u64, u64, u64, bool, u64, bool) {
    let (current, peer, update_next, update_request, tick_next, tick_request) =
        CpuRunQueueState::exercise_rr_runtime_update_for_test();
    (
        current.as_u64(),
        peer.as_u64(),
        update_next.as_u64(),
        update_request,
        tick_next.as_u64(),
        tick_request,
    )
}

/// Observes committed runtime before and after ending one rq-owned interval.
#[doc(hidden)]
pub fn axtest_runtime_interval_commit_samples() -> (u64, u64, u64) {
    const CHARGE_NS: u64 = 10;

    let thread = ThreadId::from_parts(0, 1);
    let policy = SchedulePolicy::default();
    let sched = Arc::new(ThreadSchedCell::new_test(thread, policy));
    let core = Arc::new(ThreadCore::new(
        thread, policy, sched, None, None, None, None,
    ));
    let active = ActiveSchedulingState::new(policy, SchedulingEntity::new(policy, 1, 0));
    let mut dispatch = CurrentDispatch::new(
        CurrentDispatchState {
            thread,
            schedule: CurrentClassState::Owned(active),
            metadata: RqTaskMetadata::test(1),
            rt_quota_exempt: false,
        },
        &core,
        RqTaskTime::test(0),
    );
    let initial = core.runtime_committed_ns_for_test();

    let _charge = dispatch.charge(CHARGE_NS, CHARGE_NS, 0);
    let while_running = core.runtime_committed_ns_for_test();
    dispatch.finish_runtime_interval();
    let after_switch_out = core.runtime_committed_ns_for_test();

    (initial, while_running, after_switch_out)
}

/// Observes Fair virtual time around migration of an unrelated RT entity.
#[doc(hidden)]
pub fn axtest_realtime_migration_fair_virtual_time() -> (u64, u64) {
    crate::scheduler::exercise_realtime_migration_fair_virtual_time()
}

/// Checks that persistent overload does not synthesize no-switch balance work.
#[doc(hidden)]
pub fn axtest_no_switch_ignores_persistent_rt_overload() -> bool {
    crate::TaskSystem::no_switch_ignores_persistent_rt_overload()
}

/// Checks that an overload level alone does not synthesize push work.
#[doc(hidden)]
pub fn axtest_schedule_selection_ignores_persistent_rt_overload() -> bool {
    crate::TaskSystem::schedule_selection_ignores_persistent_rt_overload()
}

/// Observes root-domain push generations across an RT priority drop with no overload source.
#[doc(hidden)]
pub fn axtest_priority_drop_without_overload_push_generations() -> (u64, u64) {
    crate::TaskSystem::priority_drop_without_overload_push_generations()
}

/// Counts root-domain state locks taken by a clean push-target query.
#[doc(hidden)]
pub fn axtest_clean_push_target_query_lock_acquisitions() -> (bool, usize) {
    crate::TaskSystem::clean_push_target_query_lock_acquisitions()
}

/// Observes whether an idle entry with no pull source requests balance work.
#[doc(hidden)]
pub fn axtest_empty_idle_entry_balance_pending() -> bool {
    crate::TaskSystem::empty_idle_entry_balance_pending()
}

/// Checks that a clean same-task yield keeps its existing timer publication.
#[doc(hidden)]
pub fn axtest_lone_yield_reuses_scheduler_deadline() -> bool {
    crate::TaskSystem::lone_yield_reuses_scheduler_deadline()
}

/// Checks that balance callbacks do not break the cross-switch rq lock baton.
#[doc(hidden)]
pub fn axtest_balance_callback_preserves_owner_rq_baton() -> bool {
    crate::TaskSystem::balance_callback_preserves_owner_rq_baton()
}

/// Counts RT active/pushable iterator visits for one pinned enqueue/dequeue.
#[doc(hidden)]
pub fn axtest_pinned_realtime_membership_visits() -> (usize, usize, usize, usize) {
    crate::scheduler::exercise_pinned_realtime_membership_visits()
}

/// Observes delayed-wake lag after Linux-style dequeue/place/reinsert.
#[doc(hidden)]
pub fn axtest_delayed_wake_linux_lag_after_requeue_placement() -> (i64, usize, usize, u64) {
    crate::scheduler::exercise_delayed_wake_linux_lag_after_requeue_placement()
}

/// Observes placement across Linux's shared Normal/Batch CFS average.
#[doc(hidden)]
pub fn axtest_normal_and_batch_linux_cfs_placement_weight() -> (u64, u64, u64, i64) {
    crate::scheduler::exercise_normal_and_batch_linux_cfs_placement_weight()
}

/// Checks that a Linux `SCHED_IDLE` wakee never requests wakeup preemption.
#[doc(hidden)]
pub fn axtest_idle_wakee_does_not_preempt_idle_current() -> bool {
    let current = SchedulingEntity::Fair(crate::FairEntity::test_state(
        crate::Nice::ZERO,
        crate::FairMode::Idle,
        3_000,
        3_100,
    ));
    let wakee = SchedulingEntity::Fair(crate::FairEntity::test_state(
        crate::Nice::ZERO,
        crate::FairMode::Idle,
        1_000,
        3_500,
    ));
    let idle_policy = SchedulePolicy::fair(crate::Nice::ZERO, crate::FairMode::Idle);

    !crate::scheduler::wakeup_preempts(idle_policy, &current, false, idle_policy, &wakee, 2_000)
}

/// Observes whether switch-in/out publish `on_cpu` with stores or RMWs.
#[doc(hidden)]
pub fn axtest_on_cpu_publication_kinds() -> (u64, u64, u64, u64) {
    crate::system::exercise_on_cpu_publication_kinds()
}
