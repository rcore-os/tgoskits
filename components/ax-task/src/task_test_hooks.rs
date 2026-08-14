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
static WAKE_ENTITY_READ_COPY_TARGET: AtomicU64 = AtomicU64::new(0);
static WAKE_ENTITY_READ_COUNT: AtomicU64 = AtomicU64::new(0);
static WAKE_ENTITY_READ_COPY_COUNT: AtomicU64 = AtomicU64::new(0);
static WAKE_ENTITY_READ_COPY_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static PARK_IRQ_OWNER_PROBE: IrqOwnerProbe = IrqOwnerProbe::new();
static SWITCH_TAIL_IRQ_OWNER_PROBE: IrqOwnerProbe = IrqOwnerProbe::new();
static DEADLINE_PUBLICATION_TARGET_CPU: AtomicU64 = AtomicU64::new(0);
static DEADLINE_PUBLICATION_OBSERVATION_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DEADLINE_PUBLICATION_RT_PERIOD_OBSERVATION_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DEADLINE_PUBLICATION_REGISTRATION_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DEADLINE_PUBLICATION_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DEADLINE_PUBLICATION_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static DEADLINE_SOFT_EXPIRY_TARGET_CPU: AtomicU64 = AtomicU64::new(0);
static DEADLINE_SOFT_EXPIRY_ENTRIES: AtomicU64 = AtomicU64::new(0);
static DEADLINE_SOFT_EXPIRY_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static KTIMER_SELECTION_TARGET_CPU: AtomicU64 = AtomicU64::new(0);
static KTIMER_SELECTION_TARGET_TIMER: AtomicU64 = AtomicU64::new(0);
static KTIMER_SELECTION_BASE_ENTRIES: AtomicU64 = AtomicU64::new(0);
static KTIMER_SELECTION_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static RT_POLICY_DELIVERY_TARGET: AtomicU64 = AtomicU64::new(0);
static RT_POLICY_DELIVERY_REQUIRED: AtomicU8 = AtomicU8::new(0);
static RT_POLICY_DELIVERY_EVENTS: AtomicU8 = AtomicU8::new(0);
static RT_POLICY_REQUEST_PUBLICATIONS: AtomicU64 = AtomicU64::new(0);
static RT_POLICY_DELIVERY_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static CURRENT_HANDLE_QUERY_TARGET: AtomicU64 = AtomicU64::new(0);
static CURRENT_HANDLE_QUERY_COUNT: AtomicU64 = AtomicU64::new(0);
static CURRENT_HANDLE_QUERY_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);
static NO_SWITCH_THREAD_LOCK_TARGET: AtomicU64 = AtomicU64::new(0);
static NO_SWITCH_THREAD_LOCK_COUNT: AtomicU64 = AtomicU64::new(0);
static NO_SWITCH_THREAD_LOCK_STAGE: AtomicU8 = AtomicU8::new(STAGE_IDLE);

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
const STAGE_COMPLETE: u8 = 3;
const RT_POLICY_RESCHEDULE: u8 = 1 << 0;
const RT_POLICY_OWNER_WORK: u8 = 1 << 1;

/// Enters and exits one ordinary preemption scope through the real runtime.
pub fn exercise_preempt_guard() {
    drop(crate::lock::PreemptScope::enter());
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
    arm_deadline_publication_probe_at_stage(cpu, STAGE_ARMED);
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
