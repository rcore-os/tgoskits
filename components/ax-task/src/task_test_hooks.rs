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
