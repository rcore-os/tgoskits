use super::{membership::SequenceAllocationError, *};
use crate::{
    CurrentClassState, CurrentDispatch, CurrentDispatchState, CurrentSchedule, DeadlineFlags,
    DeadlinePolicy, FairEntity, FairMode, Nice, RqTaskTime, RtPriority,
};

fn pick_next(queue: &mut RunQueue, eligibility: RtEligibility) -> PickedThread {
    let picked = queue.pick_next_task(eligibility).unwrap();
    queue.set_next_task(&picked);
    picked
}

fn pick_linked_current(queue: &mut RunQueue) -> ThreadId {
    let picked = queue.pick_next_task(RtEligibility::Runnable).unwrap();
    assert!(
        matches!(picked, PickedThread::Linked(_)),
        "only RT and Deadline retain their running entity in the class structure"
    );
    queue.set_next_task(&picked);
    let metadata = picked.metadata().clone();
    let core = Arc::clone(picked.core());
    let thread = picked.id();
    let dispatch = CurrentDispatch::new(
        CurrentDispatchState {
            thread,
            schedule: CurrentClassState::Linked {
                policy: picked.policy(),
            },
            metadata,
            rt_quota_exempt: picked.rt_quota_exempt(),
        },
        &core,
        RqTaskTime::test(0),
    );
    queue.install_current(dispatch);
    thread
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn fair_wakeup_preemption_requires_the_wakee_to_be_the_eevdf_pick() {
    let mut queue = RunQueue::new();
    let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let contender = ThreadId::from_parts(1, 1);
    let wakee = ThreadId::from_parts(2, 1);
    let contender_entity = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 900, 1_000);
    let wakee_entity = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 950, 1_100);
    queue
        .enqueue_test(
            contender,
            policy,
            SchedulingEntity::Fair(contender_entity),
            0,
            EnqueueReason::Preempted,
        )
        .unwrap();
    queue
        .enqueue_test(
            wakee,
            policy,
            SchedulingEntity::Fair(wakee_entity),
            0,
            EnqueueReason::Preempted,
        )
        .unwrap();
    let mut current_entity = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 1_000, 1_200);
    assert!(current_entity.charge(1, 0));
    queue.update_fair_virtual_time(Some(current_entity));
    let virtual_time = queue.virtual_time_for_mode(FairMode::Normal);
    let current = CurrentSchedule::test_state(policy, SchedulingEntity::Fair(current_entity));

    assert!(queue.fair_wakee_is_selected(contender, FairMode::Normal, virtual_time));
    assert!(!queue.fair_wakee_is_selected(wakee, FairMode::Normal, virtual_time));
    assert!(wakee_entity.deadline_precedes(current_entity));
    let preempts_current =
        current.should_preempt(policy, SchedulingEntity::Fair(wakee_entity), virtual_time)
            && queue.fair_wakee_is_selected(wakee, FairMode::Normal, virtual_time);
    assert!(
        !preempts_current,
        "a wakee that loses the full EEVDF pick must not request preemption",
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn deadline_precedes_rt_and_fair() {
    let mut queue = RunQueue::new();
    let fair = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let rt = SchedulePolicy::fifo(RtPriority::new(99).unwrap());
    let deadline =
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap());
    queue
        .enqueue_test(
            ThreadId::from_parts(0, 1),
            fair,
            SchedulingEntity::new(fair, 1, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();
    queue
        .enqueue_test(
            ThreadId::from_parts(1, 1),
            rt,
            SchedulingEntity::new(rt, 1, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();
    let mut deadline_entity = SchedulingEntity::new(deadline, 1, 0);
    deadline_entity.activate_deadline(0);
    queue
        .enqueue_test(
            ThreadId::from_parts(2, 1),
            deadline,
            deadline_entity,
            0,
            EnqueueReason::Wake,
        )
        .unwrap();
    assert_eq!(
        pick_next(&mut queue, RtEligibility::Runnable).id(),
        ThreadId::from_parts(2, 1)
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn deadline_runqueue_orders_across_linux_rq_clock_wrap() {
    let mut queue = RunQueue::new();
    let earlier_policy =
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 4, 20, DeadlineFlags::NONE).unwrap());
    let later_policy =
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 10, 20, DeadlineFlags::NONE).unwrap());
    let earlier_id = ThreadId::from_parts(0, 1);
    let later_id = ThreadId::from_parts(1, 1);
    let now = u64::MAX - 5;

    for (id, policy) in [(later_id, later_policy), (earlier_id, earlier_policy)] {
        let mut entity = SchedulingEntity::new(policy, 1, 0);
        entity.activate_deadline(now);
        queue
            .enqueue_test(id, policy, entity, now, EnqueueReason::Wake)
            .unwrap();
    }

    assert_eq!(
        pick_next(&mut queue, RtEligibility::Runnable).id(),
        earlier_id
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn kernel_stopper_runs_before_deadline_even_when_rt_is_throttled() {
    let mut queue = RunQueue::new();
    let stopper = SchedulePolicy::kernel_stop();
    let deadline =
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap());
    let mut deadline_entity = SchedulingEntity::new(deadline, 1, 0);
    deadline_entity.activate_deadline(0);
    queue
        .enqueue_test(
            ThreadId::from_parts(0, 1),
            deadline,
            deadline_entity,
            0,
            EnqueueReason::Wake,
        )
        .unwrap();
    queue
        .enqueue_test(
            ThreadId::from_parts(1, 1),
            stopper,
            SchedulingEntity::new(stopper, 1, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();

    assert_eq!(
        pick_next(&mut queue, RtEligibility::Throttled).id(),
        ThreadId::from_parts(1, 1),
        "stopper work must bypass ordinary RT bandwidth throttling"
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn kernel_stopper_does_not_enter_the_realtime_priority_array() {
    let mut queue = RunQueue::new();
    let stopper = SchedulePolicy::kernel_stop();
    queue
        .enqueue_test(
            ThreadId::from_parts(1, 1),
            stopper,
            SchedulingEntity::new(stopper, 1, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();

    assert_eq!(queue.rt.count_at_priority(100), 0);
    assert_eq!(queue.placement_demand(), 0);
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn kernel_stopper_preempts_all_user_sched_classes() {
    let stopper = SchedulePolicy::kernel_stop();
    let stopper_entity = SchedulingEntity::new(stopper, 1, 0);
    for policy in [
        SchedulePolicy::default(),
        SchedulePolicy::fifo(RtPriority::new(99).unwrap()),
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap()),
    ] {
        let mut entity = SchedulingEntity::new(policy, 1, 0);
        entity.activate_deadline(0);
        let current = CurrentSchedule::test_state(policy, entity);
        assert!(current.should_preempt(stopper, stopper_entity.clone(), 0));
    }

    let current = CurrentSchedule::test_state(stopper, stopper_entity);
    assert!(!current.should_preempt(
        SchedulePolicy::default(),
        SchedulingEntity::new(SchedulePolicy::default(), 1, 0),
        0
    ));
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn fifo_preemption_preserves_the_head_position() {
    let mut queue = RunQueue::new();
    let policy = SchedulePolicy::fifo(RtPriority::new(10).unwrap());
    for slot in [1, 2] {
        queue
            .enqueue_test(
                ThreadId::from_parts(slot, 1),
                policy,
                SchedulingEntity::new(policy, 1, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();
    }
    queue
        .enqueue_test(
            ThreadId::from_parts(0, 1),
            policy,
            SchedulingEntity::new(policy, 1, 0),
            0,
            EnqueueReason::Preempted,
        )
        .unwrap();
    assert_eq!(
        pick_next(&mut queue, RtEligibility::Runnable).id(),
        ThreadId::from_parts(0, 1)
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn lone_round_robin_quantum_does_not_request_reschedule() {
    let mut queue = RunQueue::new();
    let priority = RtPriority::new(10).unwrap();
    let policy = SchedulePolicy::round_robin_with_quantum(priority, 100).unwrap();
    let thread = ThreadId::from_parts(0, 1);
    queue
        .enqueue_test(
            thread,
            policy,
            SchedulingEntity::new(policy, 1, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();
    assert_eq!(pick_linked_current(&mut queue), thread);

    let (charge, ..) = queue.charge_current(100, 100, 0, 0, 1, 0).unwrap();
    let tick = SchedulerClass::Realtime.task_tick(&mut queue, thread, policy, charge);

    assert!(
        !tick.request_reschedule,
        "Linux refreshes a lone RR task's quantum without rescheduling it"
    );
    assert!(
        !queue
            .rt
            .get(priority.get(), thread)
            .unwrap()
            .entity()
            .round_robin_quantum_expired(),
        "the lone RR task must start a fresh quantum in place"
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn competing_round_robin_quantum_rotates_the_active_queue() {
    let mut queue = RunQueue::new();
    let priority = RtPriority::new(10).unwrap();
    let policy = SchedulePolicy::round_robin_with_quantum(priority, 100).unwrap();
    let current = ThreadId::from_parts(0, 1);
    let peer = ThreadId::from_parts(1, 1);
    for thread in [current, peer] {
        queue
            .enqueue_test(
                thread,
                policy,
                SchedulingEntity::new(policy, 1, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();
    }
    assert_eq!(pick_linked_current(&mut queue), current);

    let (charge, ..) = queue.charge_current(100, 100, 0, 0, 1, 0).unwrap();
    let tick = SchedulerClass::Realtime.task_tick(&mut queue, current, policy, charge);

    assert!(tick.request_reschedule);
    assert_eq!(
        queue.rt.select().unwrap().id,
        peer,
        "Linux requeues an expired RR current behind its same-priority peer"
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn first_fair_placement_cannot_start_behind_runqueue_virtual_time() {
    let mut queue = RunQueue::new();
    queue.set_virtual_time_for_test(10_000);
    let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let thread = ThreadId::from_parts(0, 1);

    queue
        .enqueue_test(
            thread,
            policy,
            SchedulingEntity::new(policy, 1_000, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();

    let entity = queue.dequeue(thread).unwrap().entity().fair().unwrap();
    assert_eq!(entity.vruntime(), 10_000);
    assert_eq!(entity.virtual_deadline(), 10_500);
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn fair_preemption_preserves_positive_lag_and_active_deadline() {
    let mut queue = RunQueue::new();
    queue.set_virtual_time_for_test(1_000);
    let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let thread = ThreadId::from_parts(0, 1);

    queue
        .enqueue_test(
            thread,
            policy,
            SchedulingEntity::Fair(FairEntity::test_state(
                Nice::ZERO,
                FairMode::Normal,
                900,
                950,
            )),
            0,
            EnqueueReason::Preempted,
        )
        .unwrap();

    let entity = queue.dequeue(thread).unwrap().entity().fair().unwrap();
    assert_eq!(
        (entity.vruntime(), entity.virtual_deadline()),
        (900, 950),
        "a same-rq preemption must not erase the current EEVDF request's lag"
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn fair_migration_preserves_positive_lag_and_active_deadline() {
    let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let migrating = ThreadId::from_parts(0, 1);
    let peer = ThreadId::from_parts(1, 1);
    let mut source = RunQueue::new();
    source
        .enqueue_test(
            migrating,
            policy,
            SchedulingEntity::Fair(FairEntity::test_state(
                Nice::ZERO,
                FairMode::Normal,
                900,
                950,
            )),
            0,
            EnqueueReason::Preempted,
        )
        .unwrap();
    source
        .enqueue_test(
            peer,
            policy,
            SchedulingEntity::Fair(FairEntity::test_state(
                Nice::ZERO,
                FairMode::Normal,
                1_100,
                1_200,
            )),
            0,
            EnqueueReason::Preempted,
        )
        .unwrap();
    let detached = source
        .detach_for_transfer(migrating, None, 500_000)
        .unwrap();

    let mut destination = RunQueue::new();
    destination.set_virtual_time_for_test(2_000);
    destination
        .enqueue_test(
            peer,
            policy,
            SchedulingEntity::Fair(FairEntity::test_state(
                Nice::ZERO,
                FairMode::Normal,
                2_000,
                2_100,
            )),
            0,
            EnqueueReason::Preempted,
        )
        .unwrap();
    destination
        .enqueue_task(detached, EnqueueReason::Migrated, None)
        .unwrap();

    let entity = destination
        .dequeue(migrating)
        .unwrap()
        .entity()
        .fair()
        .unwrap();
    assert_eq!(
        (entity.vruntime(), entity.virtual_deadline()),
        (1_800, 1_850),
        "migration must restore source vlag and relative deadline on the destination rq"
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn fair_yield_forfeits_request_before_positive_lag_peer() {
    let mut queue = RunQueue::new();
    let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let yielding = ThreadId::from_parts(0, 1);
    let waiting = ThreadId::from_parts(1, 1);

    queue
        .enqueue_test(
            waiting,
            policy,
            SchedulingEntity::new(policy, 100, 100),
            0,
            EnqueueReason::Migrated,
        )
        .unwrap();
    queue
        .enqueue_test(
            yielding,
            policy,
            SchedulingEntity::new(policy, 100, 0),
            0,
            EnqueueReason::Yield,
        )
        .unwrap();

    assert_eq!(
        pick_next(&mut queue, RtEligibility::Runnable).id(),
        waiting,
        "yield must forfeit the active request so positive-lag peers become eligible",
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn weighted_virtual_time_makes_every_non_negative_lag_entity_eligible() {
    let mut queue = RunQueue::new();
    let low_weight = SchedulePolicy::fair(Nice::new(19).unwrap(), FairMode::Normal);
    let normal_weight = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    for (slot, policy, vruntime, deadline) in [
        (0, low_weight, 0, 100),
        (1, normal_weight, 4, 8),
        (2, normal_weight, 10, 20),
    ] {
        let SchedulePolicy::Fair { nice, mode } = policy else {
            unreachable!();
        };
        queue
            .enqueue_test(
                ThreadId::from_parts(slot, 1),
                policy,
                SchedulingEntity::Fair(FairEntity::test_state(nice, mode, vruntime, deadline)),
                0,
                EnqueueReason::Migrated,
            )
            .unwrap();
    }

    assert_eq!(
        pick_next(&mut queue, RtEligibility::Runnable).id(),
        ThreadId::from_parts(1, 1),
        "weighted V must make both vruntime 0 and 4 eligible, then choose vd=8",
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn fair_deadline_order_survives_virtual_time_wrap() {
    let mut queue = RunQueue::new();
    let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let virtual_time = u64::MAX - 100;
    queue.set_virtual_time_for_test(virtual_time);
    let later = ThreadId::from_parts(0, 1);
    let earlier = ThreadId::from_parts(1, 1);

    queue
        .enqueue_test(
            later,
            policy,
            SchedulingEntity::new(policy, 200, virtual_time),
            0,
            EnqueueReason::Migrated,
        )
        .unwrap();
    queue
        .enqueue_test(
            earlier,
            policy,
            SchedulingEntity::new(policy, 110, virtual_time),
            0,
            EnqueueReason::Migrated,
        )
        .unwrap();

    assert_eq!(
        pick_next(&mut queue, RtEligibility::Runnable).id(),
        earlier,
        "EEVDF must order wrapped virtual deadlines by signed distance",
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn fair_weighted_virtual_time_includes_current_across_wrap() {
    let mut queue = RunQueue::new();
    let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let before_wrap = u64::MAX - 100;
    queue.set_virtual_time_for_test(before_wrap);
    queue
        .enqueue_test(
            ThreadId::from_parts(0, 1),
            policy,
            SchedulingEntity::Fair(FairEntity::test_state(
                Nice::ZERO,
                FairMode::Normal,
                before_wrap,
                before_wrap.wrapping_add(100),
            )),
            0,
            EnqueueReason::Migrated,
        )
        .unwrap();

    let current =
        FairEntity::test_state(Nice::ZERO, FairMode::Normal, 20, 20_u64.wrapping_add(100));
    queue.update_fair_virtual_time(Some(current));

    assert_eq!(
        queue.virtual_time(),
        u64::MAX - 40,
        "the owner-rq mean must use signed deltas and include the running entity",
    );
    queue.fair.assert_invariants();
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn fair_pushable_summary_uses_wrapped_runqueue_order() {
    let mut queue = RunQueue::new();
    let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let virtual_time = u64::MAX - 10;
    queue.set_virtual_time_for_test(virtual_time);
    for (slot, deadline) in [(0, 5), (1, u64::MAX - 1)] {
        queue
            .enqueue_test(
                ThreadId::from_parts(slot, 1),
                policy,
                SchedulingEntity::Fair(FairEntity::test_state(
                    Nice::ZERO,
                    FairMode::Normal,
                    virtual_time,
                    deadline,
                )),
                0,
                EnqueueReason::Migrated,
            )
            .unwrap();
    }

    assert!(queue.has_pushable_fair());
    let mut scan = queue.begin_balance_scan(Some(SchedulingClass::Fair));
    assert_eq!(
        queue
            .next_balance_candidate(&mut scan, |_| true)
            .expect("one Fair candidate must be movable")
            .id,
        ThreadId::from_parts(1, 1),
        "the Fair class must retain the owner runqueue's modular EEVDF order",
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn deadline_preemption_does_not_reapply_the_cbs_wake_rule() {
    let mut queue = RunQueue::new();
    let policy =
        SchedulePolicy::deadline(DeadlinePolicy::new(4, 8, 10, DeadlineFlags::NONE).unwrap());
    let thread = ThreadId::from_parts(0, 1);
    let mut entity = SchedulingEntity::new(policy, 1, 0);
    entity.activate_deadline(0);
    assert!(!entity.charge(1, 0, 0));

    queue
        .enqueue_test(thread, policy, entity, 4, EnqueueReason::Preempted)
        .unwrap();

    let dequeued = queue.dequeue(thread).unwrap();
    let entity = dequeued.entity();
    let deadline = entity.deadline().unwrap();
    assert_eq!(deadline.absolute_deadline_ns(), Some(8));
    assert_eq!(deadline.remaining_runtime_ns(), 3);
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn pushable_membership_tracks_each_non_idle_scheduler_class() {
    let mut queue = RunQueue::new();
    let idle = SchedulePolicy::fair(Nice::ZERO, FairMode::Idle);
    let fair = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let rt = SchedulePolicy::fifo(RtPriority::new(80).unwrap());
    let deadline =
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap());
    let idle_id = ThreadId::from_parts(0, 1);
    let fair_id = ThreadId::from_parts(1, 1);
    let rt_id = ThreadId::from_parts(2, 1);
    let deadline_id = ThreadId::from_parts(3, 1);

    queue
        .enqueue_test(
            idle_id,
            idle,
            SchedulingEntity::new(idle, 1, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();
    assert!(!queue.has_pushable_deadline());
    assert!(!queue.has_pushable_realtime());
    assert!(!queue.has_pushable_fair());
    for (id, policy) in [(fair_id, fair), (rt_id, rt), (deadline_id, deadline)] {
        let mut entity = SchedulingEntity::new(policy, 1, 0);
        if matches!(policy, SchedulePolicy::Deadline(_)) {
            entity.activate_deadline(0);
        }
        queue
            .enqueue_test(id, policy, entity, 0, EnqueueReason::Wake)
            .unwrap();
    }
    assert!(queue.has_pushable_deadline());
    assert!(queue.has_pushable_realtime());
    assert!(queue.has_pushable_fair());

    queue.dequeue(deadline_id).unwrap();
    assert!(!queue.has_pushable_deadline());
    assert!(queue.has_pushable_realtime());
    assert_eq!(pick_next(&mut queue, RtEligibility::Runnable).id(), rt_id);
    assert!(!queue.has_pushable_realtime());
    assert!(queue.has_pushable_fair());
    queue.dequeue(fair_id).unwrap();
    assert!(!queue.has_pushable_fair());
    assert_eq!(queue.dequeue(idle_id).unwrap().id, idle_id);
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn realtime_pushable_selection_does_not_rescan_the_active_fifo() {
    let mut queue = RunQueue::new();
    let policy = SchedulePolicy::fifo(RtPriority::new(80).unwrap());
    for slot in 0..64 {
        queue
            .enqueue_test(
                ThreadId::from_parts(slot, 1),
                policy,
                SchedulingEntity::new(policy, 1_000, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();
    }

    super::realtime::reset_realtime_queue_visits();
    let mut scan = queue.begin_balance_scan(Some(SchedulingClass::Realtime));
    let candidate = queue
        .next_balance_candidate(&mut scan, |thread| thread.id != ThreadId::from_parts(0, 1))
        .expect("the second RT task must remain pushable");

    assert_eq!(candidate.id, ThreadId::from_parts(1, 1));
    assert_eq!(
        super::realtime::realtime_pushable_iter_visits(),
        2,
        "rejecting the first candidate should inspect two pushable links"
    );
    assert_eq!(
        super::realtime::realtime_active_iter_visits(),
        0,
        "pushable selection must not rescan the active RT FIFO"
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn balance_scan_does_not_expand_past_its_entry_candidate_set() {
    let mut queue = RunQueue::new();
    let policy = SchedulePolicy::fifo(RtPriority::new(80).unwrap());
    let first = ThreadId::from_parts(0, 1);
    queue
        .enqueue_test(
            first,
            policy,
            SchedulingEntity::new(policy, 1_000, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();

    let mut scan = queue.begin_balance_scan(Some(SchedulingClass::Realtime));
    assert_eq!(
        queue
            .next_balance_candidate(&mut scan, |_| true)
            .unwrap()
            .id,
        first
    );

    let arrived_after_entry = ThreadId::from_parts(1, 1);
    queue
        .enqueue_test(
            arrived_after_entry,
            policy,
            SchedulingEntity::new(policy, 1_000, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();
    assert!(
        queue.next_balance_candidate(&mut scan, |_| true).is_none(),
        "one owner safe-point must be bounded by its entry candidate count"
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn rt_and_deadline_pushable_membership_tracks_migration_capability() {
    let mut queue = RunQueue::new();
    let rt = SchedulePolicy::fifo(RtPriority::new(80).unwrap());
    let deadline =
        SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 4, DeadlineFlags::NONE).unwrap());
    let rt_id = ThreadId::from_parts(0, 1);
    let deadline_id = ThreadId::from_parts(1, 1);
    let mut deadline_entity = SchedulingEntity::new(deadline, 1, 0);
    deadline_entity.activate_deadline(0);
    queue
        .enqueue_test(
            rt_id,
            rt,
            SchedulingEntity::new(rt, 1, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();
    queue
        .enqueue_test(
            deadline_id,
            deadline,
            deadline_entity,
            0,
            EnqueueReason::Wake,
        )
        .unwrap();

    for id in [rt_id, deadline_id] {
        assert!(queue.update_migration_capability(id, false));
    }
    assert!(!queue.has_pushable_realtime());
    assert!(!queue.has_pushable_deadline());

    for id in [rt_id, deadline_id] {
        assert!(queue.update_migration_capability(id, true));
    }
    assert!(queue.has_pushable_realtime());
    assert!(queue.has_pushable_deadline());
    let mut scan = queue.begin_balance_scan(None);
    assert_eq!(
        queue
            .next_balance_candidate(&mut scan, |_| true)
            .unwrap()
            .id,
        deadline_id
    );
    assert_eq!(
        queue
            .next_balance_candidate(&mut scan, |_| true)
            .unwrap()
            .id,
        rt_id
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn realtime_put_prev_restores_a_box_stable_pushable_link() {
    let mut queue = RunQueue::new();
    let policy = SchedulePolicy::fifo(RtPriority::new(80).unwrap());
    let running = ThreadId::from_parts(0, 1);
    queue
        .enqueue_test(
            running,
            policy,
            SchedulingEntity::new(policy, 1, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();

    assert_eq!(pick_linked_current(&mut queue), running);
    assert!(!queue.has_pushable_realtime());
    queue.put_prev_task(running, EnqueueReason::Yield).unwrap();
    assert!(queue.has_pushable_realtime());
    let mut scan = queue.begin_balance_scan(Some(SchedulingClass::Realtime));
    assert_eq!(
        queue
            .next_balance_candidate(&mut scan, |_| true)
            .unwrap()
            .id,
        running
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn realtime_pushable_storage_is_reusable_after_dequeue() {
    let mut queue = RunQueue::new();
    let policy = SchedulePolicy::fifo(RtPriority::new(80).unwrap());
    let thread = ThreadId::from_parts(0, 1);
    queue
        .enqueue_test(
            thread,
            policy,
            SchedulingEntity::new(policy, 1, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();

    let detached = queue.dequeue(thread).unwrap();
    assert!(!queue.has_pushable_realtime());
    queue
        .enqueue_task(detached, EnqueueReason::Wake, None)
        .unwrap();
    assert!(queue.has_pushable_realtime());
    assert_eq!(queue.dequeue(thread).unwrap().id, thread);
    assert!(!queue.has_pushable_realtime());
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn fair_virtual_time_and_pick_do_not_scan_the_runnable_set() {
    let mut queue = RunQueue::new();
    let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    for slot in 0..128 {
        queue
            .enqueue_test(
                ThreadId::from_parts(slot, 1),
                policy,
                SchedulingEntity::new(policy, 1_000, slot as u64),
                0,
                EnqueueReason::Migrated,
            )
            .unwrap();
    }

    reset_fair_runqueue_visits();
    queue.update_fair_virtual_time(None);
    assert_eq!(
        fair_runqueue_visits(),
        0,
        "weighted virtual time must come from incrementally maintained rq sums"
    );

    queue.fair.assert_invariants();
    while queue.has_fair() {
        reset_fair_runqueue_visits();
        pick_next(&mut queue, RtEligibility::Runnable);
        assert!(
            fair_runqueue_visits() <= 32,
            "EEVDF selection must remain logarithmic, observed {} visits",
            fair_runqueue_visits()
        );
        queue.fair.assert_invariants();
    }

    let mut removal_queue = RunQueue::new();
    for slot in 0..128 {
        removal_queue
            .enqueue_test(
                ThreadId::from_parts(slot, 1),
                policy,
                SchedulingEntity::new(policy, 1_000, slot as u64),
                0,
                EnqueueReason::Migrated,
            )
            .unwrap();
    }
    for index in 0..128 {
        let slot = (index * 73) % 128;
        removal_queue
            .dequeue(ThreadId::from_parts(slot, 1))
            .unwrap();
        removal_queue.fair.assert_invariants();
    }
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn fair_enqueue_uses_direct_runqueue_membership() {
    let mut queue = RunQueue::new();
    let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    queue
        .enqueue_test(
            ThreadId::from_parts(0, 1),
            policy,
            SchedulingEntity::new(policy, 1_000, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();

    reset_runqueue_membership_lookups();
    queue
        .enqueue_test(
            ThreadId::from_parts(1, 1),
            policy,
            SchedulingEntity::new(policy, 1_000, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();
    assert_eq!(
        runqueue_membership_lookups(),
        1,
        "enqueue must perform one generation-checked lookup instead of probing scheduler classes"
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn direct_membership_rejects_a_retired_thread_generation() {
    let mut queue = RunQueue::new();
    let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let retired = ThreadId::from_parts(7, 1);
    let replacement = ThreadId::from_parts(7, 2);

    queue
        .enqueue_test(
            retired,
            policy,
            SchedulingEntity::new(policy, 1_000, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();
    assert_eq!(queue.dequeue(retired).unwrap().id, retired);
    queue
        .enqueue_test(
            replacement,
            policy,
            SchedulingEntity::new(policy, 1_000, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();

    assert!(queue.dequeue(retired).is_none());
    assert_eq!(queue.dequeue(replacement).unwrap().id, replacement);
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn realtime_bitmap_tracks_the_highest_nonempty_priority() {
    let mut queue = RunQueue::new();
    let low = SchedulePolicy::fifo(RtPriority::new(1).unwrap());
    let high = SchedulePolicy::fifo(RtPriority::new(99).unwrap());
    let low_id = ThreadId::from_parts(0, 1);
    let high_id = ThreadId::from_parts(1, 1);
    for (id, policy) in [(low_id, low), (high_id, high)] {
        queue
            .enqueue_test(
                id,
                policy,
                SchedulingEntity::new(policy, 1_000, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();
    }

    assert_eq!(queue.highest_rt_priority(), Some(99));
    assert_eq!(queue.dequeue(high_id).unwrap().id, high_id);
    assert_eq!(queue.highest_rt_priority(), Some(1));
    assert_eq!(pick_next(&mut queue, RtEligibility::Runnable).id(), low_id);
    assert!(
        queue.has_rt(),
        "selected RT current remains represented in the active bitmap"
    );
    assert_eq!(queue.dequeue(low_id).unwrap().id, low_id);
    assert!(!queue.has_rt());
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn realtime_running_entity_remains_linked_in_the_active_array() {
    let mut queue = RunQueue::new();
    let policy = SchedulePolicy::fifo(RtPriority::new(10).unwrap());
    let running = ThreadId::from_parts(0, 1);
    queue
        .enqueue_test(
            running,
            policy,
            SchedulingEntity::new(policy, 1_000, 0),
            0,
            EnqueueReason::Wake,
        )
        .unwrap();

    assert_eq!(pick_linked_current(&mut queue), running);
    assert!(
        queue.contains(running),
        "Linux RT keeps current in the active priority array"
    );
    assert_eq!(queue.len(), 0, "current is not a queued balance candidate");
    assert!(!queue.has_pushable_realtime());
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn deadline_running_entity_remains_linked_in_the_active_tree() {
    let mut queue = RunQueue::new();
    let policy =
        SchedulePolicy::deadline(DeadlinePolicy::new(10, 20, 30, DeadlineFlags::NONE).unwrap());
    let running = ThreadId::from_parts(0, 1);
    let mut entity = SchedulingEntity::new(policy, 1, 0);
    entity.activate_deadline(0);
    queue
        .enqueue_test(running, policy, entity, 0, EnqueueReason::Wake)
        .unwrap();

    assert_eq!(pick_linked_current(&mut queue), running);
    assert!(
        queue.contains(running),
        "Linux Deadline keeps current in the active EDF tree"
    );
    assert_eq!(queue.len(), 0, "current is not a queued balance candidate");
    assert!(!queue.has_pushable_deadline());
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn rt_class_throttle_is_all_or_nothing() {
    let mut queue = RunQueue::new();
    let ordinary = ThreadId::from_parts(0, 1);
    let lower_pi_owner = ThreadId::from_parts(1, 1);
    let higher_pi_owner = ThreadId::from_parts(2, 1);
    queue
        .enqueue_rt_test(
            ordinary,
            SchedulePolicy::fifo(RtPriority::new(99).unwrap()),
            false,
        )
        .unwrap();
    queue
        .enqueue_rt_test(
            lower_pi_owner,
            SchedulePolicy::fifo(RtPriority::new(10).unwrap()),
            true,
        )
        .unwrap();
    queue
        .enqueue_rt_test(
            higher_pi_owner,
            SchedulePolicy::fifo(RtPriority::new(20).unwrap()),
            true,
        )
        .unwrap();

    assert!(
        queue.pick_next_task(RtEligibility::Throttled).is_none(),
        "Linux skips a throttled RT class only when no boosted entity keeps the rq runnable"
    );
    assert_eq!(
        pick_next(&mut queue, RtEligibility::Runnable).id(),
        ordinary,
        "one boosted entity makes the whole RT rq runnable at normal priority order"
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn deadline_pick_does_not_scan_the_runnable_set() {
    let mut queue = RunQueue::new();
    let policy =
        SchedulePolicy::deadline(DeadlinePolicy::new(10, 20, 30, DeadlineFlags::NONE).unwrap());
    for slot in 0..128 {
        let mut entity = SchedulingEntity::new(policy, 1, 0);
        entity.activate_deadline(slot as u64);
        queue
            .enqueue_test(
                ThreadId::from_parts(slot, 1),
                policy,
                entity,
                slot as u64,
                EnqueueReason::Wake,
            )
            .unwrap();
    }

    reset_deadline_runqueue_visits();
    pick_next(&mut queue, RtEligibility::Runnable);
    queue.deadline.assert_invariants();
    assert!(
        deadline_runqueue_visits() <= 32,
        "EDF selection must remain logarithmic, observed {} visits",
        deadline_runqueue_visits(),
    );
}

#[cfg_attr(test, test)]
#[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
fn runqueue_sequence_exhaustion_is_not_reused() {
    let mut queue = RunQueue::new();
    queue.next_sequence = u64::MAX - 1;

    assert_eq!(queue.try_allocate_sequence(), Ok(u64::MAX - 1));
    assert_eq!(
        queue.try_allocate_sequence(),
        Err(SequenceAllocationError::Exhausted)
    );
    assert_eq!(queue.next_sequence, u64::MAX);
}
