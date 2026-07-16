//! AxVM-owned per-CPU VM timer service.
//!
//! The scheduler runtime remains the only owner of the hardware one-shot
//! timer. Each CPU has one pinned service thread whose absolute-deadline wait
//! is backed by ax-task. Registrations use a fixed, preallocated slot table;
//! cancellation only changes a generation-tagged atomic state and can safely
//! target the CPU that originally armed the timer after the caller migrates.

extern crate alloc;

use alloc::{boxed::Box, format, sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    ptr::NonNull,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use ax_cpu_local::CpuPin;
use ax_kspin::PreemptGuard;
use ax_lazyinit::LazyInit;
use ax_percpu::CpuIndex;

use crate::{
    TaskInner,
    host::{HostTime, default_host},
    vcpu::PinnedCpuContext,
};

const TIMER_WORKER_STACK_SIZE: usize = 64 * 1024;
const TIMER_CAPACITY_PER_CPU: usize = 4096;

const TOKEN_SLOT_BITS: u32 = 12;
const TOKEN_CPU_BITS: u32 = 8;
const TOKEN_GENERATION_SHIFT: u32 = TOKEN_SLOT_BITS + TOKEN_CPU_BITS;
const TOKEN_SLOT_MASK: usize = (1usize << TOKEN_SLOT_BITS) - 1;
const TOKEN_CPU_MASK: usize = (1usize << TOKEN_CPU_BITS) - 1;
const TOKEN_GENERATION_MASK: u64 = (1u64 << (64 - TOKEN_GENERATION_SHIFT)) - 1;

const CONTROL_STATE_BITS: u32 = 3;
const CONTROL_STATE_MASK: u64 = (1 << CONTROL_STATE_BITS) - 1;
const STATE_FREE: u64 = 0;
const STATE_ARMED: u64 = 1;
const STATE_DISPATCHING: u64 = 2;
const STATE_CANCELLED: u64 = 3;
const STATE_CALLING: u64 = 4;
const STATE_RECLAIMING: u64 = 5;

const _: () = assert!(usize::BITS == 64);
const _: () = assert!(TIMER_CAPACITY_PER_CPU <= 1usize << TOKEN_SLOT_BITS);

/// Callback invoked when an AxVM one-shot timer expires.
pub type VmTimerCallback = Box<dyn FnOnce(Duration) + Send + 'static>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VmTimerToken {
    owner_cpu: usize,
    slot: usize,
    generation: u64,
}

impl VmTimerToken {
    fn new(owner_cpu: usize, slot: usize, generation: u64) -> Option<Self> {
        if owner_cpu > TOKEN_CPU_MASK
            || slot > TOKEN_SLOT_MASK
            || generation == 0
            || generation > TOKEN_GENERATION_MASK
        {
            return None;
        }
        Some(Self {
            owner_cpu,
            slot,
            generation,
        })
    }

    fn into_raw(self) -> usize {
        self.slot
            | (self.owner_cpu << TOKEN_SLOT_BITS)
            | ((self.generation as usize) << TOKEN_GENERATION_SHIFT)
    }

    fn from_raw(raw: usize) -> Option<Self> {
        let slot = raw & TOKEN_SLOT_MASK;
        let owner_cpu = (raw >> TOKEN_SLOT_BITS) & TOKEN_CPU_MASK;
        let generation = (raw >> TOKEN_GENERATION_SHIFT) as u64;
        Self::new(owner_cpu, slot, generation)
    }
}

struct TimerSlot {
    control: AtomicU64,
    deadline_ns: UnsafeCell<u64>,
    callback: UnsafeCell<Option<VmTimerCallback>>,
}

impl TimerSlot {
    fn new() -> Self {
        Self {
            control: AtomicU64::new(pack_control(0, STATE_FREE)),
            deadline_ns: UnsafeCell::new(0),
            callback: UnsafeCell::new(None),
        }
    }

    fn state(control: u64) -> u64 {
        control & CONTROL_STATE_MASK
    }

    fn generation(control: u64) -> u64 {
        control >> CONTROL_STATE_BITS
    }

    fn arm(&self, deadline_ns: u64, callback: VmTimerCallback) -> Result<u64, VmTimerCallback> {
        let control = self.control.load(Ordering::Acquire);
        if Self::state(control) != STATE_FREE {
            return Err(callback);
        }
        let generation = next_generation(Self::generation(control));
        // SAFETY: only the owner CPU accesses payload fields, always while
        // preemption is disabled. FREE proves that no dispatch owns them.
        unsafe {
            *self.deadline_ns.get() = deadline_ns;
            *self.callback.get() = Some(callback);
        }
        self.control
            .store(pack_control(generation, STATE_ARMED), Ordering::Release);
        Ok(generation)
    }

    fn deadline_if_armed(&self) -> Option<(u64, u64)> {
        let control = self.control.load(Ordering::Acquire);
        if Self::state(control) != STATE_ARMED {
            return None;
        }
        // SAFETY: ARMED payload is immutable until the owner wins the exact
        // generation-tagged transition out of this state.
        let deadline_ns = unsafe { *self.deadline_ns.get() };
        Some((deadline_ns, control))
    }

    fn cancel(&self, generation: u64) -> bool {
        loop {
            let control = self.control.load(Ordering::Acquire);
            if Self::generation(control) != generation {
                return false;
            }
            let state = Self::state(control);
            if state == STATE_CANCELLED || state == STATE_RECLAIMING {
                return true;
            }
            if !matches!(state, STATE_ARMED | STATE_DISPATCHING) {
                return false;
            }
            if self
                .control
                .compare_exchange_weak(
                    control,
                    pack_control(generation, STATE_CANCELLED),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return true;
            }
        }
    }

    fn claim_cancelled(&self) -> Option<(u64, VmTimerCallback)> {
        let control = self.control.load(Ordering::Acquire);
        let generation = Self::generation(control);
        if Self::state(control) != STATE_CANCELLED
            || self
                .control
                .compare_exchange(
                    control,
                    pack_control(generation, STATE_RECLAIMING),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
        {
            return None;
        }
        Some((generation, self.take_callback()))
    }

    fn claim_dispatch(&self, control: u64) -> Option<(u64, VmTimerCallback)> {
        let generation = Self::generation(control);
        if self
            .control
            .compare_exchange(
                control,
                pack_control(generation, STATE_DISPATCHING),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return None;
        }
        Some((generation, self.take_callback()))
    }

    fn take_callback(&self) -> VmTimerCallback {
        // SAFETY: the owner CPU has atomically claimed this generation and is
        // the sole payload consumer. Remote cancellation changes only control.
        unsafe { (&mut *self.callback.get()).take() }
            .expect("claimed AxVM timer slot must retain one callback")
    }

    fn finish_without_callback(&self, generation: u64, state: u64) {
        self.control
            .compare_exchange(
                pack_control(generation, state),
                pack_control(generation, STATE_FREE),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("AxVM timer owner lost a claimed slot");
    }
}

// SAFETY: payload fields are owner-CPU-only and are accessed with preemption
// disabled. Remote CPUs observe and update only the generation-tagged atomic
// control word; the per-CPU area and every slot live until shutdown.
unsafe impl Sync for TimerSlot {}

struct TimerWorkerSignal {
    epoch: AtomicUsize,
    wait_queue: crate::HostWaitQueueHandle,
}

impl TimerWorkerSignal {
    const fn new() -> Self {
        Self {
            epoch: AtomicUsize::new(0),
            wait_queue: crate::HostWaitQueueHandle::new(),
        }
    }

    fn notify(&self) {
        self.epoch.fetch_add(1, Ordering::Release);
        crate::host::task::wait_queue_wake(&self.wait_queue, 1);
    }

    fn epoch(&self) -> usize {
        self.epoch.load(Ordering::Acquire)
    }
}

pub(crate) struct PreparedVmTimerState {
    slots: Box<[TimerSlot]>,
    signal: Arc<TimerWorkerSignal>,
}

struct VmTimerState {
    owner_cpu: usize,
    slots: Box<[TimerSlot]>,
    signal: Arc<TimerWorkerSignal>,
}

impl VmTimerState {
    fn install(owner_cpu: usize, prepared: PreparedVmTimerState) -> Self {
        Self {
            owner_cpu,
            slots: prepared.slots,
            signal: prepared.signal,
        }
    }

    fn register(
        &self,
        deadline_ns: u64,
        mut callback: VmTimerCallback,
    ) -> Result<VmTimerToken, VmTimerCallback> {
        for (slot_index, slot) in self.slots.iter().enumerate() {
            match slot.arm(deadline_ns, callback) {
                Ok(generation) => {
                    return Ok(VmTimerToken::new(self.owner_cpu, slot_index, generation)
                        .expect("validated timer owner and fixed slot layout must fit the token"));
                }
                Err(returned) => callback = returned,
            }
        }
        Err(callback)
    }

    fn cancel(&self, token: VmTimerToken) -> bool {
        token.owner_cpu == self.owner_cpu
            && self
                .slots
                .get(token.slot)
                .is_some_and(|slot| slot.cancel(token.generation))
    }

    fn take_action(&self, now_ns: u64) -> Option<TimerAction> {
        for slot in &self.slots {
            if let Some((generation, callback)) = slot.claim_cancelled() {
                return Some(TimerAction::discard(slot, generation, callback));
            }
        }

        let mut earliest: Option<(u64, &TimerSlot, u64)> = None;
        for slot in &self.slots {
            let Some((deadline_ns, control)) = slot.deadline_if_armed() else {
                continue;
            };
            if deadline_ns <= now_ns
                && earliest.is_none_or(|(earliest_ns, ..)| deadline_ns < earliest_ns)
            {
                earliest = Some((deadline_ns, slot, control));
            }
        }
        let (deadline_ns, slot, control) = earliest?;
        let (generation, callback) = slot.claim_dispatch(control)?;
        Some(TimerAction::dispatch(
            slot,
            generation,
            deadline_ns,
            callback,
        ))
    }

    fn next_deadline_ns(&self) -> Option<u64> {
        self.slots
            .iter()
            .filter_map(TimerSlot::deadline_if_armed)
            .map(|(deadline_ns, _)| deadline_ns)
            .min()
    }
}

enum TimerActionKind {
    Dispatch { deadline_ns: u64 },
    Discard,
}

struct TimerAction {
    slot: NonNull<TimerSlot>,
    generation: u64,
    callback: VmTimerCallback,
    kind: TimerActionKind,
}

impl TimerAction {
    fn dispatch(
        slot: &TimerSlot,
        generation: u64,
        deadline_ns: u64,
        callback: VmTimerCallback,
    ) -> Self {
        Self {
            slot: NonNull::from(slot),
            generation,
            callback,
            kind: TimerActionKind::Dispatch { deadline_ns },
        }
    }

    fn discard(slot: &TimerSlot, generation: u64, callback: VmTimerCallback) -> Self {
        Self {
            slot: NonNull::from(slot),
            generation,
            callback,
            kind: TimerActionKind::Discard,
        }
    }

    fn finish(self, now: Duration) {
        let Self {
            slot,
            generation,
            callback,
            kind,
        } = self;
        // SAFETY: timer slots belong to a CPU-lifetime per-CPU object. The
        // action's claimed control state prevents reuse until this method
        // publishes FREE.
        let slot = unsafe { slot.as_ref() };
        match kind {
            TimerActionKind::Discard => {
                drop(callback);
                slot.finish_without_callback(generation, STATE_RECLAIMING);
            }
            TimerActionKind::Dispatch { deadline_ns } => {
                if slot
                    .control
                    .compare_exchange(
                        pack_control(generation, STATE_DISPATCHING),
                        pack_control(generation, STATE_CALLING),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    trace!("handle VM timer event scheduled at {deadline_ns:#x} ns");
                    callback(now);
                    slot.finish_without_callback(generation, STATE_CALLING);
                } else {
                    drop(callback);
                    slot.finish_without_callback(generation, STATE_CANCELLED);
                }
            }
        }
    }
}

#[ax_percpu::def_percpu]
static TIMER_STATE: LazyInit<VmTimerState> = LazyInit::new();

pub(crate) fn prepare_percpu() -> PreparedVmTimerState {
    let mut slots = Vec::with_capacity(TIMER_CAPACITY_PER_CPU);
    slots.resize_with(TIMER_CAPACITY_PER_CPU, TimerSlot::new);
    PreparedVmTimerState {
        slots: slots.into_boxed_slice(),
        signal: Arc::new(TimerWorkerSignal::new()),
    }
}

pub(crate) fn validate_percpu_owner(pinned_cpu: &PinnedCpuContext<'_>) -> crate::AxVmResult {
    let owner_cpu = pinned_cpu.cpu_index_usize();
    if owner_cpu > TOKEN_CPU_MASK {
        return Err(crate::AxVmError::invalid_config(format_args!(
            "host CPU index {owner_cpu} exceeds the VM timer token limit {TOKEN_CPU_MASK}"
        )));
    }
    Ok(())
}

pub(crate) fn install_percpu(pinned_cpu: &PinnedCpuContext<'_>, prepared: PreparedVmTimerState) {
    let owner_cpu = pinned_cpu.cpu_index_usize();
    info!("Initializing AxVM timer service on CPU {owner_cpu}...");
    TIMER_STATE.with_current_ref(pinned_cpu.bound_cpu_pin(), |slot| {
        slot.init_once(VmTimerState::install(owner_cpu, prepared));
    });
}

pub(crate) fn start_percpu_worker(owner_cpu: usize) {
    let mut worker = TaskInner::new(
        timer_worker_main,
        format!("axvm-timer-{owner_cpu}"),
        TIMER_WORKER_STACK_SIZE,
    );
    worker.set_cpumask(crate::host::task::cpu_mask_one_shot(owner_cpu));
    let _worker = crate::host::task::spawn_task(worker);
}

/// Registers one VM timer on the current pinned CPU service.
///
/// Returns a generation-bearing token, or `None` when the fixed per-CPU timer
/// capacity is exhausted.
pub fn register_timer(deadline_ns: u64, callback: VmTimerCallback) -> Option<usize> {
    let preempt_guard = PreemptGuard::new();
    let (owner_cpu, registration, signal) = with_current_state(preempt_guard.cpu_pin(), |state| {
        (
            state.owner_cpu,
            state.register(deadline_ns, callback),
            Arc::clone(&state.signal),
        )
    });
    drop(preempt_guard);
    match registration {
        Ok(token) => {
            signal.notify();
            Some(token.into_raw())
        }
        Err(callback) => {
            drop(callback);
            warn!(
                "AxVM timer capacity exhausted on CPU {} (capacity {})",
                owner_cpu, TIMER_CAPACITY_PER_CPU,
            );
            None
        }
    }
}

/// Cancels a previously registered VM timer without invoking its callback.
pub fn cancel_timer(raw_token: usize) -> bool {
    let Some(token) = VmTimerToken::from_raw(raw_token) else {
        return false;
    };
    let Some(state) = remote_state(token.owner_cpu) else {
        return false;
    };
    let cancelled = state.cancel(token);
    if cancelled {
        state.signal.notify();
    }
    cancelled
}

fn timer_worker_main() {
    let signal = current_signal();
    loop {
        while let Some(action) = take_current_action() {
            action.finish(default_host().monotonic_time());
        }

        let observed_epoch = signal.epoch();
        match current_next_deadline() {
            Some(deadline) => {
                if deadline <= default_host().monotonic_time() {
                    continue;
                }
                let _timed_out = crate::host::task::wait_queue_wait_until_deadline(
                    &signal.wait_queue,
                    deadline,
                    || signal.epoch() != observed_epoch,
                );
            }
            None => crate::host::task::wait_queue_wait_until(&signal.wait_queue, || {
                signal.epoch() != observed_epoch
            }),
        }
    }
}

fn take_current_action() -> Option<TimerAction> {
    let preempt_guard = PreemptGuard::new();
    let now_ns = duration_nanos(default_host().monotonic_time());
    with_current_state(preempt_guard.cpu_pin(), |state| state.take_action(now_ns))
}

fn current_signal() -> Arc<TimerWorkerSignal> {
    let preempt_guard = PreemptGuard::new();
    with_current_state(preempt_guard.cpu_pin(), |state| Arc::clone(&state.signal))
}

fn current_next_deadline() -> Option<Duration> {
    let preempt_guard = PreemptGuard::new();
    with_current_state(preempt_guard.cpu_pin(), |state| {
        state.next_deadline_ns().map(Duration::from_nanos)
    })
}

fn remote_state(owner_cpu: usize) -> Option<&'static VmTimerState> {
    let cpu = CpuIndex::from_u32(u32::try_from(owner_cpu).ok()?)?;
    let slot = TIMER_STATE.remote_ptr(cpu).ok()?;
    // SAFETY: the per-CPU area and once-initialized slot live until shutdown.
    // VmTimerState is Sync: remote cancellation only touches atomic slot state
    // and immutable worker notification data.
    unsafe { (&*slot).get() }
}

fn with_current_state<R>(cpu_pin: &CpuPin, operation: impl FnOnce(&VmTimerState) -> R) -> R {
    let bound_cpu_pin = ax_percpu::bound_current(cpu_pin)
        .expect("AxVM timer access requires a bound CPU-local area");
    TIMER_STATE.with_current_ref(&bound_cpu_pin, |slot| {
        operation(
            slot.get()
                .expect("AxVM timer service must be initialized on this CPU"),
        )
    })
}

const fn pack_control(generation: u64, state: u64) -> u64 {
    (generation << CONTROL_STATE_BITS) | state
}

fn next_generation(current: u64) -> u64 {
    let next = current.wrapping_add(1) & TOKEN_GENERATION_MASK;
    if next == 0 { 1 } else { next }
}

fn duration_nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn test_state(owner_cpu: usize, capacity: usize) -> VmTimerState {
        let mut slots = Vec::with_capacity(capacity);
        slots.resize_with(capacity, TimerSlot::new);
        VmTimerState {
            owner_cpu,
            slots: slots.into_boxed_slice(),
            signal: Arc::new(TimerWorkerSignal::new()),
        }
    }

    #[test]
    fn token_preserves_owner_slot_and_generation() {
        let token = VmTimerToken::new(7, 123, 0xabc).unwrap();
        assert_eq!(VmTimerToken::from_raw(token.into_raw()), Some(token));
    }

    #[test]
    fn cancellation_after_dequeue_suppresses_callback_before_invocation() {
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = Arc::clone(&calls);
        let state = test_state(3, 1);
        let Ok(token) = state.register(
            10,
            Box::new(move |_| {
                callback_calls.fetch_add(1, Ordering::Relaxed);
            }),
        ) else {
            panic!("the single test timer slot must be available");
        };
        let action = state.take_action(10).unwrap();

        assert!(state.cancel(token));
        action.finish(Duration::from_nanos(10));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(
            TimerSlot::state(state.slots[0].control.load(Ordering::Acquire)),
            STATE_FREE,
        );
    }

    #[test]
    fn stale_generation_cannot_cancel_a_reused_slot() {
        let calls = Arc::new(AtomicUsize::new(0));
        let state = test_state(1, 1);
        let Ok(stale) = state.register(1, Box::new(|_| {})) else {
            panic!("the single test timer slot must be available");
        };
        assert!(state.cancel(stale));
        state
            .take_action(0)
            .expect("cancelled event must be reclaimed")
            .finish(Duration::ZERO);

        let callback_calls = Arc::clone(&calls);
        let Ok(current) = state.register(
            2,
            Box::new(move |_| {
                callback_calls.fetch_add(1, Ordering::Relaxed);
            }),
        ) else {
            panic!("the reclaimed test timer slot must be reusable");
        };
        assert_ne!(stale.generation, current.generation);
        assert!(!state.cancel(stale));
        state
            .take_action(2)
            .unwrap()
            .finish(Duration::from_nanos(2));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
