//! Task-scoped fault injection into real resource providers for QEMU tests.

use core::{
    marker::PhantomData,
    sync::atomic::{AtomicU8, AtomicU64, Ordering},
};

use ax_task::thread::{TaskError, current};

static OWNER: AtomicU64 = AtomicU64::new(0);
static FAILURE: AtomicU8 = AtomicU8::new(0);
static EVENTS: AtomicU64 = AtomicU64::new(0);

/// Recoverable resource stages before a newly created task is published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CreationStage {
    Stack   = 1,
    Tls     = 2,
    Context = 3,
    Bind    = 4,
    Fp      = 8,
    Mm      = 9,
}

/// Actual provider entry and destruction events in one creation transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CreationEvent {
    Stack       = 1,
    Tls         = 2,
    Context     = 3,
    Bind        = 4,
    DropContext = 5,
    DropTls     = 6,
    DropStack   = 7,
    Fp          = 8,
    Mm          = 9,
}

/// One creator-task probe; other tasks and interrupt work remain unaffected.
pub struct ThreadCreationProbe {
    _not_send: PhantomData<*mut ()>,
}

impl ThreadCreationProbe {
    /// Fails the next occurrence of the selected stage for this task.
    pub fn fail_at(stage: CreationStage) -> Result<Self, TaskError> {
        current::validate_blocking_context()?;
        let owner = current::current_thread_id()?.as_u64();
        OWNER
            .compare_exchange(0, owner, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| TaskError::ThreadBusy)?;
        EVENTS.store(0, Ordering::Relaxed);
        FAILURE.store(stage as u8, Ordering::Release);
        Ok(Self {
            _not_send: PhantomData,
        })
    }

    /// Returns the provider event sequence, in execution order.
    pub fn events(&self) -> alloc::vec::Vec<CreationEvent> {
        let mut encoded = EVENTS.load(Ordering::Acquire);
        let mut events = alloc::vec::Vec::new();
        while encoded != 0 {
            events.push(match encoded & 15 {
                1 => CreationEvent::Stack,
                2 => CreationEvent::Tls,
                3 => CreationEvent::Context,
                4 => CreationEvent::Bind,
                5 => CreationEvent::DropContext,
                6 => CreationEvent::DropTls,
                7 => CreationEvent::DropStack,
                8 => CreationEvent::Fp,
                9 => CreationEvent::Mm,
                _ => unreachable!("invalid creation probe event"),
            });
            encoded >>= 4;
        }
        events.reverse();
        events
    }
}

impl Drop for ThreadCreationProbe {
    fn drop(&mut self) {
        FAILURE.store(0, Ordering::Relaxed);
        OWNER.store(0, Ordering::Release);
    }
}

pub(super) fn record(event: CreationEvent) -> bool {
    let owner = OWNER.load(Ordering::Acquire);
    if owner == 0
        || ax_task::runtime::task_runtime::in_hard_irq()
        || current::current_thread_id().map(|id| id.as_u64()) != Ok(owner)
    {
        return false;
    }
    EVENTS
        .try_update(Ordering::AcqRel, Ordering::Acquire, |events| {
            (events >> 60 == 0).then_some((events << 4) | event as u64)
        })
        .expect("creation probe event capacity exhausted");
    FAILURE
        .compare_exchange(event as u8, 0, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

static MM_SWITCHES: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// Samples actual MM preparation paths: KK, KU, UK, UU.
pub fn mm_switch_counts() -> [u64; 4] {
    core::array::from_fn(|index| MM_SWITCHES[index].load(Ordering::Acquire))
}

pub(super) fn record_mm_switch(previous_user: bool, next_user: bool) {
    MM_SWITCHES[usize::from(previous_user) * 2 + usize::from(next_user)]
        .fetch_add(1, Ordering::Release);
}

static IDLE_TARGET: AtomicU64 = AtomicU64::new(0);
const IDLE_AFTER_DRAIN: u64 = 1 << 59;
const IDLE_AT_WAIT: u64 = 1 << 60;
const IDLE_ARMING: u64 = 1 << 61;
const IDLE_DONE: u64 = 1 << 63;
const IDLE_SUCCESS: u64 = 1 << 62;

/// Requests one real idle-owner scheduler offline/online transaction.
/// Consume its result before submitting another probe. This does not power off
/// the processor; IRQ delivery stays excluded until re-online completes.
pub fn request_idle_cpu_round_trip(cpu: usize) -> Result<(), TaskError> {
    request_idle_probe(cpu, 0)
}

/// Publishes scheduler work after idle's drain, before the offline admission.
/// The owner must service that work before attempting the lifecycle transition.
pub fn request_idle_cpu_round_trip_after_work(cpu: usize) -> Result<(), TaskError> {
    request_idle_probe(cpu, IDLE_AFTER_DRAIN)
}

fn request_idle_probe(cpu: usize, phase: u64) -> Result<(), TaskError> {
    current::validate_blocking_context()?;
    if cpu >= ax_hal::cpu_num() {
        return Err(TaskError::InvalidCpu(cpu as u32));
    }
    IDLE_TARGET
        .compare_exchange(
            0,
            (cpu as u64 + 1) | IDLE_ARMING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| TaskError::ThreadBusy)?;
    let target = ax_task::runtime::cpu::RuntimeCpuId::new(cpu as u32);
    if let Err(error) = ax_task::runtime::cpu::notify_idle_cpu_probe(target) {
        // ARMING is not consumable by the idle owner, so failed admission can
        // withdraw this request without racing an offline transaction.
        IDLE_TARGET.store(0, Ordering::Release);
        return Err(error);
    }
    // ARMING remains persistent pending work in the final idle recheck.
    // Make it consumable only after the producer's publication lease ends.
    IDLE_TARGET.store((cpu as u64 + 1) | phase, Ordering::Release);
    Ok(())
}

/// Returns true for a completed cycle and false for a non-quiescent rejection.
pub fn take_idle_cpu_round_trip_result() -> Option<bool> {
    let state = IDLE_TARGET.load(Ordering::Acquire);
    if state & IDLE_DONE == 0 {
        return None;
    }
    IDLE_TARGET
        .compare_exchange(state, 0, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|state| state & IDLE_SUCCESS != 0)
}

pub(super) fn service_idle_cpu_round_trip() {
    // Called only by the idle loop, after its normal scheduler-work drain.
    // SAFETY: idle is permanently bound to this CPU for its entire lifetime.
    let cpu = unsafe { ax_task::runtime::task_runtime::current_cpu_id() }.as_u32();
    let target = u64::from(cpu) + 1;
    let state = IDLE_TARGET.load(Ordering::Acquire);
    if state != target && state != (target | IDLE_AFTER_DRAIN) {
        return;
    }
    let publish_work = state & IDLE_AFTER_DRAIN != 0;
    if publish_work {
        IDLE_TARGET.store(target, Ordering::Release);
    }
    let result = match ax_task::runtime::cpu::probe_idle_cpu_round_trip(publish_work) {
        Ok(()) => IDLE_SUCCESS,
        // No offline transition committed. Preserve the request so the idle
        // loop drains newly published work at its next safe point.
        Err(TaskError::NotReady) => return,
        Err(TaskError::CpuNotQuiescent(_)) => {
            warn!(
                "idle CPU {cpu} offline rejection: {:?}",
                ax_task::runtime::cpu::idle_offline_rejection()
            );
            0
        }
        Err(error) => panic!("idle CPU lifecycle probe failed: {error}"),
    };
    IDLE_TARGET.store((u64::from(cpu) + 1) | IDLE_DONE | result, Ordering::Release);
}

/// Publishes a probe exactly after the idle service point, before final WFI.
/// This deterministic interleaving must be caught by the final idle recheck.
pub fn request_idle_cpu_round_trip_at_wait(cpu: usize) -> Result<(), TaskError> {
    request_idle_probe(cpu, IDLE_AT_WAIT)
}

pub(super) fn publish_idle_probe_at_wait() -> bool {
    // SAFETY: the idle wait transaction holds its preemption guard.
    let cpu = unsafe { ax_task::runtime::task_runtime::current_cpu_id() }.as_u32();
    let ready = u64::from(cpu) + 1;
    IDLE_TARGET
        .compare_exchange(
            ready | IDLE_AT_WAIT,
            ready,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

pub(super) fn idle_probe_pending() -> bool {
    // SAFETY: only called inside the pinned, IRQ-off idle wait transaction.
    let cpu = unsafe { ax_task::runtime::task_runtime::current_cpu_id() }.as_u32();
    let state = IDLE_TARGET.load(Ordering::Acquire);
    let target = u64::from(cpu) + 1;
    state == target
        || state == (target | IDLE_ARMING)
        || state == (target | IDLE_AT_WAIT)
        || state == (target | IDLE_AFTER_DRAIN)
}
