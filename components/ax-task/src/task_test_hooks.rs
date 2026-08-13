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
static WAKE_IRQ_OWNER_TARGET: AtomicU64 = AtomicU64::new(0);
static WAKE_IRQ_OWNER_THREAD_SCHED_ENTRIES: AtomicU64 = AtomicU64::new(0);
static WAKE_IRQ_OWNER_RUN_QUEUE_ENTRIES: AtomicU64 = AtomicU64::new(0);
static WAKE_IRQ_OWNER_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static PARK_IRQ_OWNER_TARGET: AtomicU64 = AtomicU64::new(0);
static PARK_IRQ_OWNER_THREAD_SCHED_ENTRIES: AtomicU64 = AtomicU64::new(0);
static PARK_IRQ_OWNER_RUN_QUEUE_ENTRIES: AtomicU64 = AtomicU64::new(0);
static PARK_IRQ_OWNER_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);

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

/// Arms one real running-to-blocked park for IRQ-owner accounting.
pub fn arm_park_irq_owner_probe(thread: u64) {
    assert_ne!(thread, 0, "a task-test park identity must be non-zero");
    assert_eq!(
        PARK_IRQ_OWNER_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one park IRQ-owner probe may be armed"
    );
    PARK_IRQ_OWNER_TARGET.store(thread, Ordering::Relaxed);
    PARK_IRQ_OWNER_THREAD_SCHED_ENTRIES.store(0, Ordering::Relaxed);
    PARK_IRQ_OWNER_RUN_QUEUE_ENTRIES.store(0, Ordering::Relaxed);
    PARK_IRQ_OWNER_STAGE.store(STAGE_ARMED, Ordering::Release);
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
    if PARK_IRQ_OWNER_STAGE
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
    let entries = ParkIrqOwnerEntries {
        thread_sched: PARK_IRQ_OWNER_THREAD_SCHED_ENTRIES.load(Ordering::Relaxed),
        run_queue: PARK_IRQ_OWNER_RUN_QUEUE_ENTRIES.load(Ordering::Relaxed),
    };
    PARK_IRQ_OWNER_TARGET.store(0, Ordering::Relaxed);
    PARK_IRQ_OWNER_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(entries)
}

pub(crate) fn record_park_irq_owner_scopes(thread: ThreadId, thread_sched: bool, run_queue: bool) {
    if PARK_IRQ_OWNER_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || PARK_IRQ_OWNER_TARGET.load(Ordering::Relaxed) != thread.as_u64()
    {
        return;
    }
    PARK_IRQ_OWNER_THREAD_SCHED_ENTRIES.store(u64::from(thread_sched), Ordering::Relaxed);
    PARK_IRQ_OWNER_RUN_QUEUE_ENTRIES.store(u64::from(run_queue), Ordering::Relaxed);
    assert_eq!(
        PARK_IRQ_OWNER_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_COMPLETE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "park IRQ-owner scopes were recorded in an invalid stage"
    );
}

/// Arms one real blocked-to-runnable wake for IRQ-owner accounting.
pub fn arm_wake_irq_owner_probe(thread: u64) {
    assert_ne!(thread, 0, "a task-test wake identity must be non-zero");
    assert_eq!(
        WAKE_IRQ_OWNER_STAGE.compare_exchange(
            STAGE_IDLE,
            STAGE_CONFIGURING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_IDLE),
        "only one wake IRQ-owner probe may be armed"
    );
    WAKE_IRQ_OWNER_TARGET.store(thread, Ordering::Relaxed);
    WAKE_IRQ_OWNER_THREAD_SCHED_ENTRIES.store(0, Ordering::Relaxed);
    WAKE_IRQ_OWNER_RUN_QUEUE_ENTRIES.store(0, Ordering::Relaxed);
    WAKE_IRQ_OWNER_STAGE.store(STAGE_ARMED, Ordering::Release);
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
    if WAKE_IRQ_OWNER_STAGE
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
    let entries = WakeIrqOwnerEntries {
        thread_sched: WAKE_IRQ_OWNER_THREAD_SCHED_ENTRIES.load(Ordering::Relaxed),
        run_queue: WAKE_IRQ_OWNER_RUN_QUEUE_ENTRIES.load(Ordering::Relaxed),
    };
    WAKE_IRQ_OWNER_TARGET.store(0, Ordering::Relaxed);
    WAKE_IRQ_OWNER_STAGE.store(STAGE_IDLE, Ordering::Release);
    Some(entries)
}

/// Returns whether one generation-valid target is physically blocked.
pub fn thread_is_blocked(thread: u64) -> bool {
    let thread = ThreadId::from_parts(thread as u32, (thread >> 32) as u32);
    crate::thread_handle(thread).is_ok_and(|handle| handle.state() == ThreadState::Blocked)
}

pub(crate) fn record_wake_irq_owner_scopes(thread: ThreadId, thread_sched: bool, run_queue: bool) {
    if WAKE_IRQ_OWNER_STAGE.load(Ordering::Acquire) != STAGE_ARMED
        || WAKE_IRQ_OWNER_TARGET.load(Ordering::Relaxed) != thread.as_u64()
    {
        return;
    }
    WAKE_IRQ_OWNER_THREAD_SCHED_ENTRIES.store(u64::from(thread_sched), Ordering::Relaxed);
    WAKE_IRQ_OWNER_RUN_QUEUE_ENTRIES.store(u64::from(run_queue), Ordering::Relaxed);
    assert_eq!(
        WAKE_IRQ_OWNER_STAGE.compare_exchange(
            STAGE_ARMED,
            STAGE_COMPLETE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ),
        Ok(STAGE_ARMED),
        "wake IRQ-owner scopes were recorded in an invalid stage"
    );
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
