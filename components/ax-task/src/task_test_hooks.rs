//! Deterministic scheduler interleavings for target-side task tests.
//!
//! This module is available only through the non-default `task-test-hooks`
//! feature. It controls the real task system; it does not install a modeled
//! runtime or own a second scheduler state.

use core::{
    pin::Pin,
    sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

use crate::{
    DispatchCharge, FairEntity, FairMode, Nice, RootRtBandwidth, RtRunQueueBandwidth, RunQueue,
    SchedulePolicy, SchedulerClass, SchedulingEntity, TaskError, TaskSystemConfig, ThreadId,
    ThreadState,
    inbox::{InboxKind, InboxMessage, InboxNode, PublishResult},
    runtime::MonotonicDeadline,
    system::CpuDeadlineState,
    timer::{TaskDeadlineKind, TaskDeadlineNode},
};

static TARGET_WAITER: AtomicU64 = AtomicU64::new(0);
static PI_RELEASE_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static PI_CANCEL_RELEASE_OWNER: AtomicU64 = AtomicU64::new(0);
static PI_CANCEL_RELEASE_WAITER: AtomicU64 = AtomicU64::new(0);
static PI_CANCEL_RELEASE_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static TARGET_CHAIN_TOP: AtomicU64 = AtomicU64::new(0);
static PI_CHAIN_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static PI_OWNER_EXIT_TARGET_WAITER: AtomicU64 = AtomicU64::new(0);
static PI_OWNER_EXIT_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static PI_OWNER_LIFETIME_TARGET_WAITER: AtomicU64 = AtomicU64::new(0);
static PI_OWNER_LIFETIME_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static PI_OWNER_SPIN_TARGET_WAITER: AtomicU64 = AtomicU64::new(0);
static PI_OWNER_SPIN_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static PI_OWNER_SPIN_ITERATIONS: AtomicU64 = AtomicU64::new(0);
static WAKE_IRQ_OWNER_PROBE: IrqOwnerProbe = IrqOwnerProbe::new();
static WAKE_ENTITY_READ_COPY_TARGET: AtomicU64 = AtomicU64::new(0);
static WAKE_ENTITY_READ_COUNT: AtomicU64 = AtomicU64::new(0);
static WAKE_ENTITY_READ_COPY_COUNT: AtomicU64 = AtomicU64::new(0);
static WAKE_ENTITY_READ_COPY_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static WAKE_FAIR_VTIME_TARGET: AtomicU64 = AtomicU64::new(0);
static WAKE_FAIR_VTIME_UPDATE_COUNT: AtomicU64 = AtomicU64::new(0);
static WAKE_FAIR_VTIME_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static CURRENT_FAIR_VTIME_TARGET: AtomicU64 = AtomicU64::new(0);
static CURRENT_FAIR_VTIME_UPDATE_COUNT: AtomicU64 = AtomicU64::new(0);
static CURRENT_FAIR_VTIME_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static DIRECT_WAKE_FAILURE_TARGET: AtomicU64 = AtomicU64::new(0);
static DIRECT_WAKE_FAILURE_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static DIRECT_WAKE_COALESCED_BLOCKED: AtomicU8 = AtomicU8::new(0);
static DIRECT_WAKE_ON_RQ_TARGET: AtomicU64 = AtomicU64::new(0);
static DIRECT_WAKE_ON_RQ_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static WAKE_OWNER_DEADLINE_REFRESH_TARGET: AtomicU64 = AtomicU64::new(0);
static WAKE_OWNER_DEADLINE_REFRESH_REQUIRED: AtomicU8 = AtomicU8::new(0);
static WAKE_OWNER_DEADLINE_REFRESH_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static EQUAL_RT_WAKE_TARGET: AtomicU64 = AtomicU64::new(0);
static EQUAL_RT_WAKE_RESCHEDULE: AtomicU8 = AtomicU8::new(0);
static EQUAL_RT_WAKE_INJECT_OWNER_WORK: AtomicU8 = AtomicU8::new(0);
static EQUAL_RT_WAKE_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static RT_WAKE_PLACEMENT_TARGET: AtomicU64 = AtomicU64::new(0);
static RT_WAKE_PLACEMENT_CPU: AtomicU64 = AtomicU64::new(0);
static RT_WAKE_PLACEMENT_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static PARK_PREPARE_RUNTIME_CPU_TARGET: AtomicU64 = AtomicU64::new(0);
static PARK_PREPARE_RUNTIME_CPU_ENTRIES: AtomicU64 = AtomicU64::new(0);
static PARK_PREPARE_RUNTIME_CPU_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static PARK_IRQ_OWNER_PROBE: IrqOwnerProbe = IrqOwnerProbe::new();
static PARK_THREAD_SCHED_ACQUIRE_COUNT: AtomicU64 = AtomicU64::new(0);
static SWITCH_TAIL_IRQ_OWNER_PROBE: IrqOwnerProbe = IrqOwnerProbe::new();
static SWITCH_TAIL_THREAD_SCHED_ACQUIRE_COUNT: AtomicU64 = AtomicU64::new(0);
static SWITCH_TAIL_RQ_REACQUIRE_COUNT: AtomicU64 = AtomicU64::new(0);
static SWITCH_TAIL_RQ_BATON_COUNT: AtomicU64 = AtomicU64::new(0);
static SWITCH_TAIL_STATE_ORDER_TARGET: AtomicU64 = AtomicU64::new(0);
static SWITCH_TAIL_STATE_OBSERVED_ON_CPU: AtomicU8 = AtomicU8::new(0);
static SWITCH_TAIL_STATE_ORDER_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static POLICY_SWITCH_HANDOFF_TARGET: AtomicU64 = AtomicU64::new(0);
static POLICY_SWITCH_HANDOFF_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static OWNER_WAKE_PUBLICATION_TARGET: AtomicU64 = AtomicU64::new(0);
static OWNER_WAKE_PUBLICATION_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static DEADLINE_PUBLICATION_TARGET_CPU: AtomicU64 = AtomicU64::new(0);
static DEADLINE_PUBLICATION_OBSERVATION_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DEADLINE_PUBLICATION_RT_PERIOD_OBSERVATION_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DEADLINE_PUBLICATION_REGISTRATION_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DEADLINE_PUBLICATION_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DEADLINE_PUBLICATION_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static DEADLINE_SOFT_EXPIRY_TARGET_CPU: AtomicU64 = AtomicU64::new(0);
static DEADLINE_SOFT_EXPIRY_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DEADLINE_SOFT_EXPIRY_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static KTIMER_PENDING_YIELD_TARGET_CPU: AtomicU64 = AtomicU64::new(0);
static KTIMER_PENDING_YIELD_COUNT: AtomicU64 = AtomicU64::new(0);
static KTIMER_PENDING_YIELD_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static KTIMER_SELECTION_TARGET_CPU: AtomicU64 = AtomicU64::new(0);
static KTIMER_SELECTION_TARGET_TIMER: AtomicU64 = AtomicU64::new(0);
static KTIMER_SELECTION_BASE_ENTRIES: AtomicU64 = AtomicU64::new(0);
static KTIMER_SELECTION_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static RT_POLICY_DELIVERY_TARGET: AtomicU64 = AtomicU64::new(0);
static RT_POLICY_DELIVERY_REQUIRED: AtomicU8 = AtomicU8::new(0);
static RT_POLICY_DELIVERY_EVENTS: AtomicU8 = AtomicU8::new(0);
static RT_POLICY_REQUEST_PUBLICATIONS: AtomicU64 = AtomicU64::new(0);
static RT_POLICY_DELIVERY_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static DISABLED_RT_BANDWIDTH_TARGET_CPU: AtomicU64 = AtomicU64::new(0);
static DISABLED_RT_BANDWIDTH_ACTIVATION_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DISABLED_RT_BANDWIDTH_CHARGE_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DISABLED_RT_BANDWIDTH_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static CURRENT_HANDLE_QUERY_TARGET: AtomicU64 = AtomicU64::new(0);
static CURRENT_HANDLE_QUERY_COUNT: AtomicU64 = AtomicU64::new(0);
static CURRENT_HANDLE_QUERY_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static CURRENT_PREEMPT_GUARD_TARGET: AtomicU64 = AtomicU64::new(0);
static CURRENT_PREEMPT_GUARD_COUNT: AtomicU64 = AtomicU64::new(0);
static CURRENT_PREEMPT_GUARD_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static CURRENT_DISPATCH_DETACH_TARGET: AtomicU64 = AtomicU64::new(0);
static CURRENT_DISPATCH_DETACH_COUNT: AtomicU64 = AtomicU64::new(0);
static CURRENT_DISPATCH_DETACH_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static LONE_YIELD_RUNTIME_TARGET: AtomicU64 = AtomicU64::new(0);
static LONE_YIELD_RUNTIME_RUNNING: AtomicU8 = AtomicU8::new(0);
static LONE_YIELD_RUNTIME_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static LONE_PREEMPT_TRANSITION_TARGET: AtomicU64 = AtomicU64::new(0);
static LONE_PREEMPT_PUT_PREV_COUNT: AtomicU64 = AtomicU64::new(0);
static LONE_PREEMPT_SET_NEXT_COUNT: AtomicU64 = AtomicU64::new(0);
static LONE_PREEMPT_TRANSITION_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static RUNNABLE_HANDOFF_OUTGOING_TARGET: AtomicU64 = AtomicU64::new(0);
static RUNNABLE_HANDOFF_INCOMING_TARGET: AtomicU64 = AtomicU64::new(0);
static RUNNABLE_HANDOFF_RUNNING_TO_READY: AtomicU64 = AtomicU64::new(0);
static RUNNABLE_HANDOFF_READY_TO_RUNNING: AtomicU64 = AtomicU64::new(0);
static RUNNABLE_HANDOFF_DEADLINE_DERIVATIONS: AtomicU64 = AtomicU64::new(0);
static RUNNABLE_HANDOFF_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static NO_SWITCH_THREAD_LOCK_TARGET: AtomicU64 = AtomicU64::new(0);
static NO_SWITCH_THREAD_LOCK_COUNT: AtomicU64 = AtomicU64::new(0);
static NO_SWITCH_THREAD_LOCK_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static YIELD_THREAD_LOCK_TARGET: AtomicU64 = AtomicU64::new(0);
static YIELD_THREAD_LOCK_COUNT: AtomicU64 = AtomicU64::new(0);
static YIELD_THREAD_LOCK_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static OWNER_CONTROL_REARM_TARGET_CPU: AtomicU64 = AtomicU64::new(0);
static OWNER_CONTROL_REARM_AFTER_DRAIN: AtomicU64 = AtomicU64::new(0);
static OWNER_CONTROL_REARM_AFTER_ACK: AtomicU64 = AtomicU64::new(0);
static OWNER_CONTROL_REARM_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static OWNER_CONTROL_REARM_NODES: [InboxNode; crate::DEFAULT_BATCH_LIMIT + 1] =
    [const { InboxNode::new(InboxKind::OwnerControl) }; crate::DEFAULT_BATCH_LIMIT + 1];
static OWNER_CONTROL_COALESCING_NODE: InboxNode = InboxNode::new(InboxKind::OwnerControl);
static OWNER_CONTROL_PENDING_REQUEST_NODE: InboxNode = InboxNode::new(InboxKind::OwnerControl);
static FAIR_DELAY_DEQUEUE_TARGET: AtomicU64 = AtomicU64::new(0);
static PARK_PROFILE_HOOK: AtomicUsize = AtomicUsize::new(0);
static LINKED_PICK_FULL_SNAPSHOT_COUNT: AtomicU64 = AtomicU64::new(0);
static LINKED_PICK_FULL_SNAPSHOT_TARGET_A: AtomicU64 = AtomicU64::new(0);
static LINKED_PICK_FULL_SNAPSHOT_TARGET_B: AtomicU64 = AtomicU64::new(0);
static LINKED_PICK_FULL_SNAPSHOT_SCOPE: AtomicUsize = AtomicUsize::new(0);
static THREAD_SCHED_PUBLICATION_WAIT_TARGET: AtomicU64 = AtomicU64::new(0);
static THREAD_SCHED_PUBLICATION_WAIT_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static THREAD_SCHED_LOCK_HOLD_TARGET: AtomicU64 = AtomicU64::new(0);
static THREAD_SCHED_LOCK_HOLD_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static PARK_PUBLICATION_SERIALIZATION_TARGET: AtomicU64 = AtomicU64::new(0);
static PARK_PUBLICATION_SERIALIZATION_OUTCOME: AtomicU8 = AtomicU8::new(0);
static PARK_PUBLICATION_SERIALIZATION_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static WAIT_CLAIM_BEFORE_WAKE_TARGET: AtomicU64 = AtomicU64::new(0);
static WAIT_CLAIM_BEFORE_WAKE_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);

const STAGE_IDLE: u8 = 0;
const STAGE_CONFIGURING: u8 = 1;
const STAGE_ARMED: u8 = 2;
const STAGE_WAITER_REGISTERED: u8 = 3;
const STAGE_RELEASE_BEFORE_WAKE: u8 = 4;
const STAGE_WAITER_MAY_CLAIM: u8 = 5;
const STAGE_RELEASE_MAY_WAKE: u8 = 6;
const STAGE_WAITING_FOR_TRANSACTION: u8 = 7;
const STAGE_TRANSACTION_ACTIVE: u8 = 8;
const STAGE_CANCELLED: u8 = 9;
const STAGE_CHAIN_DECIDED: u8 = 3;
const STAGE_OWNER_MAY_CHANGE: u8 = 4;
const STAGE_OWNER_CAPTURED: u8 = 3;
const STAGE_OWNER_EXITED: u8 = 4;
const STAGE_COMPLETE: u8 = 3;
const STAGE_SWITCH_HANDOFF_PAUSED: u8 = 3;
const STAGE_SWITCH_HANDOFF_UPDATE_WAITING: u8 = 4;
const STAGE_SWITCH_HANDOFF_RELEASED: u8 = 5;
const STAGE_OWNER_WAKE_PUBLISHED: u8 = 3;
const STAGE_OWNER_WAKE_RELEASED: u8 = 4;
const STAGE_DIRECT_WAKE_FAILURE_PAUSED: u8 = 3;
const STAGE_DIRECT_WAKE_FAILURE_RELEASED: u8 = 4;
const STAGE_DIRECT_WAKE_ON_RQ_PAUSED: u8 = 3;
const STAGE_DIRECT_WAKE_ON_RQ_RELEASED: u8 = 4;
const STAGE_DIRECT_WAKE_ON_RQ_COMPLETE: u8 = 5;
const STAGE_OWNER_CONTROL_DRAINED: u8 = 3;
const STAGE_OWNER_CONTROL_ACKNOWLEDGED: u8 = 4;
const STAGE_WAIT_CLAIM_PAUSED: u8 = 3;
const STAGE_WAIT_CLAIM_RELEASED: u8 = 4;
const STAGE_THREAD_SCHED_LOCK_HELD: u8 = 3;
const STAGE_THREAD_SCHED_LOCK_RELEASED: u8 = 4;
const PARK_PUBLICATION_TASK_LOCK_BUSY: u8 = 1 << 0;
const PARK_PUBLICATION_STARTED: u8 = 1 << 1;
const RT_POLICY_RESCHEDULE: u8 = 1 << 0;
const RT_POLICY_OWNER_WORK: u8 = 1 << 1;

/// Returns the footprint of the unique active scheduler-state owner token.
///
/// The token crosses task-control, runqueue, and current-dispatch ownership
/// boundaries on every block and wake. Keep the mutable scheduler record at a
/// stable address so those transitions move one pointer rather than copy the
/// complete Fair, RT, or Deadline entity.
pub const fn active_scheduling_state_footprint() -> usize {
    core::mem::size_of::<crate::ActiveSchedulingState>()
}

/// Checks that deferred soft-timer ownership cannot hide a hard scheduler deadline.
pub fn softirq_activation_preserves_hard_deadline() -> bool {
    let mut state = CpuDeadlineState::new(TaskSystemConfig::new(1));
    let node = TaskDeadlineNode::deadline_cbs_for_thread(ThreadId::from_parts(1, 1));
    let deadline = MonotonicDeadline::from_nanos(10).expect("test deadline must be finite");
    let _registration = state
        .queue
        .arm(&node, deadline, TaskDeadlineKind::DeadlineCbs)
        .expect("test hard deadline must fit the fixed queue");
    state.softirq_activated = true;
    state.timer_deadline() == Some(deadline)
}

/// Checks Linux EEVDF's lone-current periodic tick rule.
pub fn lone_fair_slice_expiry_only_updates_accounting() -> bool {
    let mut queue = RunQueue::configured(u64::MAX, 1);
    let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let current_entity =
        SchedulingEntity::Fair(FairEntity::new(Nice::ZERO, FairMode::Normal, 1, 0));
    let tick = SchedulerClass::Fair.task_tick(
        &mut queue,
        ThreadId::from_parts(1, 1),
        policy,
        &current_entity,
        DispatchCharge {
            slice_expired: true,
            ..DispatchCharge::default()
        },
    );
    !tick.request_reschedule
}

/// Checks Linux v7.1 RUN_TO_PARITY for an equal-slice Fair wakeup.
pub fn equal_slice_wakeup_preserves_current_protection() -> bool {
    let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let mut current = FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 2_000);
    current.set_slice_protection(None);
    let wakee = FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 1_000);

    !crate::scheduler::wakeup_preempts(
        policy,
        &SchedulingEntity::Fair(current),
        false,
        policy,
        &SchedulingEntity::Fair(wakee),
        2_000,
    )
}

/// Checks Linux v7.1's default `WF_SYNC` EEVDF preemption decision.
pub fn sync_wakeup_uses_default_eevdf_without_next_buddy() -> bool {
    let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
    let current =
        SchedulingEntity::Fair(FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 2_000));
    let wakee = SchedulingEntity::Fair(FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 1_000));

    crate::scheduler::wakeup_preempts(policy, &current, false, policy, &wakee, 2_000)
        && crate::scheduler::default_sync_wakeup_preempts(
            policy, &current, false, policy, &wakee, 2_000,
        )
}

/// Checks that renewing an EEVDF request preserves positive lag.
pub fn fair_request_renewal_preserves_lag() -> bool {
    let mut entity = FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 0);
    entity
        .place_after_activation(0, 0)
        .expect("test entity must accept its initial placement");
    assert!(entity.charge(500, 0));

    entity.renew_request();

    entity.vruntime() == 500 && entity.virtual_deadline() == 1_500
}

/// Checks Linux's two deadline renewals when an expired Fair task yields.
pub fn expired_fair_request_yield_forfeits_new_request() -> bool {
    let mut entity = FairEntity::new(Nice::ZERO, FairMode::Normal, 100, 0);
    entity
        .place_after_activation(0, 0)
        .expect("test entity must accept its initial placement");
    entity.renew_request();
    assert!(entity.charge(100, 0));

    entity.yield_request(100);

    entity.vruntime() == 200 && entity.virtual_deadline() == 300
}

/// Checks Linux's no-switch yield rule for a lone current Fair or RT task.
pub fn lone_current_yield_preserves_linux_dispatch() -> bool {
    let fair = SchedulingEntity::Fair(FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 0));
    let fifo = SchedulingEntity::Fifo;
    let round_robin = SchedulingEntity::RoundRobin {
        remaining_quantum_ns: 1_000,
    };

    crate::system::lone_current_yield_keeps_dispatch(Some(&fair), false)
        && crate::system::lone_current_yield_keeps_dispatch(Some(&fifo), false)
        && crate::system::lone_current_yield_keeps_dispatch(Some(&round_robin), false)
        && !crate::system::lone_current_yield_keeps_dispatch(Some(&fifo), true)
        && !crate::system::lone_current_yield_keeps_dispatch(Some(&round_robin), true)
        && !crate::system::lone_current_yield_keeps_dispatch(
            Some(&SchedulingEntity::KernelStop),
            false,
        )
}

/// Checks Linux's immediate period kick when idle RT bandwidth restarts.
pub fn inactive_rt_bandwidth_restart_kicks_period_immediately() -> bool {
    let cpu = crate::CpuId::new(0);
    let bandwidth = RootRtBandwidth::new(TaskSystemConfig::new(1).with_rt_bandwidth(1_000, 950));
    let period_ns = bandwidth.period_ns();
    let origin = crate::runtime::MonotonicInstant::from_nanos(0)
        .expect("the monotonic origin must be representable");

    assert!(bandwidth.activate(cpu, || origin));
    let initial_deadline = bandwidth
        .deadline_for(cpu)
        .expect("RT activation must publish a period deadline");
    let initial_firing = bandwidth
        .begin_period(
            cpu,
            crate::runtime::MonotonicInstant::from_nanos(initial_deadline.as_nanos())
                .expect("the initial RT deadline must be representable"),
        )
        .expect("the initial RT period must fire at its published deadline");
    bandwidth.finish_period(initial_firing, false);

    let restart_ns = period_ns * 2 + period_ns / 2;
    let restart = crate::runtime::MonotonicInstant::from_nanos(restart_ns)
        .expect("the RT restart sample must be representable");
    assert!(bandwidth.activate(cpu, || restart));
    bandwidth.deadline_for(cpu).is_some_and(|deadline| {
        // Linux uses hrtimer_forward_now(timer, 0) when restarting an idle
        // bandwidth timer. Its minimum hrtimer-resolution delay is represented
        // by an already-due clockevent in this scheduler's deadline domain.
        deadline.as_nanos() == restart_ns
    })
}

/// Checks Linux's no-throttle rule after a runqueue borrows one full RT period.
pub fn borrowed_full_rt_period_has_no_throttle_edge() -> bool {
    const PERIOD_NS: u64 = 100;
    let mut bandwidth = RtRunQueueBandwidth::offline();
    bandwidth.enable(PERIOD_NS, PERIOD_NS / 2);

    assert!(bandwidth.account(PERIOD_NS / 2 + 1));
    bandwidth.borrow_runtime(PERIOD_NS / 2, PERIOD_NS);
    assert_eq!(bandwidth.runtime_ns(), PERIOD_NS);
    assert!(bandwidth.account(PERIOD_NS / 2));

    !bandwidth.should_throttle() && bandwidth.runtime_until_throttle().is_none()
}

/// Checks that an already-throttled rq cannot borrow more root RT runtime.
pub fn already_throttled_rt_charge_preserves_runtime_loans() -> bool {
    crate::TaskSystem::already_throttled_rt_charge_preserves_runtime_loans()
}

/// Checks that an empty RT ledger neither rebalances nor clears throttling.
pub fn zero_rt_time_period_preserves_throttle_and_runtime_loans() -> bool {
    crate::TaskSystem::zero_rt_time_period_preserves_throttle_and_runtime_loans()
}

/// Checks Linux's 10 us lower bound for a Fair hrtick request.
pub fn fair_hrtick_uses_linux_minimum_delta() -> bool {
    FairEntity::new(Nice::ZERO, FairMode::Normal, 1, 0).runtime_timer_delta_ns() == 10_000
}

/// Checks that Linux hrtick follows the EEVDF request deadline, not `vprot`.
pub fn fair_hrtick_tracks_request_deadline() -> bool {
    let mut entity = FairEntity::new(Nice::ZERO, FairMode::Normal, 100_000, 10_000);
    entity.set_slice_protection(Some(25_000));

    entity.slice_is_protected() && entity.runtime_timer_delta_ns() == 100_000
}

/// Reports the real runtime execution context for target-side timer tests.
pub fn in_hard_irq_context() -> bool {
    crate::runtime::task_runtime::in_hard_irq()
}

/// Enters and exits one ordinary preemption scope through the real runtime.
pub fn exercise_preempt_guard() {
    drop(crate::lock::PreemptScope::enter());
}

/// Observes the nested exit while an outer ordinary preemption scope remains live.
pub fn exercise_nested_preempt_guard_inner_exit(observe: impl FnOnce()) {
    let outer = crate::lock::PreemptScope::enter();
    let inner = crate::lock::PreemptScope::enter();
    drop(inner);
    observe();
    drop(outer);
}

/// Arms external-handle accounting for one real scheduler thread.
pub fn arm_current_handle_query_probe(thread: u64) {
    assert_ne!(
        thread, 0,
        "a current-handle probe identity must be non-zero"
    );
    assert_eq!(
        CURRENT_HANDLE_QUERY_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one current-handle query probe may be armed"
    );
    CURRENT_HANDLE_QUERY_TARGET.store(thread, Ordering::Relaxed);
    CURRENT_HANDLE_QUERY_COUNT.store(0, Ordering::Relaxed);
    CURRENT_HANDLE_QUERY_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Takes the number of external current handles acquired while armed.
pub fn take_current_handle_query_count() -> Option<u64> {
    if CURRENT_HANDLE_QUERY_STAGE
        .compare_exchange(
            STAGE_ARMED,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let count = CURRENT_HANDLE_QUERY_COUNT.load(Ordering::Relaxed);
    CURRENT_HANDLE_QUERY_TARGET.store(0, Ordering::Relaxed);
    CURRENT_HANDLE_QUERY_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(count)
}

pub(crate) fn record_current_handle_query(thread: ThreadId) {
    if CURRENT_HANDLE_QUERY_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && CURRENT_HANDLE_QUERY_TARGET.load(Ordering::Relaxed) == thread.as_u64()
    {
        CURRENT_HANDLE_QUERY_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

/// Arms preemption-guard accounting for one real scheduler thread.
pub fn arm_current_preempt_guard_probe(thread: u64) {
    assert_ne!(
        thread, 0,
        "a current preemption-guard probe identity must be non-zero"
    );
    assert_eq!(
        CURRENT_PREEMPT_GUARD_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one current preemption-guard probe may be armed"
    );
    CURRENT_PREEMPT_GUARD_TARGET.store(thread, Ordering::Relaxed);
    CURRENT_PREEMPT_GUARD_COUNT.store(0, Ordering::Relaxed);
    CURRENT_PREEMPT_GUARD_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Takes preemption-guard entries made by the armed scheduler thread.
pub fn take_current_preempt_guard_count() -> Option<u64> {
    if CURRENT_PREEMPT_GUARD_STAGE
        .compare_exchange(
            STAGE_ARMED,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let count = CURRENT_PREEMPT_GUARD_COUNT.load(Ordering::Relaxed);
    CURRENT_PREEMPT_GUARD_TARGET.store(0, Ordering::Relaxed);
    CURRENT_PREEMPT_GUARD_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(count)
}

pub(crate) fn record_current_preempt_guard(thread: ThreadId) {
    if CURRENT_PREEMPT_GUARD_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && CURRENT_PREEMPT_GUARD_TARGET.load(Ordering::Relaxed) == thread.as_u64()
    {
        CURRENT_PREEMPT_GUARD_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

/// Arms current-dispatch accounting for one real scheduler thread.
pub fn arm_current_dispatch_accounting_probe(thread: u64) {
    assert_ne!(
        thread, 0,
        "a current-dispatch probe identity must be non-zero"
    );
    assert_eq!(
        CURRENT_DISPATCH_DETACH_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one current-dispatch detach probe may be armed"
    );
    CURRENT_DISPATCH_DETACH_TARGET.store(thread, Ordering::Relaxed);
    CURRENT_DISPATCH_DETACH_COUNT.store(0, Ordering::Relaxed);
    CURRENT_DISPATCH_DETACH_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Takes current-dispatch detaches observed during the accounting transaction.
pub fn take_current_dispatch_accounting_detach_count() -> Option<u64> {
    if CURRENT_DISPATCH_DETACH_STAGE
        .compare_exchange(
            STAGE_COMPLETE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let count = CURRENT_DISPATCH_DETACH_COUNT.load(Ordering::Relaxed);
    CURRENT_DISPATCH_DETACH_TARGET.store(0, Ordering::Relaxed);
    CURRENT_DISPATCH_DETACH_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(count)
}

pub(crate) fn begin_current_dispatch_accounting_probe(thread: ThreadId) {
    if CURRENT_DISPATCH_DETACH_TARGET.load(Ordering::Acquire) == thread.as_u64() {
        let _ = CURRENT_DISPATCH_DETACH_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_TRANSACTION_ACTIVE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

pub(crate) fn complete_current_dispatch_accounting_probe(thread: ThreadId) {
    if CURRENT_DISPATCH_DETACH_TARGET.load(Ordering::Acquire) == thread.as_u64() {
        let _ = CURRENT_DISPATCH_DETACH_STAGE.compare_exchange(
            STAGE_TRANSACTION_ACTIVE,
            STAGE_COMPLETE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

pub(crate) fn record_current_dispatch_detach(thread: ThreadId) {
    if CURRENT_DISPATCH_DETACH_STAGE.load(Ordering::Acquire) == STAGE_TRANSACTION_ACTIVE
        && CURRENT_DISPATCH_DETACH_TARGET.load(Ordering::Relaxed) == thread.as_u64()
    {
        CURRENT_DISPATCH_DETACH_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

/// Arms a runtime-publication probe for one real lone-current yield.
pub fn arm_lone_yield_runtime_probe(thread: u64) {
    assert_ne!(thread, 0, "a lone-yield probe identity must be non-zero");
    assert_eq!(
        LONE_YIELD_RUNTIME_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one lone-yield runtime probe may be armed"
    );
    LONE_YIELD_RUNTIME_TARGET.store(thread, Ordering::Relaxed);
    LONE_YIELD_RUNTIME_RUNNING.store(0, Ordering::Relaxed);
    LONE_YIELD_RUNTIME_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Takes whether a real lone-current yield retained its running publication.
pub fn take_lone_yield_runtime_running() -> Option<bool> {
    if LONE_YIELD_RUNTIME_STAGE
        .compare_exchange(
            STAGE_COMPLETE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let running = LONE_YIELD_RUNTIME_RUNNING.load(Ordering::Relaxed) != 0;
    LONE_YIELD_RUNTIME_TARGET.store(0, Ordering::Relaxed);
    LONE_YIELD_RUNTIME_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(running)
}

pub(crate) fn record_lone_yield_runtime_state(thread: ThreadId, running: bool) {
    if LONE_YIELD_RUNTIME_TARGET.load(Ordering::Acquire) != thread.as_u64()
        || LONE_YIELD_RUNTIME_STAGE
            .compare_exchange(
                STAGE_ARMED,
                STAGE_CONFIGURING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
    {
        return;
    }
    LONE_YIELD_RUNTIME_RUNNING.store(u8::from(running), Ordering::Relaxed);
    LONE_YIELD_RUNTIME_STAGE.store(STAGE_COMPLETE, Ordering::Release);
}

/// Lifecycle operations observed while one lone current task services preemption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LonePreemptionTransitions {
    /// Linux `put_prev_task()` equivalents applied to the current task.
    pub put_prev: u64,
    /// Linux `set_next_task()` equivalents applied to the selected task.
    pub set_next: u64,
}

/// Arms lifecycle-operation accounting for one lone-current preemption pass.
pub fn arm_lone_preemption_transition_probe(thread: u64) {
    assert_ne!(
        thread, 0,
        "a lone-preemption probe identity must be non-zero"
    );
    assert_eq!(
        LONE_PREEMPT_TRANSITION_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one lone-preemption transition probe may be armed"
    );
    LONE_PREEMPT_TRANSITION_TARGET.store(thread, Ordering::Relaxed);
    LONE_PREEMPT_PUT_PREV_COUNT.store(0, Ordering::Relaxed);
    LONE_PREEMPT_SET_NEXT_COUNT.store(0, Ordering::Relaxed);
    LONE_PREEMPT_TRANSITION_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Takes lifecycle operations from the armed lone-current preemption pass.
pub fn take_lone_preemption_transitions() -> Option<LonePreemptionTransitions> {
    if LONE_PREEMPT_TRANSITION_STAGE
        .compare_exchange(
            STAGE_ARMED,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let transitions = LonePreemptionTransitions {
        put_prev: LONE_PREEMPT_PUT_PREV_COUNT.load(Ordering::Relaxed),
        set_next: LONE_PREEMPT_SET_NEXT_COUNT.load(Ordering::Relaxed),
    };
    LONE_PREEMPT_TRANSITION_TARGET.store(0, Ordering::Relaxed);
    LONE_PREEMPT_TRANSITION_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(transitions)
}

pub(crate) fn record_lone_preemption_put_prev(thread: ThreadId) {
    if LONE_PREEMPT_TRANSITION_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && LONE_PREEMPT_TRANSITION_TARGET.load(Ordering::Relaxed) == thread.as_u64()
    {
        LONE_PREEMPT_PUT_PREV_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_lone_preemption_set_next(thread: ThreadId) {
    if LONE_PREEMPT_TRANSITION_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && LONE_PREEMPT_TRANSITION_TARGET.load(Ordering::Relaxed) == thread.as_u64()
    {
        LONE_PREEMPT_SET_NEXT_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

/// Runnable lifecycle publications observed across one real task handoff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunnableHandoffTransitions {
    /// Outgoing `Running -> Ready` publications.
    pub running_to_ready: u64,
    /// Incoming `Ready -> Running` publications.
    pub ready_to_running: u64,
    /// Scheduler-deadline derivations performed by this exact handoff.
    pub schedule_selection_deadline_derivations: u64,
}

/// Arms lifecycle accounting for one real runnable-to-runnable handoff.
pub fn arm_runnable_handoff_transition_probe(outgoing: u64, incoming: u64) {
    assert_ne!(outgoing, 0, "a runnable handoff identity must be non-zero");
    assert_ne!(incoming, 0, "a runnable handoff identity must be non-zero");
    assert_ne!(outgoing, incoming, "a handoff requires distinct tasks");
    assert_eq!(
        RUNNABLE_HANDOFF_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one runnable handoff probe may be armed"
    );
    RUNNABLE_HANDOFF_OUTGOING_TARGET.store(outgoing, Ordering::Relaxed);
    RUNNABLE_HANDOFF_INCOMING_TARGET.store(incoming, Ordering::Relaxed);
    RUNNABLE_HANDOFF_RUNNING_TO_READY.store(0, Ordering::Relaxed);
    RUNNABLE_HANDOFF_READY_TO_RUNNING.store(0, Ordering::Relaxed);
    RUNNABLE_HANDOFF_DEADLINE_DERIVATIONS.store(0, Ordering::Relaxed);
    RUNNABLE_HANDOFF_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Takes lifecycle publications from the armed runnable handoff.
pub fn take_runnable_handoff_transitions() -> Option<RunnableHandoffTransitions> {
    if RUNNABLE_HANDOFF_STAGE
        .compare_exchange(
            STAGE_ARMED,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let transitions = RunnableHandoffTransitions {
        running_to_ready: RUNNABLE_HANDOFF_RUNNING_TO_READY.load(Ordering::Relaxed),
        ready_to_running: RUNNABLE_HANDOFF_READY_TO_RUNNING.load(Ordering::Relaxed),
        schedule_selection_deadline_derivations: RUNNABLE_HANDOFF_DEADLINE_DERIVATIONS
            .load(Ordering::Relaxed),
    };
    RUNNABLE_HANDOFF_OUTGOING_TARGET.store(0, Ordering::Relaxed);
    RUNNABLE_HANDOFF_INCOMING_TARGET.store(0, Ordering::Relaxed);
    RUNNABLE_HANDOFF_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(transitions)
}

pub(crate) fn record_runnable_handoff_deadline_derivations(
    outgoing: Option<ThreadId>,
    incoming: ThreadId,
    derivations: u64,
) {
    if RUNNABLE_HANDOFF_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || outgoing.map(ThreadId::as_u64)
            != Some(RUNNABLE_HANDOFF_OUTGOING_TARGET.load(Ordering::Relaxed))
        || incoming.as_u64() != RUNNABLE_HANDOFF_INCOMING_TARGET.load(Ordering::Relaxed)
    {
        return;
    }
    RUNNABLE_HANDOFF_DEADLINE_DERIVATIONS.fetch_add(derivations, Ordering::Relaxed);
}

pub(crate) fn record_runnable_handoff_transition(
    thread: ThreadId,
    from: ThreadState,
    to: ThreadState,
) {
    if RUNNABLE_HANDOFF_STAGE.load(Ordering::Acquire) != STAGE_ARMED {
        return;
    }
    if RUNNABLE_HANDOFF_OUTGOING_TARGET.load(Ordering::Relaxed) == thread.as_u64()
        && from == ThreadState::Running
        && to == ThreadState::Ready
    {
        RUNNABLE_HANDOFF_RUNNING_TO_READY.fetch_add(1, Ordering::Relaxed);
    }
    if RUNNABLE_HANDOFF_INCOMING_TARGET.load(Ordering::Relaxed) == thread.as_u64()
        && from == ThreadState::Ready
        && to == ThreadState::Running
    {
        RUNNABLE_HANDOFF_READY_TO_RUNNING.fetch_add(1, Ordering::Relaxed);
    }
}

/// Arms task-lock accounting for one real scheduler no-switch pass.
pub fn arm_no_switch_thread_lock_probe(thread: u64) {
    assert_ne!(thread, 0, "a no-switch probe identity must be non-zero");
    assert_eq!(
        NO_SWITCH_THREAD_LOCK_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one no-switch task-lock probe may be armed"
    );
    NO_SWITCH_THREAD_LOCK_TARGET.store(thread, Ordering::Relaxed);
    NO_SWITCH_THREAD_LOCK_COUNT.store(0, Ordering::Relaxed);
    NO_SWITCH_THREAD_LOCK_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Publishes owner work without requesting a context switch on this CPU.
pub fn request_current_owner_work() -> Result<(), crate::TaskError> {
    let _pin = crate::lock::PreemptScope::enter();
    let remote = crate::facade::current_cpu_remote().ok_or(crate::TaskError::NotInitialized)?;
    remote.request_scheduler_work();
    Ok(())
}

/// Publishes one preemption request for the current CPU.
pub fn request_current_reschedule() -> Result<(), crate::TaskError> {
    let _pin = crate::lock::PreemptScope::enter();
    let remote = crate::facade::current_cpu_remote().ok_or(crate::TaskError::NotInitialized)?;
    remote.request_remote_reschedule();
    Ok(())
}

/// Returns the current CPU's completed scheduler-deadline derivation count.
pub fn current_scheduler_deadline_derivations() -> Result<u64, crate::TaskError> {
    let _pin = crate::lock::PreemptScope::enter();
    let remote = crate::facade::current_cpu_remote().ok_or(crate::TaskError::NotInitialized)?;
    Ok(remote.scheduler_deadline_derivations())
}

/// Returns the current CPU's cumulative switch-selection deadline derivations.
///
/// This count is not scoped to a task or scheduler transaction.
pub fn current_schedule_selection_deadline_derivations() -> Result<u64, crate::TaskError> {
    let _pin = crate::lock::PreemptScope::enter();
    let remote = crate::facade::current_cpu_remote().ok_or(crate::TaskError::NotInitialized)?;
    Ok(remote.schedule_selection_deadline_derivations())
}

/// Logical publications after requesting the same sticky scheduler reason twice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulerRequestCoalescingPublications {
    /// Whether the first request changed the sticky scheduler state.
    pub first: bool,
    /// Whether the duplicate request changed the sticky scheduler state.
    pub duplicate: bool,
}

/// Publishes the current CPU's sticky owner-work reason twice in one guard.
pub fn request_current_owner_work_twice()
-> Result<SchedulerRequestCoalescingPublications, crate::TaskError> {
    let pin = crate::lock::PreemptScope::enter();
    let remote = crate::facade::current_cpu_remote().ok_or(crate::TaskError::NotInitialized)?;
    let first = remote.request_scheduler_work_transition_for_test();
    let duplicate = remote.request_scheduler_work_transition_for_test();
    drop(pin);
    Ok(SchedulerRequestCoalescingPublications { first, duplicate })
}

/// Publishes the current CPU's coupled preempt/owner-work reasons twice.
pub fn request_current_combined_scheduler_work_twice()
-> Result<SchedulerRequestCoalescingPublications, crate::TaskError> {
    let pin = crate::lock::PreemptScope::enter();
    let remote = crate::facade::current_cpu_remote().ok_or(crate::TaskError::NotInitialized)?;
    let first = remote.request_combined_scheduler_work_transition_for_test();
    let duplicate = remote.request_combined_scheduler_work_transition_for_test();
    drop(pin);
    Ok(SchedulerRequestCoalescingPublications { first, duplicate })
}

/// Logical head publications around one coalesced owner-control node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnerControlCoalescingPublications {
    /// Whether an older sticky owner-work request existed at the head edge.
    pub previous_owner_work: bool,
    /// Whether the first membership owned the empty-to-nonempty head edge.
    pub first_head: bool,
    /// Whether the already-pending duplicate owned another head edge.
    pub duplicate_head: bool,
}

/// Publishes the same owner-control node twice before its owner may drain it.
pub fn publish_coalesced_owner_control_twice()
-> Result<OwnerControlCoalescingPublications, crate::TaskError> {
    let pin = crate::lock::PreemptScope::enter();
    let remote = crate::facade::current_cpu_remote().ok_or(crate::TaskError::NotInitialized)?;
    let owner = remote.owner();
    let message =
        InboxMessage::deadline_refresh_with_payload(ThreadId::from_parts(u32::MAX, 1), owner, 0, 0);
    // An unrelated sticky request may already exist when a new inbox head is
    // published. Linux attributes the notification to the llist head edge,
    // never to a delta in a shared scheduler counter.
    remote.defer_scheduler_work();
    // SAFETY: this process-lifetime fixture is never moved.
    let node = unsafe { Pin::new_unchecked(&OWNER_CONTROL_COALESCING_NODE) };
    let (first_result, first_publication) =
        remote.publish_owner_control_observed_for_test(node, message);
    assert_eq!(
        first_result,
        PublishResult::Published,
        "the coalescing fixture must begin detached"
    );
    let (duplicate_result, duplicate_publication) =
        remote.publish_owner_control_observed_for_test(node, message);
    assert_eq!(
        duplicate_result,
        PublishResult::AlreadyPending,
        "the second publication must coalesce into the pending node"
    );
    let previous_owner_work =
        first_publication.is_some_and(|publication| publication.previous_owner_work_requested());
    drop(pin);
    Ok(OwnerControlCoalescingPublications {
        previous_owner_work,
        first_head: first_publication.is_some(),
        duplicate_head: duplicate_publication.is_some(),
    })
}

/// Head publication when an older sticky owner-work request already exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnerControlPendingRequestPublication {
    /// Whether the head transition observed the older sticky request.
    pub previous_owner_work: bool,
    /// Whether the fresh membership owned a head notification.
    pub head: bool,
}

/// Publishes a fresh owner-control inbox head while the sticky owner-work bit
/// from an older delivery is still set.
pub fn publish_owner_control_after_pending_request()
-> Result<OwnerControlPendingRequestPublication, crate::TaskError> {
    let pin = crate::lock::PreemptScope::enter();
    let remote = crate::facade::current_cpu_remote().ok_or(crate::TaskError::NotInitialized)?;
    let owner = remote.owner();
    remote.request_scheduler_work();
    let message = InboxMessage::deadline_refresh_with_payload(
        ThreadId::from_parts(u32::MAX - 1, 1),
        owner,
        0,
        0,
    );
    // SAFETY: this process-lifetime fixture is never moved.
    let node = unsafe { Pin::new_unchecked(&OWNER_CONTROL_PENDING_REQUEST_NODE) };
    let (result, publication) = remote.publish_owner_control_observed_for_test(node, message);
    assert_eq!(
        result,
        PublishResult::Published,
        "the pending-request fixture must begin detached"
    );
    drop(pin);
    Ok(OwnerControlPendingRequestPublication {
        previous_owner_work: publication
            .is_some_and(|publication| publication.previous_owner_work_requested()),
        head: publication.is_some(),
    })
}

/// Sticky owner-work state around one bounded owner-control remainder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnerControlRearmState {
    /// Whether owner work was still armed immediately after the bounded drain.
    pub after_drain: bool,
    /// Whether the final transaction rearmed the remaining inbox work.
    pub after_ack: bool,
}

/// Publishes one more owner-control message than a scheduler pass may drain.
pub fn publish_bounded_owner_control_remainder() -> Result<(), crate::TaskError> {
    assert_eq!(
        OWNER_CONTROL_REARM_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one owner-control rearm probe may be armed"
    );
    let pin = crate::lock::PreemptScope::enter();
    let remote = crate::facade::current_cpu_remote().ok_or(crate::TaskError::NotInitialized)?;
    let owner = remote.owner();
    for (index, node) in OWNER_CONTROL_REARM_NODES.iter().enumerate() {
        let thread = ThreadId::from_parts(index as u32 + 1, 1);
        let message = InboxMessage::deadline_refresh_with_payload(thread, owner, 0, 0);
        // SAFETY: the process-lifetime fixture array is never moved.
        let node = unsafe { Pin::new_unchecked(node) };
        assert_eq!(
            remote.publish_owner_control(node, message),
            PublishResult::Published,
            "every fixture node must own one distinct inbox membership"
        );
    }
    OWNER_CONTROL_REARM_TARGET_CPU.store(owner.as_u32() as u64, Ordering::Relaxed);
    OWNER_CONTROL_REARM_AFTER_DRAIN.store(0, Ordering::Relaxed);
    OWNER_CONTROL_REARM_AFTER_ACK.store(0, Ordering::Relaxed);
    OWNER_CONTROL_REARM_STAGE.store(STAGE_ARMED, Ordering::Release);
    drop(pin);
    Ok(())
}

/// Takes the sticky-state observations after the real owner transaction rearm.
pub fn take_bounded_owner_control_rearm() -> Option<OwnerControlRearmState> {
    if OWNER_CONTROL_REARM_STAGE
        .compare_exchange(
            STAGE_OWNER_CONTROL_ACKNOWLEDGED,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let state = OwnerControlRearmState {
        after_drain: OWNER_CONTROL_REARM_AFTER_DRAIN.load(Ordering::Relaxed) != 0,
        after_ack: OWNER_CONTROL_REARM_AFTER_ACK.load(Ordering::Relaxed) != 0,
    };
    OWNER_CONTROL_REARM_TARGET_CPU.store(0, Ordering::Relaxed);
    OWNER_CONTROL_REARM_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(state)
}

pub(crate) fn record_bounded_owner_control_drain(
    cpu: crate::CpuId,
    pending: bool,
    owner_work_requested: bool,
) {
    if !pending
        || OWNER_CONTROL_REARM_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || OWNER_CONTROL_REARM_TARGET_CPU.load(Ordering::Relaxed) != cpu.as_u32() as u64
    {
        return;
    }
    assert!(
        !owner_work_requested,
        "the bounded drain must have claimed its original owner-work request"
    );
    OWNER_CONTROL_REARM_AFTER_DRAIN.store(u64::from(owner_work_requested), Ordering::Relaxed);
    assert_eq!(
        OWNER_CONTROL_REARM_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_OWNER_CONTROL_DRAINED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "owner-control remainder completed in an invalid stage"
    );
}

pub(crate) fn publish_preempt_before_bounded_owner_control_rearm(cpu: crate::CpuId) {
    if OWNER_CONTROL_REARM_STAGE.load(Ordering::Acquire) != STAGE_OWNER_CONTROL_DRAINED
        || OWNER_CONTROL_REARM_TARGET_CPU.load(Ordering::Relaxed) != cpu.as_u32() as u64
    {
        return;
    }
    let remote = crate::facade::current_cpu_remote()
        .expect("the bounded-drain rearm probe must run on an initialized owner CPU");
    assert_eq!(remote.owner(), cpu);
    // Force the exact Linux independence check after the rq decision boundary:
    // TIF_NEED_RESCHED may become sticky here, but it cannot stand in for the
    // remaining wake-list membership.
    remote.request_reschedule();
    assert!(
        remote.preemption_requested(),
        "the rearm probe must leave its independent preemption request sticky"
    );
}

pub(crate) fn record_bounded_owner_control_ack(cpu: crate::CpuId, owner_work_requested: bool) {
    if OWNER_CONTROL_REARM_STAGE.load(Ordering::Acquire) != STAGE_OWNER_CONTROL_DRAINED
        || OWNER_CONTROL_REARM_TARGET_CPU.load(Ordering::Relaxed) != cpu.as_u32() as u64
    {
        return;
    }
    OWNER_CONTROL_REARM_AFTER_ACK.store(u64::from(owner_work_requested), Ordering::Relaxed);
    assert_eq!(
        OWNER_CONTROL_REARM_STAGE.compare_exchange(
            STAGE_OWNER_CONTROL_DRAINED,
            STAGE_OWNER_CONTROL_ACKNOWLEDGED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_OWNER_CONTROL_DRAINED),
        "owner-control acknowledgement completed in an invalid stage"
    );
}

/// Publishes owner work for one online CPU and reports whether its scheduler
/// delivery contract was completed.
pub fn request_cpu_owner_work(cpu: u32) -> Result<bool, crate::TaskError> {
    let _pin = crate::lock::PreemptScope::enter();
    let cpu = crate::CpuId::new(cpu);
    let system = crate::facade::runtime_task_system()?;
    let remote = system
        .cpu_remote(cpu)
        .ok_or(crate::TaskError::CpuOffline(cpu.as_u32()))?;
    Ok(remote.request_scheduler_work_for_test())
}

/// Returns Linux `rq->nr_running` from one CPU's last committed rq summary.
pub fn cpu_nr_running(cpu: u32) -> Result<usize, crate::TaskError> {
    let _pin = crate::lock::PreemptScope::enter();
    let cpu = crate::CpuId::new(cpu);
    let system = crate::facade::runtime_task_system()?;
    let remote = system
        .cpu_remote(cpu)
        .ok_or(crate::TaskError::CpuOffline(cpu.as_u32()))?;
    Ok(remote.load_summary().nr_running())
}

/// Exercises the production publication used after moving a Deadline
/// reservation away from its previous owner CPU.
pub fn exercise_detached_deadline_owner_work(cpu: u32) -> Result<bool, crate::TaskError> {
    let _pin = crate::lock::PreemptScope::enter();
    let cpu = crate::CpuId::new(cpu);
    let system = crate::facade::runtime_task_system()?;
    system.exercise_detached_deadline_owner_work_for_test(cpu)
}

/// Takes task-lock entries from the armed no-switch scheduler pass.
pub fn take_no_switch_thread_lock_count() -> Option<u64> {
    if NO_SWITCH_THREAD_LOCK_STAGE
        .compare_exchange(
            STAGE_COMPLETE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let count = NO_SWITCH_THREAD_LOCK_COUNT.load(Ordering::Relaxed);
    NO_SWITCH_THREAD_LOCK_TARGET.store(0, Ordering::Relaxed);
    NO_SWITCH_THREAD_LOCK_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(count)
}

/// Cancels a probe when the sampled scheduler pass performed a real switch.
pub fn cancel_no_switch_thread_lock_probe() {
    assert_eq!(
        NO_SWITCH_THREAD_LOCK_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "only an unfinished no-switch task-lock probe may be cancelled"
    );
    NO_SWITCH_THREAD_LOCK_TARGET.store(0, Ordering::Relaxed);
    NO_SWITCH_THREAD_LOCK_STAGE.store(STAGE_IDLE, Ordering::Release);
}

pub(crate) fn record_no_switch_thread_lock(thread: ThreadId) {
    if NO_SWITCH_THREAD_LOCK_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && NO_SWITCH_THREAD_LOCK_TARGET.load(Ordering::Relaxed) == thread.as_u64()
    {
        NO_SWITCH_THREAD_LOCK_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn complete_no_switch_thread_lock_probe(thread: ThreadId) {
    if NO_SWITCH_THREAD_LOCK_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || NO_SWITCH_THREAD_LOCK_TARGET.load(Ordering::Relaxed) != thread.as_u64()
    {
        return;
    }
    assert_eq!(
        NO_SWITCH_THREAD_LOCK_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_COMPLETE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "the no-switch task-lock probe completed in an invalid stage"
    );
}

/// Arms task-lock accounting for one real scheduler yield pass.
pub fn arm_yield_thread_lock_probe(thread: u64) {
    assert_ne!(thread, 0, "a yield probe identity must be non-zero");
    assert_eq!(
        YIELD_THREAD_LOCK_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one yield task-lock probe may be armed"
    );
    YIELD_THREAD_LOCK_TARGET.store(thread, Ordering::Relaxed);
    YIELD_THREAD_LOCK_COUNT.store(0, Ordering::Relaxed);
    YIELD_THREAD_LOCK_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Takes task-lock entries from the armed scheduler yield pass.
pub fn take_yield_thread_lock_count() -> Option<u64> {
    if YIELD_THREAD_LOCK_STAGE
        .compare_exchange(
            STAGE_COMPLETE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let count = YIELD_THREAD_LOCK_COUNT.load(Ordering::Relaxed);
    YIELD_THREAD_LOCK_TARGET.store(0, Ordering::Relaxed);
    YIELD_THREAD_LOCK_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(count)
}

pub(crate) fn record_yield_thread_lock(thread: ThreadId) {
    if YIELD_THREAD_LOCK_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && YIELD_THREAD_LOCK_TARGET.load(Ordering::Relaxed) == thread.as_u64()
    {
        YIELD_THREAD_LOCK_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn complete_yield_thread_lock_probe(thread: ThreadId) {
    if YIELD_THREAD_LOCK_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || YIELD_THREAD_LOCK_TARGET.load(Ordering::Relaxed) != thread.as_u64()
    {
        return;
    }
    assert_eq!(
        YIELD_THREAD_LOCK_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_COMPLETE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "the yield task-lock probe completed in an invalid stage"
    );
}

struct IrqOwnerProbe {
    target: AtomicU64,
    thread_sched_entries: AtomicU64,
    run_queue_entries: AtomicU64,
    stage: AtomicU8,
}

#[derive(Clone, Copy)]
struct IrqOwnerProbeEntries {
    thread_sched: u64,
    run_queue: u64,
}

impl IrqOwnerProbe {
    const fn new() -> Self {
        Self {
            target: AtomicU64::new(0),
            thread_sched_entries: AtomicU64::new(0),
            run_queue_entries: AtomicU64::new(0),
            stage: AtomicU8::new(STAGE_IDLE),
        }
    }

    fn arm(&self, target: u64, name: &str) {
        assert_ne!(target, 0, "a task-test {name} identity must be non-zero");
        assert_eq!(
            self.stage.compare_exchange(
                STAGE_IDLE,
                STAGE_CONFIGURING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ),
            Ok(STAGE_IDLE),
            "only one {name} IRQ-owner probe may be armed"
        );
        self.target.store(target, Ordering::Relaxed);
        self.thread_sched_entries.store(0, Ordering::Relaxed);
        self.run_queue_entries.store(0, Ordering::Relaxed);
        self.stage.store(STAGE_ARMED, Ordering::Release);
    }

    fn take(&self) -> Option<IrqOwnerProbeEntries> {
        if self
            .stage
            .compare_exchange(
                STAGE_COMPLETE,
                STAGE_CONFIGURING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return None;
        }
        let entries = IrqOwnerProbeEntries {
            thread_sched: self.thread_sched_entries.load(Ordering::Relaxed),
            run_queue: self.run_queue_entries.load(Ordering::Relaxed),
        };
        self.target.store(0, Ordering::Relaxed);
        self.stage.store(STAGE_IDLE, Ordering::Release);
        Some(entries)
    }

    fn matches(&self, target: ThreadId) -> bool {
        self.stage.load(Ordering::Acquire) == STAGE_ARMED
            && self.target.load(Ordering::Relaxed) == target.as_u64()
    }

    fn record(&self, target: ThreadId, thread_sched: bool, run_queue: bool, name: &str) {
        if !self.matches(target) {
            return;
        }
        self.thread_sched_entries
            .store(u64::from(thread_sched), Ordering::Relaxed);
        self.run_queue_entries
            .store(u64::from(run_queue), Ordering::Relaxed);
        assert_eq!(
            self.stage.compare_exchange(
                STAGE_ARMED,
                STAGE_COMPLETE,
                Ordering::AcqRel,
                Ordering::Acquire,
            ),
            Ok(STAGE_ARMED),
            "{name} IRQ-owner scopes were recorded in an invalid stage"
        );
    }
}

/// Arms one real local deadline publication for lock-entry accounting.
pub fn arm_deadline_publication_probe(cpu: usize) {
    arm_deadline_publication_probe_at_stage(cpu, STAGE_ARMED);
}

/// Repeats one already-due scheduler publication on the real current-CPU base.
pub fn exercise_due_deadline_republication() -> Result<DeadlinePublicationEntries, TaskError> {
    let mut irq = crate::facade::RuntimeIrqGuard::enter();
    let mut cpu = crate::facade::runtime_current_cpu_mut(&mut irq)?;
    cpu.as_mut()
        .exercise_due_scheduler_deadline_republication_for_test()
}

/// Arms lock-entry accounting for the next timed-park base transaction.
///
/// Unrelated scheduler or clockevent work on the same CPU remains outside the
/// sample until the timed-park path explicitly begins its transaction.
pub fn arm_park_deadline_publication_probe(cpu: usize) {
    arm_deadline_publication_probe_at_stage(cpu, STAGE_WAITING_FOR_TRANSACTION);
}

fn arm_deadline_publication_probe_at_stage(cpu: usize, armed_stage: u8) {
    let target = u64::try_from(cpu)
        .ok()
        .and_then(|cpu| cpu.checked_add(1))
        .expect("a task-test CPU identity must fit the encoded probe target");
    assert_eq!(
        DEADLINE_PUBLICATION_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one deadline publication probe may be armed"
    );
    DEADLINE_PUBLICATION_TARGET_CPU.store(target, Ordering::Relaxed);
    DEADLINE_PUBLICATION_OBSERVATION_ENTRIES.store(0, Ordering::Relaxed);
    DEADLINE_PUBLICATION_RT_PERIOD_OBSERVATION_ENTRIES.store(0, Ordering::Relaxed);
    DEADLINE_PUBLICATION_REGISTRATION_ENTRIES.store(0, Ordering::Relaxed);
    DEADLINE_PUBLICATION_ENTRIES.store(0, Ordering::Relaxed);
    DEADLINE_PUBLICATION_STAGE.store(armed_stage, Ordering::Release);
}

/// Deadline-base lock entries observed for one real local publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlinePublicationEntries {
    /// Separate deadline observation locks taken before publication.
    pub observation: u64,
    /// Root RT-period locks taken while deriving the clockevent deadline.
    pub rt_period_observation: u64,
    /// Deadline-base locks that mutate task or kernel timer registration.
    pub registration: u64,
    /// Authoritative publication locks taken by the transaction.
    pub publication: u64,
}

/// Takes deadline-base entries for the armed local publication.
pub fn take_deadline_publication_entries() -> Option<DeadlinePublicationEntries> {
    if DEADLINE_PUBLICATION_STAGE
        .compare_exchange(
            STAGE_COMPLETE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let entries = DeadlinePublicationEntries {
        observation: DEADLINE_PUBLICATION_OBSERVATION_ENTRIES.load(Ordering::Relaxed),
        rt_period_observation: DEADLINE_PUBLICATION_RT_PERIOD_OBSERVATION_ENTRIES
            .load(Ordering::Relaxed),
        registration: DEADLINE_PUBLICATION_REGISTRATION_ENTRIES.load(Ordering::Relaxed),
        publication: DEADLINE_PUBLICATION_ENTRIES.load(Ordering::Relaxed),
    };
    DEADLINE_PUBLICATION_TARGET_CPU.store(0, Ordering::Relaxed);
    DEADLINE_PUBLICATION_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(entries)
}

fn deadline_publication_probe_matches(cpu: crate::CpuId) -> bool {
    DEADLINE_PUBLICATION_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && DEADLINE_PUBLICATION_TARGET_CPU.load(Ordering::Relaxed) == u64::from(cpu.as_u32()) + 1
}

pub(crate) struct ParkDeadlinePublicationProbe {
    cpu: crate::CpuId,
    active: bool,
}

impl ParkDeadlinePublicationProbe {
    pub(crate) fn complete(mut self) {
        if self.active {
            complete_deadline_publication(self.cpu);
            self.active = false;
        }
    }
}

impl Drop for ParkDeadlinePublicationProbe {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if DEADLINE_PUBLICATION_STAGE
            .compare_exchange(STAGE_ARMED, STAGE_IDLE, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            DEADLINE_PUBLICATION_TARGET_CPU.store(0, Ordering::Relaxed);
        }
    }
}

pub(crate) fn begin_park_deadline_publication(cpu: crate::CpuId) -> ParkDeadlinePublicationProbe {
    if DEADLINE_PUBLICATION_TARGET_CPU.load(Ordering::Relaxed) != u64::from(cpu.as_u32()) + 1 {
        return ParkDeadlinePublicationProbe { cpu, active: false };
    }
    let active = DEADLINE_PUBLICATION_STAGE
        .compare_exchange(
            STAGE_WAITING_FOR_TRANSACTION,
            STAGE_ARMED,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok();
    ParkDeadlinePublicationProbe { cpu, active }
}

pub(crate) fn record_deadline_observation_entry(cpu: crate::CpuId) {
    if deadline_publication_probe_matches(cpu) {
        DEADLINE_PUBLICATION_OBSERVATION_ENTRIES.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_deadline_rt_period_lock_entry(cpu: crate::CpuId) {
    if deadline_publication_probe_matches(cpu) {
        DEADLINE_PUBLICATION_RT_PERIOD_OBSERVATION_ENTRIES.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_deadline_registration_entry(cpu: crate::CpuId) {
    if deadline_publication_probe_matches(cpu) {
        DEADLINE_PUBLICATION_REGISTRATION_ENTRIES.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_deadline_publication_entry(cpu: crate::CpuId) {
    if !deadline_publication_probe_matches(cpu) {
        return;
    }
    DEADLINE_PUBLICATION_ENTRIES.fetch_add(1, Ordering::Relaxed);
    complete_deadline_publication(cpu);
}

pub(crate) fn complete_deadline_publication(cpu: crate::CpuId) {
    if !deadline_publication_probe_matches(cpu) {
        return;
    }
    assert_eq!(
        DEADLINE_PUBLICATION_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_COMPLETE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "deadline publication completed in an invalid stage"
    );
}

/// Arms lock-entry accounting for one real local soft-expiry pass.
pub fn arm_deadline_soft_expiry_probe(cpu: usize) {
    let target = u64::try_from(cpu)
        .ok()
        .and_then(|cpu| cpu.checked_add(1))
        .expect("a task-test CPU identity must fit the encoded probe target");
    assert_eq!(
        DEADLINE_SOFT_EXPIRY_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one deadline soft-expiry probe may be armed"
    );
    DEADLINE_SOFT_EXPIRY_TARGET_CPU.store(target, Ordering::Relaxed);
    DEADLINE_SOFT_EXPIRY_ENTRIES.store(0, Ordering::Relaxed);
    DEADLINE_SOFT_EXPIRY_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Takes deadline-base lock entries from one local soft-expiry pass.
pub fn take_deadline_soft_expiry_entries() -> Option<u64> {
    if DEADLINE_SOFT_EXPIRY_STAGE
        .compare_exchange(
            STAGE_COMPLETE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let entries = DEADLINE_SOFT_EXPIRY_ENTRIES.load(Ordering::Relaxed);
    DEADLINE_SOFT_EXPIRY_TARGET_CPU.store(0, Ordering::Relaxed);
    DEADLINE_SOFT_EXPIRY_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(entries)
}

fn deadline_soft_expiry_probe_matches(cpu: crate::CpuId) -> bool {
    DEADLINE_SOFT_EXPIRY_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && DEADLINE_SOFT_EXPIRY_TARGET_CPU.load(Ordering::Relaxed) == u64::from(cpu.as_u32()) + 1
}

pub(crate) fn record_deadline_soft_expiry_entry(cpu: crate::CpuId) {
    if deadline_soft_expiry_probe_matches(cpu) {
        DEADLINE_SOFT_EXPIRY_ENTRIES.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn complete_deadline_soft_expiry_pass(cpu: crate::CpuId) {
    if !deadline_soft_expiry_probe_matches(cpu) {
        return;
    }
    assert_eq!(
        DEADLINE_SOFT_EXPIRY_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_COMPLETE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "deadline soft-expiry accounting completed in an invalid stage"
    );
}

/// Owns one deadline-base accounting probe until it is collected or abandoned.
#[must_use = "dropping the probe restores its global test state"]
pub struct ArmedKtimerSelectionProbe {
    active: bool,
}

impl ArmedKtimerSelectionProbe {
    /// Takes the selected worker pass's deadline-base mutation count.
    pub fn take_base_entries(mut self) -> Option<u64> {
        let entries = take_ktimer_selection_base_entries();
        if entries.is_some() {
            self.active = false;
        }
        entries
    }
}

impl Drop for ArmedKtimerSelectionProbe {
    fn drop(&mut self) {
        if self.active {
            cancel_ktimer_selection_probe();
        }
    }
}

/// Arms deadline-base accounting for the worker pass that claims `timer`.
pub fn arm_ktimer_selection_probe(timer: crate::KernelTimerHandle) -> ArmedKtimerSelectionProbe {
    while KTIMER_SELECTION_STAGE.load(Ordering::Acquire) == STAGE_CANCELLED {
        core::hint::spin_loop();
    }
    assert_eq!(
        KTIMER_SELECTION_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one ktimer selection probe may be armed"
    );
    KTIMER_SELECTION_TARGET_CPU.store(u64::from(timer.owner().as_u32()) + 1, Ordering::Relaxed);
    KTIMER_SELECTION_TARGET_TIMER.store(timer.identity().get(), Ordering::Relaxed);
    KTIMER_SELECTION_BASE_ENTRIES.store(0, Ordering::Relaxed);
    KTIMER_SELECTION_STAGE.store(STAGE_ARMED, Ordering::Release);
    ArmedKtimerSelectionProbe { active: true }
}

/// Takes deadline-base mutation entries from the targeted worker selection.
fn take_ktimer_selection_base_entries() -> Option<u64> {
    if KTIMER_SELECTION_STAGE
        .compare_exchange(
            STAGE_COMPLETE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let entries = KTIMER_SELECTION_BASE_ENTRIES.load(Ordering::Relaxed);
    KTIMER_SELECTION_TARGET_CPU.store(0, Ordering::Relaxed);
    KTIMER_SELECTION_TARGET_TIMER.store(0, Ordering::Relaxed);
    KTIMER_SELECTION_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(entries)
}

fn cancel_ktimer_selection_probe() {
    loop {
        let stage = KTIMER_SELECTION_STAGE.load(Ordering::Acquire);
        match stage {
            STAGE_IDLE | STAGE_CANCELLED => return,
            STAGE_ARMED | STAGE_COMPLETE => {
                if KTIMER_SELECTION_STAGE
                    .compare_exchange(
                        stage,
                        STAGE_CONFIGURING,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    continue;
                }
                clear_ktimer_selection_probe();
                return;
            }
            STAGE_TRANSACTION_ACTIVE => {
                if KTIMER_SELECTION_STAGE
                    .compare_exchange(
                        STAGE_TRANSACTION_ACTIVE,
                        STAGE_CANCELLED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return;
                }
            }
            STAGE_CONFIGURING => core::hint::spin_loop(),
            _ => panic!("ktimer selection cancellation observed an invalid stage"),
        }
    }
}

fn clear_ktimer_selection_probe() {
    KTIMER_SELECTION_BASE_ENTRIES.store(0, Ordering::Relaxed);
    KTIMER_SELECTION_TARGET_CPU.store(0, Ordering::Relaxed);
    KTIMER_SELECTION_TARGET_TIMER.store(0, Ordering::Relaxed);
    KTIMER_SELECTION_STAGE.store(STAGE_IDLE, Ordering::Release);
}

pub(crate) struct KtimerSelectionProbe {
    cpu: crate::CpuId,
    active: bool,
}

impl KtimerSelectionProbe {
    pub(crate) fn complete(mut self, claimed: Option<crate::KernelTimerHandle>) {
        if !self.active {
            return;
        }
        let targeted = claimed.is_some_and(|timer| {
            timer.owner() == self.cpu
                && timer.identity().get() == KTIMER_SELECTION_TARGET_TIMER.load(Ordering::Relaxed)
        });
        let next = if targeted {
            STAGE_COMPLETE
        } else {
            KTIMER_SELECTION_BASE_ENTRIES.store(0, Ordering::Relaxed);
            STAGE_ARMED
        };
        match KTIMER_SELECTION_STAGE.compare_exchange(
            STAGE_TRANSACTION_ACTIVE,
            next,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(STAGE_TRANSACTION_ACTIVE) => {}
            Err(STAGE_CANCELLED) => clear_ktimer_selection_probe(),
            _ => panic!("ktimer selection completed in an invalid stage"),
        }
        self.active = false;
    }
}

impl Drop for KtimerSelectionProbe {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        KTIMER_SELECTION_BASE_ENTRIES.store(0, Ordering::Relaxed);
        match KTIMER_SELECTION_STAGE.compare_exchange(
            STAGE_TRANSACTION_ACTIVE,
            STAGE_ARMED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(STAGE_TRANSACTION_ACTIVE) => {}
            Err(STAGE_CANCELLED) => clear_ktimer_selection_probe(),
            _ => {}
        }
    }
}

pub(crate) fn begin_ktimer_selection_probe(cpu: crate::CpuId) -> KtimerSelectionProbe {
    if KTIMER_SELECTION_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || KTIMER_SELECTION_TARGET_CPU.load(Ordering::Relaxed) != u64::from(cpu.as_u32()) + 1
    {
        return KtimerSelectionProbe { cpu, active: false };
    }
    let active = KTIMER_SELECTION_STAGE
        .compare_exchange(
            STAGE_ARMED,
            STAGE_TRANSACTION_ACTIVE,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok();
    if active {
        KTIMER_SELECTION_BASE_ENTRIES.store(0, Ordering::Relaxed);
    }
    KtimerSelectionProbe { cpu, active }
}

pub(crate) fn record_ktimer_selection_base_entry(cpu: crate::CpuId) {
    if KTIMER_SELECTION_STAGE.load(Ordering::Acquire) == STAGE_TRANSACTION_ACTIVE
        && KTIMER_SELECTION_TARGET_CPU.load(Ordering::Relaxed) == u64::from(cpu.as_u32()) + 1
    {
        KTIMER_SELECTION_BASE_ENTRIES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Arms one real policy transition for owner-delivery accounting.
pub fn arm_rt_policy_delivery_probe(thread: u64) {
    assert_ne!(thread, 0, "an RT-policy probe identity must be non-zero");
    assert_eq!(
        RT_POLICY_DELIVERY_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one RT-policy delivery probe may be armed"
    );
    RT_POLICY_DELIVERY_TARGET.store(thread, Ordering::Relaxed);
    RT_POLICY_DELIVERY_REQUIRED.store(0, Ordering::Relaxed);
    RT_POLICY_DELIVERY_EVENTS.store(0, Ordering::Relaxed);
    RT_POLICY_REQUEST_PUBLICATIONS.store(0, Ordering::Relaxed);
    RT_POLICY_DELIVERY_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Logical owner events published by one RT policy transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtPolicyDeliveryEvents {
    /// The policy transaction required the owner to reconsider its dispatch.
    pub reschedule_required: bool,
    /// The owner was actually asked to reconsider its current dispatch.
    pub reschedule_delivered: bool,
    /// The policy transaction newly activated root scheduler work.
    pub owner_work_required: bool,
    /// The owner was actually asked to publish newly activated scheduler work.
    pub owner_work_delivered: bool,
    /// Scheduler-request publication batches emitted by the policy transaction.
    pub request_publications: u64,
}

/// Takes logical delivery events for the armed RT policy transition.
pub fn take_rt_policy_delivery_events() -> Option<RtPolicyDeliveryEvents> {
    if RT_POLICY_DELIVERY_STAGE
        .compare_exchange(
            STAGE_ARMED,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let required = RT_POLICY_DELIVERY_REQUIRED.load(Ordering::Relaxed);
    let delivered = RT_POLICY_DELIVERY_EVENTS.load(Ordering::Relaxed);
    let request_publications = RT_POLICY_REQUEST_PUBLICATIONS.load(Ordering::Relaxed);
    RT_POLICY_DELIVERY_TARGET.store(0, Ordering::Relaxed);
    RT_POLICY_DELIVERY_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(RtPolicyDeliveryEvents {
        reschedule_required: required & RT_POLICY_RESCHEDULE != 0,
        reschedule_delivered: delivered & RT_POLICY_RESCHEDULE != 0,
        owner_work_required: required & RT_POLICY_OWNER_WORK != 0,
        owner_work_delivered: delivered & RT_POLICY_OWNER_WORK != 0,
        request_publications,
    })
}

pub(crate) fn record_rt_policy_delivery_requirements(
    thread: ThreadId,
    reschedule: bool,
    owner_work: bool,
) {
    if RT_POLICY_DELIVERY_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || RT_POLICY_DELIVERY_TARGET.load(Ordering::Relaxed) != thread.as_u64()
    {
        return;
    }
    let required = (u8::from(reschedule) * RT_POLICY_RESCHEDULE)
        | (u8::from(owner_work) * RT_POLICY_OWNER_WORK);
    RT_POLICY_DELIVERY_REQUIRED.store(required, Ordering::Relaxed);
}

fn record_rt_policy_delivery(thread: ThreadId, event: u8) {
    if RT_POLICY_DELIVERY_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && RT_POLICY_DELIVERY_TARGET.load(Ordering::Relaxed) == thread.as_u64()
    {
        RT_POLICY_DELIVERY_EVENTS.fetch_or(event, Ordering::Relaxed);
    }
}

pub(crate) fn record_rt_policy_reschedule(thread: ThreadId) {
    record_rt_policy_delivery(thread, RT_POLICY_RESCHEDULE);
}

pub(crate) fn record_rt_policy_owner_work(thread: ThreadId) {
    record_rt_policy_delivery(thread, RT_POLICY_OWNER_WORK);
}

pub(crate) fn record_rt_policy_request_publication(thread: ThreadId) {
    if RT_POLICY_DELIVERY_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && RT_POLICY_DELIVERY_TARGET.load(Ordering::Relaxed) == thread.as_u64()
    {
        RT_POLICY_REQUEST_PUBLICATIONS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Arms one CPU for disabled root-RT-bandwidth entry accounting.
pub fn arm_disabled_rt_bandwidth_probe(cpu: usize) {
    let target = u64::try_from(cpu)
        .expect("an RT-bandwidth probe CPU must fit u64")
        .checked_add(1)
        .expect("an RT-bandwidth probe CPU identity must fit u64");
    assert_eq!(
        DISABLED_RT_BANDWIDTH_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one disabled RT-bandwidth probe may be armed"
    );
    DISABLED_RT_BANDWIDTH_TARGET_CPU.store(target, Ordering::Relaxed);
    DISABLED_RT_BANDWIDTH_ACTIVATION_ENTRIES.store(0, Ordering::Relaxed);
    DISABLED_RT_BANDWIDTH_CHARGE_ENTRIES.store(0, Ordering::Relaxed);
    DISABLED_RT_BANDWIDTH_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Calls into disabled root RT bandwidth observed on one CPU.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DisabledRtBandwidthEntries {
    /// Attempts to start a root RT period while bandwidth control was disabled.
    pub activation: u64,
    /// Attempts to charge root RT runtime while bandwidth control was disabled.
    pub charge: u64,
}

/// Takes disabled root-RT-bandwidth entries for the armed CPU.
pub fn take_disabled_rt_bandwidth_entries() -> Option<DisabledRtBandwidthEntries> {
    if DISABLED_RT_BANDWIDTH_STAGE
        .compare_exchange(
            STAGE_ARMED,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let entries = DisabledRtBandwidthEntries {
        activation: DISABLED_RT_BANDWIDTH_ACTIVATION_ENTRIES.load(Ordering::Relaxed),
        charge: DISABLED_RT_BANDWIDTH_CHARGE_ENTRIES.load(Ordering::Relaxed),
    };
    DISABLED_RT_BANDWIDTH_TARGET_CPU.store(0, Ordering::Relaxed);
    DISABLED_RT_BANDWIDTH_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(entries)
}

fn disabled_rt_bandwidth_probe_matches(cpu: crate::CpuId) -> bool {
    DISABLED_RT_BANDWIDTH_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && DISABLED_RT_BANDWIDTH_TARGET_CPU.load(Ordering::Relaxed) == u64::from(cpu.as_u32()) + 1
}

pub(crate) fn record_disabled_rt_bandwidth_activation_entry(cpu: crate::CpuId) {
    if disabled_rt_bandwidth_probe_matches(cpu) {
        DISABLED_RT_BANDWIDTH_ACTIVATION_ENTRIES.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_disabled_rt_bandwidth_charge_entry(cpu: crate::CpuId) {
    if disabled_rt_bandwidth_probe_matches(cpu) {
        DISABLED_RT_BANDWIDTH_CHARGE_ENTRIES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Arms one real context-switch tail for IRQ-owner accounting.
pub fn arm_switch_tail_irq_owner_probe(previous: u64) {
    SWITCH_TAIL_THREAD_SCHED_ACQUIRE_COUNT.store(0, Ordering::Relaxed);
    SWITCH_TAIL_RQ_REACQUIRE_COUNT.store(0, Ordering::Relaxed);
    SWITCH_TAIL_RQ_BATON_COUNT.store(0, Ordering::Relaxed);
    SWITCH_TAIL_IRQ_OWNER_PROBE.arm(previous, "switch-tail");
}

/// Runtime IRQ-owner entries observed inside one real switch tail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SwitchTailIrqOwnerEntries {
    /// Previous-task scheduler locks acquired after the architecture switch.
    pub thread_sched_acquired: u64,
    /// Entries taken by the previous thread scheduler lock.
    pub thread_sched: u64,
    /// Entries taken by a migration runqueue transaction.
    pub run_queue: u64,
    /// New rq acquisitions performed after the architecture switch.
    pub rq_reacquired: u64,
    /// Selection-owned rq lock batons consumed after the architecture switch.
    pub rq_baton_consumed: u64,
}

/// Takes task-sched and rq runtime IRQ-owner entries for the switch tail.
pub fn take_switch_tail_irq_owner_entries() -> Option<SwitchTailIrqOwnerEntries> {
    SWITCH_TAIL_IRQ_OWNER_PROBE
        .take()
        .map(|entries| SwitchTailIrqOwnerEntries {
            thread_sched_acquired: SWITCH_TAIL_THREAD_SCHED_ACQUIRE_COUNT
                .swap(0, Ordering::Relaxed),
            thread_sched: entries.thread_sched,
            run_queue: entries.run_queue,
            rq_reacquired: SWITCH_TAIL_RQ_REACQUIRE_COUNT.swap(0, Ordering::Relaxed),
            rq_baton_consumed: SWITCH_TAIL_RQ_BATON_COUNT.swap(0, Ordering::Relaxed),
        })
}

pub(crate) fn record_switch_tail_thread_sched_acquisition(previous: ThreadId) {
    if SWITCH_TAIL_IRQ_OWNER_PROBE.stage.load(Ordering::Acquire) == STAGE_ARMED
        && SWITCH_TAIL_IRQ_OWNER_PROBE.target.load(Ordering::Relaxed) == previous.as_u64()
    {
        SWITCH_TAIL_THREAD_SCHED_ACQUIRE_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_switch_tail_irq_owner_scopes(
    previous: ThreadId,
    thread_sched: bool,
    run_queue: bool,
    rq_reacquired: bool,
    rq_baton_consumed: bool,
) {
    if !SWITCH_TAIL_IRQ_OWNER_PROBE.matches(previous) {
        return;
    }
    if rq_reacquired {
        SWITCH_TAIL_RQ_REACQUIRE_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    if rq_baton_consumed {
        SWITCH_TAIL_RQ_BATON_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    SWITCH_TAIL_IRQ_OWNER_PROBE
        .thread_sched_entries
        .fetch_add(u64::from(thread_sched), Ordering::Relaxed);
    SWITCH_TAIL_IRQ_OWNER_PROBE
        .run_queue_entries
        .fetch_add(u64::from(run_queue), Ordering::Relaxed);
}

pub(crate) fn complete_switch_tail_irq_owner_probe(previous: ThreadId) {
    if !SWITCH_TAIL_IRQ_OWNER_PROBE.matches(previous) {
        return;
    }
    assert_eq!(
        SWITCH_TAIL_IRQ_OWNER_PROBE.stage.compare_exchange(
            STAGE_ARMED,
            STAGE_COMPLETE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "switch-tail IRQ-owner scopes completed in an invalid stage"
    );
}

/// Arms the Linux `prev->__state` before `prev->on_cpu = 0` ordering probe.
pub fn arm_switch_tail_state_order_probe(previous: u64) {
    assert_ne!(previous, 0, "a switch-tail state identity must be non-zero");
    assert_eq!(
        SWITCH_TAIL_STATE_ORDER_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one switch-tail state-order probe may be armed"
    );
    SWITCH_TAIL_STATE_ORDER_TARGET.store(previous, Ordering::Relaxed);
    SWITCH_TAIL_STATE_OBSERVED_ON_CPU.store(0, Ordering::Relaxed);
    SWITCH_TAIL_STATE_ORDER_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Takes whether outgoing state was observed before releasing `on_cpu`.
pub fn take_switch_tail_state_observed_while_on_cpu() -> Option<bool> {
    if SWITCH_TAIL_STATE_ORDER_STAGE
        .compare_exchange(
            STAGE_COMPLETE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let observed = SWITCH_TAIL_STATE_OBSERVED_ON_CPU.load(Ordering::Relaxed) != 0;
    SWITCH_TAIL_STATE_ORDER_TARGET.store(0, Ordering::Relaxed);
    SWITCH_TAIL_STATE_ORDER_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(observed)
}

pub(crate) fn record_switch_tail_state_observation(previous: ThreadId, on_cpu: bool) {
    if SWITCH_TAIL_STATE_ORDER_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || SWITCH_TAIL_STATE_ORDER_TARGET.load(Ordering::Relaxed) != previous.as_u64()
    {
        return;
    }
    SWITCH_TAIL_STATE_OBSERVED_ON_CPU.store(u8::from(on_cpu), Ordering::Relaxed);
    assert_eq!(
        SWITCH_TAIL_STATE_ORDER_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_COMPLETE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "switch-tail state ordering completed in an invalid stage"
    );
}

/// Arms a pause after one yielding task has left `rq->curr` but before its
/// architecture context switch releases the outgoing stack.
pub fn arm_policy_switch_handoff_probe(previous: u64) {
    assert_ne!(previous, 0, "a policy handoff identity must be non-zero");
    assert_eq!(
        POLICY_SWITCH_HANDOFF_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one policy switch-handoff probe may be armed"
    );
    POLICY_SWITCH_HANDOFF_TARGET.store(previous, Ordering::Relaxed);
    POLICY_SWITCH_HANDOFF_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Returns whether the target has committed its next `rq->curr` and paused
/// before the architecture context switch.
pub fn policy_switch_handoff_paused() -> bool {
    POLICY_SWITCH_HANDOFF_STAGE.load(Ordering::Acquire) == STAGE_SWITCH_HANDOFF_PAUSED
}

/// Returns whether the concurrent policy writer reached the owner-rq boundary.
pub fn policy_switch_handoff_update_waiting() -> bool {
    POLICY_SWITCH_HANDOFF_STAGE.load(Ordering::Acquire) == STAGE_SWITCH_HANDOFF_UPDATE_WAITING
}

/// Releases the switch tail after the policy writer reached the retained rq lock.
pub fn release_policy_switch_handoff() {
    assert_eq!(
        POLICY_SWITCH_HANDOFF_STAGE.compare_exchange(
            STAGE_SWITCH_HANDOFF_UPDATE_WAITING,
            STAGE_SWITCH_HANDOFF_RELEASED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_SWITCH_HANDOFF_UPDATE_WAITING),
        "policy handoff release must follow the owner-rq update attempt"
    );
}

/// Releases a paused switch handoff when the test only needs to observe the
/// committed pre-switch window and has no concurrent policy writer.
pub fn release_policy_switch_handoff_after_observation() {
    assert_eq!(
        POLICY_SWITCH_HANDOFF_STAGE.compare_exchange(
            STAGE_SWITCH_HANDOFF_PAUSED,
            STAGE_SWITCH_HANDOFF_RELEASED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_SWITCH_HANDOFF_PAUSED),
        "observed policy handoff release requires a paused switch"
    );
}

pub(crate) fn record_policy_switch_handoff_update_attempt(thread: ThreadId) {
    if POLICY_SWITCH_HANDOFF_TARGET.load(Ordering::Acquire) != thread.as_u64() {
        return;
    }
    assert_eq!(
        POLICY_SWITCH_HANDOFF_STAGE.compare_exchange(
            STAGE_SWITCH_HANDOFF_PAUSED,
            STAGE_SWITCH_HANDOFF_UPDATE_WAITING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_SWITCH_HANDOFF_PAUSED),
        "policy writer reached the switch handoff outside the paused rq window"
    );
}

pub(crate) fn pause_policy_switch_handoff(previous: ThreadId) {
    if POLICY_SWITCH_HANDOFF_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || POLICY_SWITCH_HANDOFF_TARGET.load(Ordering::Relaxed) != previous.as_u64()
    {
        return;
    }
    assert_eq!(
        POLICY_SWITCH_HANDOFF_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_SWITCH_HANDOFF_PAUSED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "target policy handoff reached an invalid test stage"
    );
    while POLICY_SWITCH_HANDOFF_STAGE.load(Ordering::Acquire) < STAGE_SWITCH_HANDOFF_RELEASED {
        core::hint::spin_loop();
    }
    POLICY_SWITCH_HANDOFF_TARGET.store(0, Ordering::Relaxed);
    POLICY_SWITCH_HANDOFF_STAGE.store(STAGE_IDLE, Ordering::Release);
}

/// Arms a pause after one Linux TTWU wake-list publication has completed.
pub fn arm_owner_wake_publication_probe(thread: ThreadId) {
    assert_eq!(
        OWNER_WAKE_PUBLICATION_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one owner wake publication probe may be armed"
    );
    OWNER_WAKE_PUBLICATION_TARGET.store(thread.as_u64(), Ordering::Relaxed);
    OWNER_WAKE_PUBLICATION_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Returns whether the selected wake-list entry is durable but its publisher
/// has not yet returned through the outer task-context wake API.
pub fn owner_wake_publication_paused() -> bool {
    OWNER_WAKE_PUBLICATION_STAGE.load(Ordering::Acquire) == STAGE_OWNER_WAKE_PUBLISHED
}

/// Releases the publisher after the test has observed the durable wake-list entry.
pub fn release_owner_wake_publication() {
    assert_eq!(
        OWNER_WAKE_PUBLICATION_STAGE.compare_exchange(
            STAGE_OWNER_WAKE_PUBLISHED,
            STAGE_OWNER_WAKE_RELEASED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_OWNER_WAKE_PUBLISHED),
        "owner wake publication release requires a completed publication"
    );
}

pub(crate) fn pause_after_owner_wake_publication(thread: ThreadId) {
    if OWNER_WAKE_PUBLICATION_TARGET.load(Ordering::Acquire) != thread.as_u64()
        || OWNER_WAKE_PUBLICATION_STAGE
            .compare_exchange(
                STAGE_ARMED,
                STAGE_OWNER_WAKE_PUBLISHED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
    {
        return;
    }
    while OWNER_WAKE_PUBLICATION_STAGE.load(Ordering::Acquire) != STAGE_OWNER_WAKE_RELEASED {
        core::hint::spin_loop();
    }
    OWNER_WAKE_PUBLICATION_TARGET.store(0, Ordering::Relaxed);
    OWNER_WAKE_PUBLICATION_STAGE.store(STAGE_IDLE, Ordering::Release);
}

/// Arms a real park commit immediately after its final wake check.
pub fn arm_park_after_final_wake_check(thread: ThreadId) -> Result<(), TaskError> {
    let system = crate::facade::runtime_task_system()?;
    crate::system::arm_park_after_final_wake_check(system, thread);
    Ok(())
}

/// Returns whether the armed park commit reached the post-accounting window.
pub fn park_after_final_wake_check_entered() -> bool {
    crate::system::park_after_final_wake_check_entered()
}

/// Releases the armed park commit after a concurrent wake was published.
pub fn complete_park_after_final_wake_check() {
    crate::system::complete_park_after_final_wake_check();
}

/// Arms one real park immediately after `Blocked` becomes visible while its
/// active entity is still owned by the rq publication transaction.
pub fn arm_park_after_blocked_publication(thread: ThreadId) -> Result<(), TaskError> {
    let system = crate::facade::runtime_task_system()?;
    crate::system::arm_park_after_blocked_publication(system, thread);
    Ok(())
}

/// Returns whether the armed park reached the blocked-publication window.
pub fn park_after_blocked_publication_entered() -> bool {
    crate::system::park_after_blocked_publication_entered()
}

/// Releases the armed blocked-publication park transaction.
pub fn complete_park_after_blocked_publication() {
    crate::system::complete_park_after_blocked_publication();
}

/// Arms a real FIFO/RR park immediately before it reserves detached entity
/// publication under its owner rq.
pub fn arm_park_before_active_publication(thread: ThreadId) -> Result<(), TaskError> {
    let system = crate::facade::runtime_task_system()?;
    crate::system::arm_park_before_active_publication(system, thread);
    Ok(())
}

/// Returns whether the armed park reached its pre-publication rq window.
pub fn park_before_active_publication_entered() -> bool {
    crate::system::park_before_active_publication_entered()
}

/// Releases the armed park to reserve and publish its detached entity.
pub fn complete_park_before_active_publication() {
    crate::system::complete_park_before_active_publication();
}

/// Pauses one selected wait-queue claim after it owns delivery but before its
/// sticky wake publication.
pub fn arm_wait_claim_before_wake(thread: ThreadId) {
    assert_ne!(thread.as_u64(), 0, "a wait-claim target must be live");
    assert_eq!(
        WAIT_CLAIM_BEFORE_WAKE_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one selected wait claim may be paused"
    );
    WAIT_CLAIM_BEFORE_WAKE_TARGET.store(thread.as_u64(), Ordering::Relaxed);
    WAIT_CLAIM_BEFORE_WAKE_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Returns whether the selected wait claim reached the pre-wake window.
pub fn wait_claim_before_wake_entered() -> bool {
    WAIT_CLAIM_BEFORE_WAKE_STAGE.load(Ordering::Acquire) == STAGE_WAIT_CLAIM_PAUSED
}

/// Releases the selected wait claim to publish its sticky wake.
pub fn complete_wait_claim_before_wake() {
    assert_eq!(
        WAIT_CLAIM_BEFORE_WAKE_STAGE.compare_exchange(
            STAGE_WAIT_CLAIM_PAUSED,
            STAGE_WAIT_CLAIM_RELEASED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_WAIT_CLAIM_PAUSED),
        "the selected wait claim was not paused"
    );
}

pub(crate) fn pause_wait_claim_before_wake(thread: ThreadId) {
    if WAIT_CLAIM_BEFORE_WAKE_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || WAIT_CLAIM_BEFORE_WAKE_TARGET.load(Ordering::Relaxed) != thread.as_u64()
    {
        return;
    }
    assert_eq!(
        WAIT_CLAIM_BEFORE_WAKE_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_WAIT_CLAIM_PAUSED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "the selected wait claim reached an invalid stage"
    );
    while WAIT_CLAIM_BEFORE_WAKE_STAGE.load(Ordering::Acquire) < STAGE_WAIT_CLAIM_RELEASED {
        core::hint::spin_loop();
    }
    WAIT_CLAIM_BEFORE_WAKE_TARGET.store(0, Ordering::Relaxed);
    WAIT_CLAIM_BEFORE_WAKE_STAGE.store(STAGE_IDLE, Ordering::Release);
}

/// Arms observation of a task-lock caller waiting for detached entity
/// publication on the selected thread.
pub fn arm_thread_sched_publication_wait(thread: ThreadId) {
    assert_ne!(thread.as_u64(), 0, "a publication-wait target must be live");
    assert_eq!(
        THREAD_SCHED_PUBLICATION_WAIT_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one task-lock publication wait may be armed"
    );
    THREAD_SCHED_PUBLICATION_WAIT_TARGET.store(thread.as_u64(), Ordering::Relaxed);
    THREAD_SCHED_PUBLICATION_WAIT_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Returns whether the target task-lock caller reached publication wait.
pub fn thread_sched_publication_wait_entered() -> bool {
    THREAD_SCHED_PUBLICATION_WAIT_STAGE.load(Ordering::Acquire) == STAGE_COMPLETE
}

/// Returns whether the selected task scheduler lock can be acquired without
/// waiting while its detached entity publication remains pending.
pub fn thread_sched_lock_available(thread: ThreadId) -> Result<bool, TaskError> {
    let available = thread_sched_lock_available_now(thread)?;
    assert_eq!(
        THREAD_SCHED_PUBLICATION_WAIT_STAGE.compare_exchange(
            STAGE_COMPLETE,
            STAGE_IDLE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_COMPLETE),
        "the task-lock availability probe completed before its waiter entered"
    );
    Ok(available)
}

/// Returns whether the selected task scheduler lock can be acquired now.
pub fn thread_sched_lock_available_now(thread: ThreadId) -> Result<bool, TaskError> {
    let system = crate::facade::runtime_task_system()?;
    let handle = system.thread_handle(thread)?;
    Ok(handle.runtime_core_arc().sched().try_lock_state_for_test())
}

pub(crate) fn record_thread_sched_publication_wait(thread: ThreadId) {
    if THREAD_SCHED_PUBLICATION_WAIT_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || THREAD_SCHED_PUBLICATION_WAIT_TARGET.load(Ordering::Relaxed) != thread.as_u64()
    {
        return;
    }
    assert_eq!(
        THREAD_SCHED_PUBLICATION_WAIT_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_COMPLETE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "target task-lock publication wait reached an invalid stage"
    );
    THREAD_SCHED_PUBLICATION_WAIT_TARGET.store(0, Ordering::Relaxed);
}

/// Arms one real task scheduler lock holder for the selected thread.
pub fn arm_thread_sched_lock_hold(thread: ThreadId) {
    assert_ne!(thread.as_u64(), 0, "a task-lock hold target must be live");
    assert_eq!(
        THREAD_SCHED_LOCK_HOLD_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one task scheduler lock holder may be armed"
    );
    THREAD_SCHED_LOCK_HOLD_TARGET.store(thread.as_u64(), Ordering::Relaxed);
    THREAD_SCHED_LOCK_HOLD_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Acquires and holds the selected thread's real scheduler lock until released.
pub fn hold_thread_sched_lock(thread: ThreadId) -> Result<(), TaskError> {
    let system = crate::facade::runtime_task_system()?;
    let handle = system.thread_handle(thread)?;
    handle.runtime_core_arc().sched().hold_lock_for_test();
    Ok(())
}

/// Returns whether the selected task scheduler lock is currently held.
pub fn thread_sched_lock_hold_entered() -> bool {
    THREAD_SCHED_LOCK_HOLD_STAGE.load(Ordering::Acquire) == STAGE_THREAD_SCHED_LOCK_HELD
}

/// Releases the selected task scheduler lock holder.
pub fn complete_thread_sched_lock_hold() {
    assert_eq!(
        THREAD_SCHED_LOCK_HOLD_STAGE.compare_exchange(
            STAGE_THREAD_SCHED_LOCK_HELD,
            STAGE_THREAD_SCHED_LOCK_RELEASED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_THREAD_SCHED_LOCK_HELD),
        "the selected task scheduler lock is not held"
    );
}

pub(crate) fn pause_thread_sched_lock_hold(thread: ThreadId) {
    if THREAD_SCHED_LOCK_HOLD_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || THREAD_SCHED_LOCK_HOLD_TARGET.load(Ordering::Relaxed) != thread.as_u64()
    {
        return;
    }
    assert_eq!(
        THREAD_SCHED_LOCK_HOLD_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_THREAD_SCHED_LOCK_HELD,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "the task scheduler lock holder reached an invalid stage"
    );
    while THREAD_SCHED_LOCK_HOLD_STAGE.load(Ordering::Acquire) < STAGE_THREAD_SCHED_LOCK_RELEASED {
        core::hint::spin_loop();
    }
    THREAD_SCHED_LOCK_HOLD_TARGET.store(0, Ordering::Relaxed);
    THREAD_SCHED_LOCK_HOLD_STAGE.store(STAGE_IDLE, Ordering::Release);
}

/// Arms observation of the rq-only RT park publication decision.
pub fn arm_park_publication_serialization(thread: ThreadId) {
    assert_ne!(thread.as_u64(), 0, "an RT publication target must be live");
    assert_eq!(
        PARK_PUBLICATION_SERIALIZATION_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one RT publication serialization probe may be armed"
    );
    PARK_PUBLICATION_SERIALIZATION_TARGET.store(thread.as_u64(), Ordering::Relaxed);
    PARK_PUBLICATION_SERIALIZATION_OUTCOME.store(0, Ordering::Relaxed);
    PARK_PUBLICATION_SERIALIZATION_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Returns whether the selected rq-only RT park recorded its publication decision.
pub fn park_publication_serialization_observed() -> bool {
    PARK_PUBLICATION_SERIALIZATION_STAGE.load(Ordering::Acquire) == STAGE_COMPLETE
}

/// The task-lock state observed before an rq-only RT park publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParkPublicationSerialization {
    /// The selected task scheduler lock was already owned by another CPU.
    pub task_lock_busy: bool,
    /// The rq-only path started detached-entity publication despite that observation.
    pub publication_started: bool,
}

/// Takes the selected rq-only RT park publication decision.
pub fn take_park_publication_serialization() -> Option<ParkPublicationSerialization> {
    PARK_PUBLICATION_SERIALIZATION_STAGE
        .compare_exchange(
            STAGE_COMPLETE,
            STAGE_IDLE,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .ok()?;
    let outcome = PARK_PUBLICATION_SERIALIZATION_OUTCOME.load(Ordering::Relaxed);
    Some(ParkPublicationSerialization {
        task_lock_busy: outcome & PARK_PUBLICATION_TASK_LOCK_BUSY != 0,
        publication_started: outcome & PARK_PUBLICATION_STARTED != 0,
    })
}

pub(crate) fn park_publication_serialization_armed(thread: ThreadId) -> bool {
    PARK_PUBLICATION_SERIALIZATION_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && PARK_PUBLICATION_SERIALIZATION_TARGET.load(Ordering::Relaxed) == thread.as_u64()
}

pub(crate) fn record_park_publication_serialization(
    thread: ThreadId,
    task_lock_busy: bool,
    publication_started: bool,
) {
    if !park_publication_serialization_armed(thread) {
        return;
    }
    let outcome = (u8::from(task_lock_busy) * PARK_PUBLICATION_TASK_LOCK_BUSY)
        | (u8::from(publication_started) * PARK_PUBLICATION_STARTED);
    PARK_PUBLICATION_SERIALIZATION_OUTCOME.store(outcome, Ordering::Relaxed);
    PARK_PUBLICATION_SERIALIZATION_TARGET.store(0, Ordering::Relaxed);
    assert_eq!(
        PARK_PUBLICATION_SERIALIZATION_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_COMPLETE,
            Ordering::Release,
            Ordering::Relaxed,
        ),
        Ok(STAGE_ARMED),
        "the RT publication serialization probe reached an invalid stage"
    );
}

/// Arms one current-task park preparation for CPU-owner accounting.
pub fn arm_park_prepare_runtime_cpu_probe(thread: u64) {
    assert_ne!(thread, 0, "a park-prepare task identity must be non-zero");
    assert_eq!(
        PARK_PREPARE_RUNTIME_CPU_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one park-prepare RuntimeCpu probe may be armed"
    );
    PARK_PREPARE_RUNTIME_CPU_TARGET.store(thread, Ordering::Relaxed);
    PARK_PREPARE_RUNTIME_CPU_ENTRIES.store(0, Ordering::Relaxed);
    PARK_PREPARE_RUNTIME_CPU_STAGE.store(STAGE_WAITING_FOR_TRANSACTION, Ordering::Release);
}

pub(crate) fn begin_park_prepare_runtime_cpu_probe(thread: ThreadId) {
    if PARK_PREPARE_RUNTIME_CPU_TARGET.load(Ordering::Acquire) != thread.as_u64() {
        return;
    }
    let _ = PARK_PREPARE_RUNTIME_CPU_STAGE.compare_exchange(
        STAGE_WAITING_FOR_TRANSACTION,
        STAGE_ARMED,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
}

pub(crate) fn complete_park_prepare_runtime_cpu_probe(thread: ThreadId) {
    if PARK_PREPARE_RUNTIME_CPU_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || PARK_PREPARE_RUNTIME_CPU_TARGET.load(Ordering::Relaxed) != thread.as_u64()
    {
        return;
    }
    assert_eq!(
        PARK_PREPARE_RUNTIME_CPU_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_COMPLETE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "the park-prepare RuntimeCpu probe completed in an invalid stage"
    );
}

/// Takes `RuntimeCpu` IRQ-owner entries used to publish one park state.
pub fn take_park_prepare_runtime_cpu_entries() -> Option<u64> {
    if PARK_PREPARE_RUNTIME_CPU_STAGE
        .compare_exchange(
            STAGE_COMPLETE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let entries = PARK_PREPARE_RUNTIME_CPU_ENTRIES.load(Ordering::Relaxed);
    PARK_PREPARE_RUNTIME_CPU_TARGET.store(0, Ordering::Relaxed);
    PARK_PREPARE_RUNTIME_CPU_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(entries)
}

pub(crate) fn record_park_prepare_runtime_cpu_entry(thread: ThreadId) {
    if PARK_PREPARE_RUNTIME_CPU_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && PARK_PREPARE_RUNTIME_CPU_TARGET.load(Ordering::Relaxed) == thread.as_u64()
    {
        PARK_PREPARE_RUNTIME_CPU_ENTRIES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Arms one real running-to-blocked park for IRQ-owner accounting.
pub fn arm_park_irq_owner_probe(thread: u64) {
    PARK_THREAD_SCHED_ACQUIRE_COUNT.store(0, Ordering::Relaxed);
    PARK_IRQ_OWNER_PROBE.arm(thread, "park");
}

/// Runtime IRQ-owner entries observed inside one real park transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParkIrqOwnerEntries {
    /// Physical acquisitions of the target task scheduler lock.
    pub thread_sched_acquired: u64,
    /// Entries taken by the target thread scheduler lock.
    pub thread_sched: u64,
    /// Entries taken by the scheduler-frame runqueue transaction.
    pub run_queue: u64,
}

/// Takes task-sched and rq runtime IRQ-owner entries for the park.
pub fn take_park_irq_owner_entries() -> Option<ParkIrqOwnerEntries> {
    PARK_IRQ_OWNER_PROBE
        .take()
        .map(|entries| ParkIrqOwnerEntries {
            thread_sched_acquired: PARK_THREAD_SCHED_ACQUIRE_COUNT.load(Ordering::Relaxed),
            thread_sched: entries.thread_sched,
            run_queue: entries.run_queue,
        })
}

pub(crate) fn record_park_thread_sched_acquisition(thread: ThreadId) {
    if PARK_IRQ_OWNER_PROBE.matches(thread) {
        PARK_THREAD_SCHED_ACQUIRE_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_park_irq_owner_scopes(thread: ThreadId, thread_sched: bool, run_queue: bool) {
    PARK_IRQ_OWNER_PROBE.record(thread, thread_sched, run_queue, "park");
}

/// Arms one direct wake to pause while holding the blocked task lock and then
/// fail its CPU-publication reservation.
pub fn arm_direct_wake_delivery_failure(thread: u64) {
    assert_ne!(thread, 0, "a direct-wake failure identity must be non-zero");
    assert_eq!(
        DIRECT_WAKE_FAILURE_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one direct-wake failure may be armed"
    );
    DIRECT_WAKE_FAILURE_TARGET.store(thread, Ordering::Relaxed);
    DIRECT_WAKE_COALESCED_BLOCKED.store(0, Ordering::Relaxed);
    DIRECT_WAKE_FAILURE_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Returns whether the selected wake is paused while owning the task lock.
pub fn direct_wake_delivery_failure_paused() -> bool {
    DIRECT_WAKE_FAILURE_STAGE.load(Ordering::Acquire) == STAGE_DIRECT_WAKE_FAILURE_PAUSED
}

/// Returns whether another wake coalesced while the target remained blocked.
pub fn direct_wake_coalesced_blocked_observed() -> bool {
    DIRECT_WAKE_COALESCED_BLOCKED.load(Ordering::Acquire) != 0
}

/// Releases the selected wake so its delivery reservation fails.
pub fn release_direct_wake_delivery_failure() {
    assert_eq!(
        DIRECT_WAKE_FAILURE_STAGE.compare_exchange(
            STAGE_DIRECT_WAKE_FAILURE_PAUSED,
            STAGE_DIRECT_WAKE_FAILURE_RELEASED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_DIRECT_WAKE_FAILURE_PAUSED),
        "direct-wake failure release must follow the paused transaction"
    );
}

pub(crate) fn pause_and_fail_direct_wake_delivery(thread: ThreadId) -> bool {
    if DIRECT_WAKE_FAILURE_TARGET.load(Ordering::Acquire) != thread.as_u64()
        || DIRECT_WAKE_FAILURE_STAGE
            .compare_exchange(
                STAGE_ARMED,
                STAGE_DIRECT_WAKE_FAILURE_PAUSED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
    {
        return false;
    }
    while DIRECT_WAKE_FAILURE_STAGE.load(Ordering::Acquire) != STAGE_DIRECT_WAKE_FAILURE_RELEASED {
        core::hint::spin_loop();
    }
    DIRECT_WAKE_FAILURE_TARGET.store(0, Ordering::Relaxed);
    DIRECT_WAKE_FAILURE_STAGE.store(STAGE_IDLE, Ordering::Release);
    true
}

pub(crate) fn record_direct_wake_coalesced_blocked(thread: ThreadId) {
    if DIRECT_WAKE_FAILURE_TARGET.load(Ordering::Acquire) == thread.as_u64()
        && DIRECT_WAKE_FAILURE_STAGE.load(Ordering::Acquire) == STAGE_DIRECT_WAKE_FAILURE_PAUSED
    {
        DIRECT_WAKE_COALESCED_BLOCKED.store(1, Ordering::Release);
    }
}

/// Arms one direct wake to report that it retained Linux `TASK_ON_RQ_QUEUED`.
pub fn arm_direct_wake_on_rq_probe(thread: u64) {
    assert_ne!(thread, 0, "a direct-wake on-rq identity must be non-zero");
    assert_eq!(
        DIRECT_WAKE_ON_RQ_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one direct-wake on-rq probe may be armed"
    );
    DIRECT_WAKE_ON_RQ_TARGET.store(thread, Ordering::Relaxed);
    DIRECT_WAKE_ON_RQ_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Returns whether the selected wake is paused before taking its existing rq.
pub fn direct_wake_on_rq_paused() -> bool {
    DIRECT_WAKE_ON_RQ_STAGE.load(Ordering::Acquire) == STAGE_DIRECT_WAKE_ON_RQ_PAUSED
}

/// Releases the selected wake after switch tail has dropped the existing rq.
pub fn release_direct_wake_on_rq_probe() {
    assert_eq!(
        DIRECT_WAKE_ON_RQ_STAGE.compare_exchange(
            STAGE_DIRECT_WAKE_ON_RQ_PAUSED,
            STAGE_DIRECT_WAKE_ON_RQ_RELEASED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_DIRECT_WAKE_ON_RQ_PAUSED),
        "direct-wake on-rq release must follow the paused transaction"
    );
}

/// Takes and resets one completed existing-rq observation.
pub fn take_direct_wake_on_rq_observation() -> bool {
    if DIRECT_WAKE_ON_RQ_STAGE
        .compare_exchange(
            STAGE_DIRECT_WAKE_ON_RQ_COMPLETE,
            STAGE_IDLE,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return false;
    }
    DIRECT_WAKE_ON_RQ_TARGET.store(0, Ordering::Relaxed);
    true
}

pub(crate) fn record_direct_wake_on_rq(thread: ThreadId) {
    if DIRECT_WAKE_ON_RQ_TARGET.load(Ordering::Acquire) != thread.as_u64() {
        return;
    }
    assert_eq!(
        DIRECT_WAKE_ON_RQ_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_DIRECT_WAKE_ON_RQ_PAUSED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "direct wake reached its existing rq outside the armed probe"
    );
    while DIRECT_WAKE_ON_RQ_STAGE.load(Ordering::Acquire) != STAGE_DIRECT_WAKE_ON_RQ_RELEASED {
        core::hint::spin_loop();
    }
    DIRECT_WAKE_ON_RQ_STAGE.store(STAGE_DIRECT_WAKE_ON_RQ_COMPLETE, Ordering::Release);
}

/// Arms one real blocked-to-runnable wake for IRQ-owner accounting.
pub fn arm_wake_irq_owner_probe(thread: u64) {
    WAKE_IRQ_OWNER_PROBE.arm(thread, "wake");
}

/// Runtime IRQ-owner entries observed inside one real wake transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WakeIrqOwnerEntries {
    /// Entries taken by the target thread scheduler lock.
    pub thread_sched: u64,
    /// Entries taken by runqueue transactions nested below that task lock.
    pub run_queue: u64,
}

/// Takes task-sched and rq runtime IRQ-owner entries for the wake.
pub fn take_wake_irq_owner_entries() -> Option<WakeIrqOwnerEntries> {
    WAKE_IRQ_OWNER_PROBE
        .take()
        .map(|entries| WakeIrqOwnerEntries {
            thread_sched: entries.thread_sched,
            run_queue: entries.run_queue,
        })
}

/// Returns whether one generation-valid target is physically blocked.
pub fn thread_is_blocked(thread: u64) -> bool {
    let thread = ThreadId::from_parts(thread as u32, (thread >> 32) as u32);
    crate::thread_handle(thread).is_ok_and(|handle| handle.state() == ThreadState::Blocked)
}

/// Forces one real Fair park to take Linux's delayed-dequeue branch.
pub fn arm_fair_delay_dequeue(thread: u64) {
    assert_ne!(thread, 0, "a delayed-dequeue identity must be non-zero");
    assert_eq!(
        FAIR_DELAY_DEQUEUE_TARGET
            .compare_exchange(0, thread, Ordering::Release, Ordering::Relaxed,),
        Ok(0),
        "only one Fair delayed-dequeue probe may be armed"
    );
}

/// Returns whether a blocked Fair task still owns Linux `TASK_ON_RQ_QUEUED`.
pub fn thread_is_delayed_fair(thread: u64) -> bool {
    let thread = ThreadId::from_parts(thread as u32, (thread >> 32) as u32);
    crate::thread_handle(thread).is_ok_and(|handle| {
        handle.state() == ThreadState::Blocked
            && handle
                .runtime_core_arc()
                .sched()
                .retains_delayed_fair_membership_for_test()
    })
}

/// Returns whether a single-CPU affinity update has left no rq transfer pending.
pub fn thread_affinity_is_settled_on_cpu(thread: u64, cpu: u32) -> bool {
    let thread = ThreadId::from_parts(thread as u32, (thread >> 32) as u32);
    crate::thread_handle(thread).is_ok_and(|handle| {
        handle
            .runtime_core_arc()
            .affinity_is_settled_on_cpu_for_test(crate::CpuId::new(cpu))
    })
}

/// Returns whether Linux `TASK_ON_RQ_MIGRATING` names the requested CPU.
pub fn thread_has_committed_migration_to_cpu(thread: u64, cpu: u32) -> bool {
    let thread = ThreadId::from_parts(thread as u32, (thread >> 32) as u32);
    crate::thread_handle(thread).is_ok_and(|handle| {
        handle
            .runtime_core_arc()
            .sched()
            .has_committed_migration_to_cpu_for_test(crate::CpuId::new(cpu))
    })
}

pub(crate) fn force_fair_delay_dequeue(thread: ThreadId, natural: bool) -> bool {
    FAIR_DELAY_DEQUEUE_TARGET
        .compare_exchange(thread.as_u64(), 0, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
        || natural
}

/// Arms accounting for premature ktimer-worker yields on one real CPU.
pub fn arm_ktimer_pending_yield_probe(cpu: usize) {
    assert_eq!(
        KTIMER_PENDING_YIELD_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one ktimer pending-yield probe may be armed"
    );
    KTIMER_PENDING_YIELD_TARGET_CPU.store(cpu as u64, Ordering::Relaxed);
    KTIMER_PENDING_YIELD_COUNT.store(0, Ordering::Relaxed);
    KTIMER_PENDING_YIELD_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Returns premature ktimer-worker yields observed while the probe is armed.
pub fn ktimer_pending_yield_count() -> Option<u64> {
    (KTIMER_PENDING_YIELD_STAGE.load(Ordering::Acquire) == STAGE_ARMED)
        .then(|| KTIMER_PENDING_YIELD_COUNT.load(Ordering::Relaxed))
}

/// Takes the premature ktimer-worker yield count and disarms the probe.
pub fn take_ktimer_pending_yield_count() -> Option<u64> {
    if KTIMER_PENDING_YIELD_STAGE
        .compare_exchange(
            STAGE_ARMED,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let count = KTIMER_PENDING_YIELD_COUNT.load(Ordering::Relaxed);
    KTIMER_PENDING_YIELD_TARGET_CPU.store(0, Ordering::Relaxed);
    KTIMER_PENDING_YIELD_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(count)
}

pub(crate) fn record_ktimer_pending_yield(cpu: crate::CpuId) {
    if KTIMER_PENDING_YIELD_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && KTIMER_PENDING_YIELD_TARGET_CPU.load(Ordering::Relaxed) == u64::from(cpu.as_u32())
    {
        KTIMER_PENDING_YIELD_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_wake_irq_owner_scopes(thread: ThreadId, thread_sched: bool, run_queue: bool) {
    WAKE_IRQ_OWNER_PROBE.record(thread, thread_sched, run_queue, "wake");
}

/// Arms temporary scheduling-entity copy accounting for one real direct wake.
pub fn arm_wake_entity_read_copy_probe(thread: u64) {
    assert_ne!(thread, 0, "a wake entity probe identity must be non-zero");
    assert_eq!(
        WAKE_ENTITY_READ_COPY_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one wake entity probe may be armed"
    );
    WAKE_ENTITY_READ_COPY_TARGET.store(thread, Ordering::Relaxed);
    WAKE_ENTITY_READ_COUNT.store(0, Ordering::Relaxed);
    WAKE_ENTITY_READ_COPY_COUNT.store(0, Ordering::Relaxed);
    WAKE_ENTITY_READ_COPY_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Read-only scheduling-entity accesses made by one direct wake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WakeEntityReadEvents {
    /// Placement and preemption reads covered by the probe.
    pub reads: u64,
    /// Reads that created a temporary scheduling-entity value.
    pub copies: u64,
}

/// Takes scheduling-entity reads made by the armed direct wake.
pub fn take_wake_entity_read_events() -> Option<WakeEntityReadEvents> {
    if WAKE_ENTITY_READ_COPY_STAGE
        .compare_exchange(
            STAGE_ARMED,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let reads = WAKE_ENTITY_READ_COUNT.load(Ordering::Relaxed);
    let copies = WAKE_ENTITY_READ_COPY_COUNT.load(Ordering::Relaxed);
    WAKE_ENTITY_READ_COPY_TARGET.store(0, Ordering::Relaxed);
    WAKE_ENTITY_READ_COPY_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(WakeEntityReadEvents { reads, copies })
}

pub(crate) fn record_wake_entity_read(thread: ThreadId, copies: u64) {
    if WAKE_ENTITY_READ_COPY_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && WAKE_ENTITY_READ_COPY_TARGET.load(Ordering::Relaxed) == thread.as_u64()
    {
        WAKE_ENTITY_READ_COUNT.fetch_add(1, Ordering::Relaxed);
        WAKE_ENTITY_READ_COPY_COUNT.fetch_add(copies, Ordering::Relaxed);
    }
}

/// Arms Fair virtual-time maintenance accounting for one real direct wake.
pub fn arm_wake_fair_vtime_probe(thread: u64) {
    assert_ne!(
        thread, 0,
        "a wake Fair-vtime probe identity must be non-zero"
    );
    assert_eq!(
        WAKE_FAIR_VTIME_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one wake Fair-vtime probe may be armed"
    );
    WAKE_FAIR_VTIME_TARGET.store(thread, Ordering::Relaxed);
    WAKE_FAIR_VTIME_UPDATE_COUNT.store(0, Ordering::Relaxed);
    WAKE_FAIR_VTIME_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Takes Fair virtual-time maintenance entries made by the armed direct wake.
pub fn take_wake_fair_vtime_updates() -> Option<u64> {
    if WAKE_FAIR_VTIME_STAGE
        .compare_exchange(
            STAGE_ARMED,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let count = WAKE_FAIR_VTIME_UPDATE_COUNT.load(Ordering::Relaxed);
    WAKE_FAIR_VTIME_TARGET.store(0, Ordering::Relaxed);
    WAKE_FAIR_VTIME_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(count)
}

pub(crate) fn record_wake_fair_vtime_update(thread: ThreadId) {
    if WAKE_FAIR_VTIME_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && WAKE_FAIR_VTIME_TARGET.load(Ordering::Relaxed) == thread.as_u64()
    {
        WAKE_FAIR_VTIME_UPDATE_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

/// Arms Fair virtual-time maintenance accounting for one running thread.
pub fn arm_current_fair_vtime_probe(thread: u64) {
    assert_ne!(
        thread, 0,
        "a current Fair-vtime probe identity must be non-zero"
    );
    assert_eq!(
        CURRENT_FAIR_VTIME_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one current Fair-vtime probe may be armed"
    );
    CURRENT_FAIR_VTIME_TARGET.store(thread, Ordering::Relaxed);
    CURRENT_FAIR_VTIME_UPDATE_COUNT.store(0, Ordering::Relaxed);
    CURRENT_FAIR_VTIME_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Takes Fair virtual-time maintenance entries made for the armed current.
pub fn take_current_fair_vtime_updates() -> Option<u64> {
    if CURRENT_FAIR_VTIME_STAGE
        .compare_exchange(
            STAGE_ARMED,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let count = CURRENT_FAIR_VTIME_UPDATE_COUNT.load(Ordering::Relaxed);
    CURRENT_FAIR_VTIME_TARGET.store(0, Ordering::Relaxed);
    CURRENT_FAIR_VTIME_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(count)
}

pub(crate) fn record_current_fair_vtime_update(thread: ThreadId) {
    if CURRENT_FAIR_VTIME_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && CURRENT_FAIR_VTIME_TARGET.load(Ordering::Relaxed) == thread.as_u64()
    {
        CURRENT_FAIR_VTIME_UPDATE_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

/// Arms preemption accounting for one real equal-priority RT wake.
pub fn arm_equal_rt_wake_probe(thread: u64) {
    arm_equal_rt_wake_probe_inner(thread, false);
}

/// Arms an equal-priority RT wake with owner-only work pending at the rq decision.
pub fn arm_equal_rt_wake_with_owner_work_probe(thread: u64) {
    arm_equal_rt_wake_probe_inner(thread, true);
}

fn arm_equal_rt_wake_probe_inner(thread: u64, inject_owner_work: bool) {
    assert_ne!(
        thread, 0,
        "an equal-priority RT wake identity must be non-zero"
    );
    assert_eq!(
        EQUAL_RT_WAKE_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one equal-priority RT wake probe may be armed"
    );
    EQUAL_RT_WAKE_TARGET.store(thread, Ordering::Relaxed);
    EQUAL_RT_WAKE_RESCHEDULE.store(0, Ordering::Relaxed);
    EQUAL_RT_WAKE_INJECT_OWNER_WORK.store(u8::from(inject_owner_work), Ordering::Relaxed);
    EQUAL_RT_WAKE_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Takes whether the armed equal-priority RT wake requested rescheduling.
pub fn take_equal_rt_wake_reschedule() -> Option<bool> {
    if EQUAL_RT_WAKE_STAGE
        .compare_exchange(
            STAGE_COMPLETE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let requested = EQUAL_RT_WAKE_RESCHEDULE.load(Ordering::Relaxed) != 0;
    EQUAL_RT_WAKE_TARGET.store(0, Ordering::Relaxed);
    EQUAL_RT_WAKE_INJECT_OWNER_WORK.store(0, Ordering::Relaxed);
    EQUAL_RT_WAKE_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(requested)
}

pub(crate) fn take_equal_rt_wake_owner_work_injection(thread: ThreadId) -> bool {
    EQUAL_RT_WAKE_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && EQUAL_RT_WAKE_TARGET.load(Ordering::Relaxed) == thread.as_u64()
        && EQUAL_RT_WAKE_INJECT_OWNER_WORK.swap(0, Ordering::AcqRel) != 0
}

pub(crate) fn record_equal_rt_wake_reschedule(thread: ThreadId, requested: bool) {
    if EQUAL_RT_WAKE_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || EQUAL_RT_WAKE_TARGET.load(Ordering::Relaxed) != thread.as_u64()
    {
        return;
    }
    EQUAL_RT_WAKE_RESCHEDULE.store(u8::from(requested), Ordering::Relaxed);
    assert_eq!(
        EQUAL_RT_WAKE_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_COMPLETE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "the equal-priority RT wake probe completed in an invalid stage"
    );
}

/// Arms target-CPU observation for one real RT wake transaction.
pub fn arm_rt_wake_placement_probe(thread: u64) {
    assert_ne!(thread, 0, "an RT wake identity must be non-zero");
    assert_eq!(
        RT_WAKE_PLACEMENT_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one RT wake placement probe may be armed"
    );
    RT_WAKE_PLACEMENT_TARGET.store(thread, Ordering::Relaxed);
    RT_WAKE_PLACEMENT_CPU.store(0, Ordering::Relaxed);
    RT_WAKE_PLACEMENT_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Takes the CPU selected by the armed RT wake transaction.
pub fn take_rt_wake_placement_cpu() -> Option<u32> {
    if RT_WAKE_PLACEMENT_STAGE
        .compare_exchange(
            STAGE_COMPLETE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let encoded = RT_WAKE_PLACEMENT_CPU.load(Ordering::Relaxed);
    RT_WAKE_PLACEMENT_TARGET.store(0, Ordering::Relaxed);
    RT_WAKE_PLACEMENT_CPU.store(0, Ordering::Relaxed);
    RT_WAKE_PLACEMENT_STAGE.store(STAGE_IDLE, Ordering::Release);
    encoded.checked_sub(1).map(|cpu| cpu as u32)
}

pub(crate) fn record_rt_wake_placement(thread: ThreadId, target: crate::CpuId) {
    if RT_WAKE_PLACEMENT_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || RT_WAKE_PLACEMENT_TARGET.load(Ordering::Relaxed) != thread.as_u64()
    {
        return;
    }
    RT_WAKE_PLACEMENT_CPU.store(u64::from(target.as_u32()) + 1, Ordering::Relaxed);
    assert_eq!(
        RT_WAKE_PLACEMENT_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_COMPLETE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "the RT wake placement probe completed in an invalid stage"
    );
}

/// Arms owner-deadline refresh accounting for one real direct wake.
pub fn arm_wake_owner_deadline_refresh_probe(thread: u64) {
    assert_ne!(thread, 0, "a wake deadline probe identity must be non-zero");
    assert_eq!(
        WAKE_OWNER_DEADLINE_REFRESH_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one wake deadline probe may be armed"
    );
    WAKE_OWNER_DEADLINE_REFRESH_TARGET.store(thread, Ordering::Relaxed);
    WAKE_OWNER_DEADLINE_REFRESH_REQUIRED.store(0, Ordering::Relaxed);
    WAKE_OWNER_DEADLINE_REFRESH_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Takes whether the armed wake made an owner scheduler deadline newly relevant.
pub fn take_wake_owner_deadline_refresh_required() -> Option<bool> {
    if WAKE_OWNER_DEADLINE_REFRESH_STAGE
        .compare_exchange(
            STAGE_COMPLETE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return None;
    }
    let required = WAKE_OWNER_DEADLINE_REFRESH_REQUIRED.load(Ordering::Relaxed) != 0;
    WAKE_OWNER_DEADLINE_REFRESH_TARGET.store(0, Ordering::Relaxed);
    WAKE_OWNER_DEADLINE_REFRESH_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(required)
}

pub(crate) fn record_wake_owner_deadline_refresh(thread: ThreadId, required: bool) {
    if WAKE_OWNER_DEADLINE_REFRESH_STAGE.load(Ordering::Acquire) == STAGE_ARMED
        && WAKE_OWNER_DEADLINE_REFRESH_TARGET.load(Ordering::Relaxed) == thread.as_u64()
    {
        WAKE_OWNER_DEADLINE_REFRESH_REQUIRED.store(u8::from(required), Ordering::Relaxed);
        WAKE_OWNER_DEADLINE_REFRESH_STAGE.store(STAGE_COMPLETE, Ordering::Release);
    }
}

/// Arms the PI release/claim/exit interleaving for one live waiter.
pub fn arm_pi_release_claim_exit(waiter: u64) {
    assert_ne!(waiter, 0, "a task-test waiter identity must be non-zero");
    assert_eq!(
        PI_RELEASE_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one PI release task-test interleaving may be armed"
    );
    TARGET_WAITER.store(waiter, Ordering::Relaxed);
    PI_RELEASE_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Returns whether the target waiter committed its PI registration.
pub fn pi_waiter_registered() -> bool {
    PI_RELEASE_STAGE.load(Ordering::Acquire) == STAGE_WAITER_REGISTERED
}

/// Lets the target waiter observe and claim the ownerless handoff.
pub fn allow_pi_waiter_claim() {
    assert_eq!(
        PI_RELEASE_STAGE.compare_exchange(
            STAGE_RELEASE_BEFORE_WAKE,
            STAGE_WAITER_MAY_CLAIM,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_RELEASE_BEFORE_WAKE),
        "PI waiter claim must follow ownerless release publication"
    );
}

/// Returns whether release published ownerless state but has not woken yet.
pub fn pi_release_before_wake() -> bool {
    PI_RELEASE_STAGE.load(Ordering::Acquire) == STAGE_RELEASE_BEFORE_WAKE
}

/// Lets the releasing task drain its delayed wake after the waiter exits.
pub fn allow_pi_release_wake() {
    assert_eq!(
        PI_RELEASE_STAGE.compare_exchange(
            STAGE_WAITER_MAY_CLAIM,
            STAGE_RELEASE_MAY_WAKE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_WAITER_MAY_CLAIM),
        "PI late wake must follow waiter claim and exit"
    );
}

/// Arms the Linux rtmutex slow-unlock race with the final waiter cancellation.
pub fn arm_pi_cancel_during_release(owner: u64, waiter: u64) {
    assert_ne!(owner, 0, "a PI cancel-race owner identity must be non-zero");
    assert_ne!(
        waiter, 0,
        "a PI cancel-race waiter identity must be non-zero"
    );
    assert_eq!(
        PI_CANCEL_RELEASE_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one PI cancel/release interleaving may be armed"
    );
    PI_CANCEL_RELEASE_OWNER.store(owner, Ordering::Relaxed);
    PI_CANCEL_RELEASE_WAITER.store(waiter, Ordering::Relaxed);
    PI_CANCEL_RELEASE_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Returns whether the final waiter committed its PI registration.
pub fn pi_cancel_waiter_registered() -> bool {
    PI_CANCEL_RELEASE_STAGE.load(Ordering::Acquire) == STAGE_WAITER_REGISTERED
}

/// Returns whether unlock observed the waiter bit and paused before slow release.
pub fn pi_release_observed_cancelable_waiter() -> bool {
    PI_CANCEL_RELEASE_STAGE.load(Ordering::Acquire) == STAGE_RELEASE_BEFORE_WAKE
}

/// Lets unlock retry after the final waiter has cancelled its registration.
pub fn allow_pi_release_after_waiter_cancel() {
    assert_eq!(
        PI_CANCEL_RELEASE_STAGE.compare_exchange(
            STAGE_RELEASE_BEFORE_WAKE,
            STAGE_RELEASE_MAY_WAKE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_RELEASE_BEFORE_WAKE),
        "PI slow release must observe the waiter before cancellation completes"
    );
}

/// Arms a pause after one PI waiter committed its origin-lock registration.
pub fn arm_pi_chain_owner_change(top: u64) {
    assert_ne!(top, 0, "a task-test chain identity must be non-zero");
    assert_eq!(
        PI_CHAIN_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one PI chain task-test interleaving may be armed"
    );
    TARGET_CHAIN_TOP.store(top, Ordering::Relaxed);
    PI_CHAIN_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Returns whether the target waiter committed its chain-walk decision.
pub fn pi_chain_decision_committed() -> bool {
    PI_CHAIN_STAGE.load(Ordering::Acquire) == STAGE_CHAIN_DECIDED
}

/// Lets the target registration continue after the original owner changed.
pub fn allow_pi_chain_owner_change() {
    assert_eq!(
        PI_CHAIN_STAGE.compare_exchange(
            STAGE_CHAIN_DECIDED,
            STAGE_OWNER_MAY_CHANGE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_CHAIN_DECIDED),
        "PI chain continuation must follow origin registration"
    );
}

/// Arms a pause after one PI waiter resolves the current physical owner.
pub fn arm_pi_owner_exit_before_waiter_registration(waiter: u64) {
    assert_ne!(waiter, 0, "a task-test waiter identity must be non-zero");
    assert_eq!(
        PI_OWNER_EXIT_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one PI owner-exit interleaving may be armed"
    );
    PI_OWNER_EXIT_TARGET_WAITER.store(waiter, Ordering::Relaxed);
    PI_OWNER_EXIT_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Returns whether the target waiter retained the observed owner identity.
pub fn pi_owner_snapshot_captured() -> bool {
    PI_OWNER_EXIT_STAGE.load(Ordering::Acquire) == STAGE_OWNER_CAPTURED
}

/// Lets the waiter continue after the observed physical owner has exited.
pub fn allow_pi_waiter_after_owner_exit() {
    assert_eq!(
        PI_OWNER_EXIT_STAGE.compare_exchange(
            STAGE_OWNER_CAPTURED,
            STAGE_OWNER_EXITED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_OWNER_CAPTURED),
        "PI waiter continuation must follow owner exit"
    );
}

/// Arms a pause after a waiter commits its owner-lifetime observation.
pub fn arm_pi_owner_lifetime_after_registration(waiter: u64) {
    assert_ne!(waiter, 0, "a PI owner-lifetime waiter must be non-zero");
    assert_eq!(
        PI_OWNER_LIFETIME_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one PI owner-lifetime probe may be armed"
    );
    PI_OWNER_LIFETIME_TARGET_WAITER.store(waiter, Ordering::Relaxed);
    PI_OWNER_LIFETIME_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Returns whether the waiter committed its owner-lifetime observation.
pub fn pi_owner_lifetime_registration_committed() -> bool {
    PI_OWNER_LIFETIME_STAGE.load(Ordering::Acquire) == STAGE_WAITER_REGISTERED
}

/// Returns whether a committed PI waiter pins the observed owner's identity.
pub fn pi_owner_lifetime_is_pinned(owner: u64) -> bool {
    let owner = ThreadId::from_parts(owner as u32, (owner >> 32) as u32);
    crate::facade::runtime_task_system()
        .and_then(|system| system.thread_external_lease_count_for_test(owner))
        .is_ok_and(|leases| leases != 0)
}

/// Lets the waiter continue after the owner-lifetime invariant is observed.
pub fn allow_pi_waiter_after_owner_lifetime_observation() {
    assert_eq!(
        PI_OWNER_LIFETIME_STAGE.compare_exchange(
            STAGE_WAITER_REGISTERED,
            STAGE_OWNER_EXITED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_WAITER_REGISTERED),
        "PI owner-lifetime observation must follow waiter registration"
    );
}

/// Arms observation of the first owner-spin iteration for one live PI waiter.
pub fn arm_pi_owner_spin(waiter: u64) {
    assert_ne!(
        waiter, 0,
        "a PI owner-spin waiter identity must be non-zero"
    );
    assert_eq!(
        PI_OWNER_SPIN_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one PI owner-spin probe may be armed"
    );
    PI_OWNER_SPIN_TARGET_WAITER.store(waiter, Ordering::Relaxed);
    PI_OWNER_SPIN_ITERATIONS.store(0, Ordering::Relaxed);
    PI_OWNER_SPIN_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Returns whether the target waiter entered owner spinning.
pub fn pi_owner_spin_entered() -> bool {
    PI_OWNER_SPIN_STAGE.load(Ordering::Acquire) == STAGE_WAITER_REGISTERED
}

/// Lets the target waiter continue after the first owner-spin iteration.
pub fn allow_pi_owner_spin() {
    assert_eq!(
        PI_OWNER_SPIN_STAGE.compare_exchange(
            STAGE_WAITER_REGISTERED,
            STAGE_RELEASE_BEFORE_WAKE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_WAITER_REGISTERED),
        "PI owner spinning must enter before it is released"
    );
}

/// Returns owner-spin iterations observed after the probe was armed.
pub fn pi_owner_spin_iterations() -> u64 {
    PI_OWNER_SPIN_ITERATIONS.load(Ordering::Acquire)
}

/// Releases the completed owner-spin observation for the next test.
pub fn finish_pi_owner_spin_probe() {
    assert_eq!(
        PI_OWNER_SPIN_STAGE.compare_exchange(
            STAGE_RELEASE_BEFORE_WAKE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_RELEASE_BEFORE_WAKE),
        "PI owner-spin probe must complete before it is released"
    );
    PI_OWNER_SPIN_TARGET_WAITER.store(0, Ordering::Relaxed);
    PI_OWNER_SPIN_ITERATIONS.store(0, Ordering::Relaxed);
    PI_OWNER_SPIN_STAGE.store(STAGE_IDLE, Ordering::Release);
}

pub(crate) fn record_pi_owner_spin(waiter: ThreadId) {
    if PI_OWNER_SPIN_TARGET_WAITER.load(Ordering::Relaxed) != waiter.as_u64() {
        return;
    }
    PI_OWNER_SPIN_ITERATIONS.fetch_add(1, Ordering::Relaxed);
    if PI_OWNER_SPIN_STAGE
        .compare_exchange(
            STAGE_ARMED,
            STAGE_WAITER_REGISTERED,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        while PI_OWNER_SPIN_STAGE.load(Ordering::Acquire) == STAGE_WAITER_REGISTERED {
            core::hint::spin_loop();
        }
    }
}

pub(crate) fn registered_waiter(waiter: ThreadId) {
    if PI_RELEASE_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || TARGET_WAITER.load(Ordering::Relaxed) != waiter.as_u64()
    {
        return;
    }
    assert_eq!(
        PI_RELEASE_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_WAITER_REGISTERED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "target PI waiter reached registration in an invalid test stage"
    );
    while PI_RELEASE_STAGE.load(Ordering::Acquire) < STAGE_WAITER_MAY_CLAIM {
        core::hint::spin_loop();
    }
}

pub(crate) fn cancel_release_waiter_registered(waiter: ThreadId) {
    if PI_CANCEL_RELEASE_WAITER.load(Ordering::Relaxed) != waiter.as_u64()
        || PI_CANCEL_RELEASE_STAGE.load(Ordering::Acquire) != STAGE_ARMED
    {
        return;
    }
    assert_eq!(
        PI_CANCEL_RELEASE_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_WAITER_REGISTERED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "PI cancel-race waiter registered in an invalid stage"
    );
}

pub(crate) fn pi_cancel_release_observed(waiter: ThreadId) -> bool {
    PI_CANCEL_RELEASE_WAITER.load(Ordering::Relaxed) == waiter.as_u64()
        && PI_CANCEL_RELEASE_STAGE.load(Ordering::Acquire) == STAGE_RELEASE_BEFORE_WAKE
}

pub(crate) fn release_observed_cancelable_waiter(owner: ThreadId) {
    if PI_CANCEL_RELEASE_OWNER.load(Ordering::Relaxed) != owner.as_u64()
        || PI_CANCEL_RELEASE_STAGE.load(Ordering::Acquire) != STAGE_WAITER_REGISTERED
    {
        return;
    }
    assert_eq!(
        PI_CANCEL_RELEASE_STAGE.compare_exchange(
            STAGE_WAITER_REGISTERED,
            STAGE_RELEASE_BEFORE_WAKE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_WAITER_REGISTERED),
        "PI cancel-race release observed waiters in an invalid stage"
    );
    while PI_CANCEL_RELEASE_STAGE.load(Ordering::Acquire) != STAGE_RELEASE_MAY_WAKE {
        core::hint::spin_loop();
    }
    PI_CANCEL_RELEASE_OWNER.store(0, Ordering::Relaxed);
    PI_CANCEL_RELEASE_WAITER.store(0, Ordering::Relaxed);
    PI_CANCEL_RELEASE_STAGE.store(STAGE_IDLE, Ordering::Release);
}

pub(crate) fn release_before_wake(waiter: ThreadId) {
    if PI_RELEASE_STAGE.load(Ordering::Acquire) != STAGE_WAITER_REGISTERED
        || TARGET_WAITER.load(Ordering::Relaxed) != waiter.as_u64()
    {
        return;
    }
    assert_eq!(
        PI_RELEASE_STAGE.compare_exchange(
            STAGE_WAITER_REGISTERED,
            STAGE_RELEASE_BEFORE_WAKE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_WAITER_REGISTERED),
        "target PI release reached late wake in an invalid test stage"
    );
    while PI_RELEASE_STAGE.load(Ordering::Acquire) != STAGE_RELEASE_MAY_WAKE {
        core::hint::spin_loop();
    }
    TARGET_WAITER.store(0, Ordering::Relaxed);
    PI_RELEASE_STAGE.store(STAGE_IDLE, Ordering::Release);
}

pub(crate) fn chain_decision_committed(top: ThreadId) {
    if PI_CHAIN_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || TARGET_CHAIN_TOP.load(Ordering::Relaxed) != top.as_u64()
    {
        return;
    }
    assert_eq!(
        PI_CHAIN_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_CHAIN_DECIDED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "target PI chain reached registration in an invalid test stage"
    );
    while PI_CHAIN_STAGE.load(Ordering::Acquire) != STAGE_OWNER_MAY_CHANGE {
        core::hint::spin_loop();
    }
    TARGET_CHAIN_TOP.store(0, Ordering::Relaxed);
    PI_CHAIN_STAGE.store(STAGE_IDLE, Ordering::Release);
}

pub(crate) fn owner_snapshot_captured(waiter: ThreadId) {
    if PI_OWNER_EXIT_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || PI_OWNER_EXIT_TARGET_WAITER.load(Ordering::Relaxed) != waiter.as_u64()
    {
        return;
    }
    assert_eq!(
        PI_OWNER_EXIT_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_OWNER_CAPTURED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "target PI waiter captured its owner in an invalid test stage"
    );
    while PI_OWNER_EXIT_STAGE.load(Ordering::Acquire) != STAGE_OWNER_EXITED {
        core::hint::spin_loop();
    }
    PI_OWNER_EXIT_TARGET_WAITER.store(0, Ordering::Relaxed);
    PI_OWNER_EXIT_STAGE.store(STAGE_IDLE, Ordering::Release);
}

pub(crate) fn owner_lifetime_registered(waiter: ThreadId) {
    if PI_OWNER_LIFETIME_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || PI_OWNER_LIFETIME_TARGET_WAITER.load(Ordering::Relaxed) != waiter.as_u64()
    {
        return;
    }
    assert_eq!(
        PI_OWNER_LIFETIME_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_WAITER_REGISTERED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "target PI waiter reached owner observation in an invalid test stage"
    );
    while PI_OWNER_LIFETIME_STAGE.load(Ordering::Acquire) != STAGE_OWNER_EXITED {
        core::hint::spin_loop();
    }
    PI_OWNER_LIFETIME_TARGET_WAITER.store(0, Ordering::Relaxed);
    PI_OWNER_LIFETIME_STAGE.store(STAGE_IDLE, Ordering::Release);
}

/// Installs one process-wide callback for temporary target-side park profiling.
pub fn install_park_profile_hook(hook: fn(u8)) {
    let hook = hook as usize;
    match PARK_PROFILE_HOOK.compare_exchange(0, hook, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {}
        Err(installed) => assert_eq!(installed, hook, "park profile hook already installed"),
    }
}

pub(crate) fn record_park_profile_stage(stage: u8) {
    let hook = PARK_PROFILE_HOOK.load(Ordering::Acquire);
    if hook == 0 {
        return;
    }
    // SAFETY: installation accepts exactly this function-pointer type and the
    // process-wide diagnostic hook is never replaced or removed.
    let hook = unsafe { core::mem::transmute::<usize, fn(u8)>(hook) };
    hook(stage);
}

/// Arms complete-rq-snapshot accounting for two RT/Deadline handoff peers.
pub fn arm_linked_pick_full_snapshot_probe(first: ThreadId, second: ThreadId) {
    LINKED_PICK_FULL_SNAPSHOT_COUNT.store(0, Ordering::Release);
    LINKED_PICK_FULL_SNAPSHOT_TARGET_A.store(first.as_u64(), Ordering::Release);
    LINKED_PICK_FULL_SNAPSHOT_TARGET_B.store(second.as_u64(), Ordering::Release);
}

/// Takes the number of complete rq snapshots copied for the armed peers.
pub fn take_linked_pick_full_snapshot_count() -> u64 {
    LINKED_PICK_FULL_SNAPSHOT_TARGET_A.store(0, Ordering::Release);
    LINKED_PICK_FULL_SNAPSHOT_TARGET_B.store(0, Ordering::Release);
    LINKED_PICK_FULL_SNAPSHOT_COUNT.swap(0, Ordering::AcqRel)
}

pub(crate) fn record_linked_pick_full_snapshot(thread: ThreadId) {
    if LINKED_PICK_FULL_SNAPSHOT_SCOPE.load(Ordering::Acquire) == 0 {
        return;
    }
    let thread = thread.as_u64();
    if thread == LINKED_PICK_FULL_SNAPSHOT_TARGET_A.load(Ordering::Acquire)
        || thread == LINKED_PICK_FULL_SNAPSHOT_TARGET_B.load(Ordering::Acquire)
    {
        LINKED_PICK_FULL_SNAPSHOT_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) struct LinkedPickFullSnapshotScope;

impl Drop for LinkedPickFullSnapshotScope {
    fn drop(&mut self) {
        LINKED_PICK_FULL_SNAPSHOT_SCOPE.fetch_sub(1, Ordering::Release);
    }
}

pub(crate) fn enter_linked_pick_full_snapshot_scope() -> LinkedPickFullSnapshotScope {
    LINKED_PICK_FULL_SNAPSHOT_SCOPE.fetch_add(1, Ordering::AcqRel);
    LinkedPickFullSnapshotScope
}
