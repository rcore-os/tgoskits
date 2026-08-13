//! Deterministic scheduler interleavings for target-side task tests.
//!
//! This module is available only through the non-default `task-test-hooks`
//! feature. It controls the real task system; it does not install a modeled
//! runtime or own a second scheduler state.

use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use crate::{ThreadId, ThreadState};

static TARGET_WAITER: AtomicU64 = AtomicU64::new(0);
static PI_RELEASE_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static TARGET_CHAIN_TOP: AtomicU64 = AtomicU64::new(0);
static PI_CHAIN_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static WAKE_IRQ_OWNER_PROBE: IrqOwnerProbe = IrqOwnerProbe::new();
static PARK_IRQ_OWNER_PROBE: IrqOwnerProbe = IrqOwnerProbe::new();
static SWITCH_TAIL_IRQ_OWNER_PROBE: IrqOwnerProbe = IrqOwnerProbe::new();
static DEADLINE_PUBLICATION_TARGET_CPU: AtomicU64 = AtomicU64::new(0);
static DEADLINE_PUBLICATION_OBSERVATION_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DEADLINE_PUBLICATION_RT_PERIOD_OBSERVATION_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DEADLINE_PUBLICATION_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DEADLINE_PUBLICATION_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static RT_POLICY_DELIVERY_TARGET: AtomicU64 = AtomicU64::new(0);
static RT_POLICY_DELIVERY_REQUIRED: AtomicU8 = AtomicU8::new(0);
static RT_POLICY_DELIVERY_EVENTS: AtomicU8 = AtomicU8::new(0);
static RT_POLICY_DELIVERY_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);

const STAGE_IDLE: u8 = 0;
const STAGE_CONFIGURING: u8 = 1;
const STAGE_ARMED: u8 = 2;
const STAGE_WAITER_REGISTERED: u8 = 3;
const STAGE_RELEASE_BEFORE_WAKE: u8 = 4;
const STAGE_WAITER_MAY_CLAIM: u8 = 5;
const STAGE_RELEASE_MAY_WAKE: u8 = 6;
const STAGE_CHAIN_DECIDED: u8 = 3;
const STAGE_OWNER_MAY_CHANGE: u8 = 4;
const STAGE_COMPLETE: u8 = 3;
const RT_POLICY_RESCHEDULE: u8 = 1 << 0;
const RT_POLICY_OWNER_WORK: u8 = 1 << 1;

/// Enters and exits one ordinary preemption scope through the real runtime.
pub fn exercise_preempt_guard() {
    drop(crate::lock::PreemptScope::enter());
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

    fn record(&self, target: ThreadId, thread_sched: bool, run_queue: bool, name: &str) {
        if self.stage.load(Ordering::Acquire) != STAGE_ARMED
            || self.target.load(Ordering::Relaxed) != target.as_u64()
        {
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
    DEADLINE_PUBLICATION_ENTRIES.store(0, Ordering::Relaxed);
    DEADLINE_PUBLICATION_STAGE.store(STAGE_ARMED, Ordering::Release);
}

/// Deadline-base lock entries observed for one real local publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlinePublicationEntries {
    /// Separate deadline observation locks taken before publication.
    pub observation: u64,
    /// Root RT-period locks taken while deriving the clockevent deadline.
    pub rt_period_observation: u64,
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

pub(crate) fn record_deadline_publication_entry(cpu: crate::CpuId) {
    if !deadline_publication_probe_matches(cpu) {
        return;
    }
    DEADLINE_PUBLICATION_ENTRIES.fetch_add(1, Ordering::Relaxed);
    assert_eq!(
        DEADLINE_PUBLICATION_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_COMPLETE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "deadline publication was recorded in an invalid stage"
    );
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
    RT_POLICY_DELIVERY_TARGET.store(0, Ordering::Relaxed);
    RT_POLICY_DELIVERY_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(RtPolicyDeliveryEvents {
        reschedule_required: required & RT_POLICY_RESCHEDULE != 0,
        reschedule_delivered: delivered & RT_POLICY_RESCHEDULE != 0,
        owner_work_required: required & RT_POLICY_OWNER_WORK != 0,
        owner_work_delivered: delivered & RT_POLICY_OWNER_WORK != 0,
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

/// Arms one real context-switch tail for IRQ-owner accounting.
pub fn arm_switch_tail_irq_owner_probe(previous: u64) {
    SWITCH_TAIL_IRQ_OWNER_PROBE.arm(previous, "switch-tail");
}

/// Runtime IRQ-owner entries observed inside one real switch tail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SwitchTailIrqOwnerEntries {
    /// Entries taken by the previous thread scheduler lock.
    pub thread_sched: u64,
    /// Entries taken by a migration runqueue transaction.
    pub run_queue: u64,
}

/// Takes task-sched and rq runtime IRQ-owner entries for the switch tail.
pub fn take_switch_tail_irq_owner_entries() -> Option<SwitchTailIrqOwnerEntries> {
    SWITCH_TAIL_IRQ_OWNER_PROBE
        .take()
        .map(|entries| SwitchTailIrqOwnerEntries {
            thread_sched: entries.thread_sched,
            run_queue: entries.run_queue,
        })
}

pub(crate) fn record_switch_tail_irq_owner_scopes(
    previous: ThreadId,
    thread_sched: bool,
    run_queue: bool,
) {
    SWITCH_TAIL_IRQ_OWNER_PROBE.record(previous, thread_sched, run_queue, "switch-tail");
}

/// Arms one real running-to-blocked park for IRQ-owner accounting.
pub fn arm_park_irq_owner_probe(thread: u64) {
    PARK_IRQ_OWNER_PROBE.arm(thread, "park");
}

/// Runtime IRQ-owner entries observed inside one real park transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParkIrqOwnerEntries {
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
            thread_sched: entries.thread_sched,
            run_queue: entries.run_queue,
        })
}

pub(crate) fn record_park_irq_owner_scopes(thread: ThreadId, thread_sched: bool, run_queue: bool) {
    PARK_IRQ_OWNER_PROBE.record(thread, thread_sched, run_queue, "park");
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

pub(crate) fn record_wake_irq_owner_scopes(thread: ThreadId, thread_sched: bool, run_queue: bool) {
    WAKE_IRQ_OWNER_PROBE.record(thread, thread_sched, run_queue, "wake");
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
