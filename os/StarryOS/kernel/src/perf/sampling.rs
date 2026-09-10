//! PMU overflow-IRQ sampling backend (`perf record`).
//!
//! This is the IRQ half of hardware-PMU sampling. A sampling perf event
//! ([`super::hw::HwPerfEvent`] with `sample_period > 0`) preloads a programmable
//! counter so it overflows after `period` events; the overflow raises the PMUv3
//! interrupt (PPI 7 / INTID 23). [`pmu_overflow_handler`] runs in hard-IRQ
//! context, reads the interrupted PC, builds one `PERF_RECORD_SAMPLE` per
//! overflowed counter, writes it into that event's mmap ring buffer, re-arms the
//! counter, and wakes a deferred worker (via [`crate::task::future::IrqNotify`]) that
//! delivers `POLLIN` to userspace pollers.
//!
//! The record emitted honours the event's `attr.sample_type`: [`build_sample`]
//! lays out every requested scalar field in the canonical `man perf_event_open`
//! order, so the real `perf` tool — which always sets `IP|TID|TIME|PERIOD` —
//! parses the stream and reports samples. The supported field set is
//! [`SUPPORTED_SAMPLE_TYPE`]; an unsupported bit is rejected at open in
//! [`super::hw`]. A `sample_type` of exactly `PERF_SAMPLE_IP` still yields the
//! original 16-byte IP-only record.
//!
//! IRQ-context discipline (enforced throughout this module's handler path):
//! no allocation, no sleeping locks, and the interrupted `ELR_EL1` / `SPSR_EL1`
//! are read *first* (before touching the PMU or memory) so a nested fault can
//! never clobber them.
//!
//! # Per-CPU registry
//!
//! The handler must locate the ring buffer for an overflowed counter `n` without
//! allocating or taking a lock. [`REGISTRY`] is one fixed generation-bearing
//! registry per CPU (index = programmable counter index). Each [`SampleSlot`]
//! owns strong output and notification references rather than borrowing raw
//! callback storage. `register` / `unregister` mutate the current CPU's registry
//! under a local-IRQ-off critical section ([`NoPreemptIrqSave`]) so removal is
//! also the local hard-IRQ grace period.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use ax_hal::irq::{IrqContext, IrqId, IrqReturn};
use kbpf_basic::linux_bpf::perf_event_mmap_page;

use super::{
    counting::CounterExtender,
    output::PerfRingOutput,
    sampling_lifecycle::SampleRegistration,
    sampling_registry::{RegisterError, SamplingRegistry, UnregisterError},
    target::PerfCpuId,
};
use crate::{
    sync::{IrqMutex, NoPreemptIrqSave},
    task::{PidNamespaceId, TgidNumber, TidNumber, future::IrqNotify, try_current_user_irq_view},
};

fn pmu_irq() -> Result<IrqId, ax_hal::irq::IrqError> {
    ax_hal::pmu::irq()
}

/// Maximum programmable counter index (matches [`ax_cpu::pmu::counter`] /
/// [`ax_cpu::pmu::overflow`]); the registry is sized one past this for indexing.
const MAX_COUNTER: usize = 30;

/// Counts sampling events independently of overflow delivery and period reloads.
/// Owner-CPU callers hold their PMU lease; the IRQ-safe lock serializes live
/// reads with overflow service. No registry lookup is needed by group callbacks.
#[derive(Debug)]
pub(crate) struct SamplingCount(IrqMutex<SamplingCountState>);

#[derive(Debug, Default)]
struct SamplingCountState {
    previous: u32,
    total: u64,
    remaining: i64,
}

impl SamplingCountState {
    fn update(&mut self, raw: u32) -> u64 {
        let delta = raw.wrapping_sub(self.previous);
        self.total = self.total.saturating_add(u64::from(delta));
        self.remaining = self.remaining.saturating_sub(i64::from(delta));
        self.previous = raw;
        self.total
    }

    fn hardware_period(&self) -> u32 {
        // Like armpmu_event_set_period(), leave half the counter range for
        // interrupt delivery latency before modular subtraction becomes ambiguous.
        self.remaining.clamp(1, i64::from(u32::MAX >> 1)) as u32
    }
}

impl SamplingCount {
    pub(crate) fn new() -> Self {
        Self(IrqMutex::new(SamplingCountState::default()))
    }

    pub(crate) fn reset(&self) {
        *self.0.lock() = SamplingCountState::default();
    }

    pub(crate) fn value(&self) -> u64 {
        self.0.lock().total
    }

    /// Accounts the current raw value, including a wrap from the preload.
    pub(crate) fn update(&self, index: usize) -> u64 {
        let mut state = self.0.lock();
        let raw = ax_cpu::pmu::counter::read(index) as u32;
        state.update(raw)
    }

    /// Reloads a stopped counter without charging the preload as events.
    pub(crate) fn preload(&self, index: usize, period: u32) {
        let mut state = self.0.lock();
        state.remaining = i64::from(period);
        Self::program_chunk(index, &mut state);
    }

    fn period_complete(&self) -> bool {
        self.0.lock().remaining <= 0
    }

    /// Reloads a hardware chunk, retaining progress and interrupt overshoot.
    fn rearm(&self, index: usize, period: u32) {
        let mut state = self.0.lock();
        if state.remaining <= 0 {
            let period = i64::from(period);
            state.remaining = if state.remaining <= -period {
                period
            } else {
                state.remaining + period
            };
        }
        Self::program_chunk(index, &mut state);
    }

    fn program_chunk(index: usize, state: &mut SamplingCountState) {
        let chunk = state.hardware_period();
        ax_cpu::pmu::counter::preload(index, chunk);
        state.previous = 0u32.wrapping_sub(chunk);
    }
}

/// Minimum sampling period for frequency mode. Floors the adaptive control loop
/// so a rare event cannot drive the period to 0 (which would re-arm the counter
/// to overflow only after a full `2^32` wrap, i.e. effectively never). `1`
/// matches Linux's lower bound — a counter preloaded to overflow after a single
/// event.
const MIN_FREQ_PERIOD: u32 = 1;
/// Maximum sampling period: the programmable counter is 32-bit, so the preload
/// `(0u32).wrapping_sub(period)` requires `period <= u32::MAX`.
const MAX_SAMPLE_PERIOD: u32 = u32::MAX;
/// Upper bound on a frequency-mode target rate (Hz). Mirrors the advertised
/// `/proc/sys/kernel/perf_event_max_sample_rate`; a wild `sample_freq` is clamped
/// here rather than rejected so `perf` still records.
pub const MAX_TARGET_FREQ: u32 = 100_000;

/// Initial period estimate for a frequency-mode event targeting `freq` Hz.
///
/// Assumes a ~1 GHz event rate as the starting point (so e.g. `-F 4000` starts
/// at `250_000`); [`pmu_overflow_handler`] adapts from here within a few samples.
/// Clamped so a degenerate `freq` cannot produce a 0 period.
pub fn initial_period_for_freq(freq: u32) -> u32 {
    (1_000_000_000u64 / freq.max(1) as u64).clamp(MIN_FREQ_PERIOD as u64, MAX_SAMPLE_PERIOD as u64)
        as u32
}

/// Next adaptive period after a frequency-mode sample (Linux `perf_adjust_period`).
///
/// `cur` events elapsed over `delta_ns` ns produced exactly one sample; to hit
/// `target_freq` samples/sec the ideal period is `cur * 1e9 / (delta_ns *
/// target_freq)`. The move toward that ideal is damped by 1/8 to avoid
/// oscillation, then clamped to a valid 32-bit period. All integer math (IRQ
/// context): the `u128` intermediate cannot overflow for `cur,delta_ns <= u64`.
fn next_freq_period(cur: u32, target_freq: u32, delta_ns: u64) -> u32 {
    if delta_ns == 0 || target_freq == 0 {
        return cur;
    }
    let ideal = (cur as u128 * 1_000_000_000u128) / (delta_ns as u128 * target_freq as u128);
    let ideal = ideal.clamp(MIN_FREQ_PERIOD as u128, MAX_SAMPLE_PERIOD as u128) as i64;
    // Damp by 1/8 toward the ideal (the `+7` biases the truncating divide so a
    // small positive gap still nudges the period up; it converges either way).
    let delta = (ideal - cur as i64 + 7) / 8;
    (cur as i64 + delta).clamp(MIN_FREQ_PERIOD as i64, MAX_SAMPLE_PERIOD as i64) as u32
}

/// `PERF_RECORD_SAMPLE` discriminant (`perf_event_type::PERF_RECORD_SAMPLE`).
const PERF_RECORD_SAMPLE: u32 = 9;
/// `PERF_RECORD_LOST`: dropped samples since the previous loss record.
const PERF_RECORD_LOST: u32 = 2;
/// `PERF_RECORD_MISC_KERNEL`: the sample landed in kernel (EL1) context.
const PERF_RECORD_MISC_KERNEL: u16 = 1;
/// `PERF_RECORD_MISC_USER`: the sample landed in user (EL0) context.
const PERF_RECORD_MISC_USER: u16 = 2;

/// Upper bound on a single `PERF_RECORD_SAMPLE` we emit: 8-byte header plus at
/// most nine 8-byte scalar fields (IDENTIFIER, IP, TID(pid+tid), TIME, ADDR, ID,
/// STREAM_ID, CPU(cpu+res), PERIOD), plus the REGS_USER ABI and saved LR.
/// [`build_sample`] writes into a stack buffer of this size and returns the
/// actual length.
const MAX_STACK_DEPTH: usize = 64;
const MAX_CALLCHAIN_ENTRIES: usize = 1 + MAX_STACK_DEPTH;
pub const MAX_SAMPLE_READ_EVENTS: usize = 31;
const SAMPLE_READ_MAX_U64S: usize = 3 + MAX_SAMPLE_READ_EVENTS * 3;
const SAMPLE_RECORD_MAX_LEN: usize =
    8 + 9 * 8 + SAMPLE_READ_MAX_U64S * 8 + (1 + MAX_CALLCHAIN_ENTRIES) * 8 + 16;
const LOST_RECORD_LEN: usize = 8 + 2 * 8;

/// Per-source loss accounting, independent of a possibly shared output ring.
#[derive(Debug)]
pub struct LossState {
    pending: AtomicU64,
    total: AtomicU64,
}

impl LossState {
    pub const fn new() -> Self {
        Self {
            pending: AtomicU64::new(0),
            total: AtomicU64::new(0),
        }
    }

    fn record_drop(&self) {
        self.pending.fetch_add(1, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Acquire)
    }
}

// `perf_event_sample_format` bits (see `man perf_event_open`). Only the scalar
// fields below are supported; every other bit (RAW, BRANCH_STACK, REGS_INTR,
// STACK_USER, WEIGHT, DATA_SRC, TRANSACTION,
// PHYS_ADDR, …) is rejected at open time.
/// `PERF_SAMPLE_IP`: instruction pointer. Always set by real `perf` for samples.
const PERF_SAMPLE_IP: u64 = 1 << 0;
/// `PERF_SAMPLE_TID`: thread + process id (`u32 pid, u32 tid`).
pub(crate) const PERF_SAMPLE_TID: u64 = 1 << 1;
/// `PERF_SAMPLE_TIME`: monotonic timestamp (`u64`).
const PERF_SAMPLE_TIME: u64 = 1 << 2;
/// `PERF_SAMPLE_ADDR`: data address (`u64`); always 0 for our IP samples.
const PERF_SAMPLE_ADDR: u64 = 1 << 3;
/// `PERF_SAMPLE_READ`: one single or group `read_format` snapshot.
pub(crate) const PERF_SAMPLE_READ: u64 = 1 << 4;
/// `PERF_SAMPLE_CALLCHAIN`: `u64 nr` followed by context markers and IPs.
const PERF_SAMPLE_CALLCHAIN: u64 = 1 << 5;
/// `PERF_SAMPLE_ID`: event id (`u64`).
const PERF_SAMPLE_ID: u64 = 1 << 6;
/// `PERF_SAMPLE_CPU`: cpu number (`u32 cpu, u32 res`).
const PERF_SAMPLE_CPU: u64 = 1 << 7;
/// `PERF_SAMPLE_PERIOD`: sampling period (`u64`).
const PERF_SAMPLE_PERIOD: u64 = 1 << 8;
/// `PERF_SAMPLE_STREAM_ID`: stream id (`u64`).
const PERF_SAMPLE_STREAM_ID: u64 = 1 << 9;
/// `PERF_SAMPLE_REGS_USER`: ABI discriminator and optional saved AArch64 LR.
const PERF_SAMPLE_REGS_USER: u64 = 1 << 12;
/// `PERF_SAMPLE_IDENTIFIER`: leading event id (`u64`), emitted first.
const PERF_SAMPLE_IDENTIFIER: u64 = 1 << 16;

/// Every `sample_type` bit the sampling backend can emit a well-formed
/// `PERF_RECORD_SAMPLE` for. A sampling event whose `sample_type` sets any bit
/// outside this mask is rejected at open ([`super::hw`] reuses this constant);
/// real `perf record` sets `IP|TID|TIME|PERIOD`, all within the mask.
pub const SUPPORTED_SAMPLE_TYPE: u64 = PERF_SAMPLE_IP
    | PERF_SAMPLE_TID
    | PERF_SAMPLE_TIME
    | PERF_SAMPLE_ADDR
    | PERF_SAMPLE_READ
    | PERF_SAMPLE_CALLCHAIN
    | PERF_SAMPLE_ID
    | PERF_SAMPLE_CPU
    | PERF_SAMPLE_PERIOD
    | PERF_SAMPLE_STREAM_ID
    | PERF_SAMPLE_REGS_USER
    | PERF_SAMPLE_IDENTIFIER;

#[derive(Clone, Copy, Default)]
pub struct SampleReadValue {
    pub value: u64,
    pub time_enabled: u64,
    pub time_running: u64,
    pub lost: u64,
}

type SampleReadCallback = unsafe fn(*const (), usize, u64) -> SampleReadValue;

#[derive(Clone)]
pub struct SampleReadEntry {
    context: *const (),
    callback: Option<SampleReadCallback>,
    pub id: u64,
    _owner: Option<Arc<dyn core::any::Any + Send + Sync>>,
}

// SAFETY: entries are invoked only while the generation-owned SampleSlot is
// registered; its event/task owner keeps the callback context alive and the
// callback itself is restricted to IRQ-safe synchronized state.
unsafe impl Send for SampleReadEntry {}
unsafe impl Sync for SampleReadEntry {}

impl SampleReadEntry {
    pub const EMPTY: Self = Self {
        context: core::ptr::null(),
        callback: None,
        id: 0,
        _owner: None,
    };

    pub fn new(context: *const (), callback: SampleReadCallback, id: u64) -> Self {
        Self {
            context,
            callback: Some(callback),
            id,
            _owner: None,
        }
    }

    /// Retains the callback's task object for this entire registry generation.
    pub(crate) fn owned<T: core::any::Any + Send + Sync>(
        owner: Arc<T>,
        callback: SampleReadCallback,
        id: u64,
    ) -> Self {
        Self {
            context: Arc::as_ptr(&owner).cast(),
            callback: Some(callback),
            id,
            _owner: Some(owner),
        }
    }

    fn read(&self, slot: usize, now: u64) -> SampleReadValue {
        self.callback
            .map_or_else(SampleReadValue::default, |callback| {
                // SAFETY: the slot owner retains the callback context until
                // generation-checked unregister completes.
                unsafe { callback(self.context, slot, now) }
            })
    }
}

/// Owned ring and wake target used by one registered sampling generation.
#[derive(Clone)]
pub struct SampleOutput {
    ring: Option<PerfRingOutput>,
    notify: Option<Arc<IrqNotify>>,
    loss: Arc<LossState>,
}

impl core::fmt::Debug for SampleOutput {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SampleOutput")
            .field(
                "ring",
                &self
                    .ring
                    .as_ref()
                    .map(|ring| (ring.ring_vaddr(), ring.ring_len())),
            )
            .field("notifies", &self.notify.is_some())
            .finish()
    }
}

impl SampleOutput {
    /// Creates an output whose ring geometry and lifetime are one value.
    pub fn new(
        ring: Option<PerfRingOutput>,
        notify: Option<Arc<IrqNotify>>,
        loss: Arc<LossState>,
    ) -> Self {
        Self { ring, notify, loss }
    }
}

/// Everything the overflow handler needs for one counter.
///
/// Stored by value in the owner CPU's [`REGISTRY`]. Strong references in
/// [`SampleOutput`] remain live until generation-checked unregister completes
/// with local IRQs excluded.
pub struct SampleSlot {
    pub(crate) count: Arc<SamplingCount>,
    output: SampleOutput,
    /// Sampling period: the counter is re-armed to overflow after this many
    /// events via [`ax_cpu::pmu::counter::preload`]. Also emitted as the
    /// `PERF_SAMPLE_PERIOD` field of each record.
    pub period: u32,
    /// `attr.sample_type`: the set of scalar fields each record carries (see
    /// [`build_sample`]). Validated against [`SUPPORTED_SAMPLE_TYPE`] at open.
    pub sample_type: u64,
    /// Whether the event requested the saved AArch64 user link register.
    pub sample_user_lr: bool,
    /// Event id emitted for the `PERF_SAMPLE_ID` / `PERF_SAMPLE_IDENTIFIER`
    /// fields. `0` when the event was opened without per-event ids (the common
    /// case in this single-group implementation).
    pub id: u64,
    pub read_format: u64,
    pub read_entries: [SampleReadEntry; MAX_SAMPLE_READ_EVENTS],
    pub read_len: u8,
    /// PID namespace view captured by the event owner.
    pub observer: PidNamespaceId,
    /// Stable owner identity for task events; system-wide events use `None`.
    pub owner_ids: Option<(TgidNumber, TidNumber)>,
    /// Frequency mode (`attr.freq`): after each sample re-derive [`period`](Self::period)
    /// to converge on [`target_freq`](Self::target_freq) samples/sec. Fixed
    /// `-c` period when false.
    pub freq: bool,
    /// Target sample rate in Hz for frequency mode; `0` in fixed-period mode.
    pub target_freq: u32,
    /// Monotonic ns of the previous sample, for the frequency-mode delta. `0`
    /// before the first sample, when the period is left at its initial estimate.
    /// Mutated in place by the handler as the period adapts.
    pub last_time: u64,
}

/// Immutable attributes copied into one owner-CPU sampling slot.
pub struct SampleSlotConfig {
    pub(crate) count: Arc<SamplingCount>,
    pub period: u32,
    pub sample_type: u64,
    /// Whether PERF_SAMPLE_REGS_USER selects the AArch64 LR bit.
    pub sample_user_lr: bool,
    pub id: u64,
    pub read_format: u64,
    pub read_entries: [SampleReadEntry; MAX_SAMPLE_READ_EVENTS],
    pub read_len: u8,
    pub observer: PidNamespaceId,
    pub owner_ids: Option<(TgidNumber, TidNumber)>,
    pub freq: bool,
    pub target_freq: u32,
    pub last_time: u64,
}

impl SampleSlot {
    /// Creates one owned per-CPU registry entry.
    pub fn new(output: SampleOutput, config: SampleSlotConfig) -> Self {
        Self {
            count: config.count,
            output,
            period: config.period,
            sample_type: config.sample_type,
            sample_user_lr: config.sample_user_lr,
            id: config.id,
            read_format: config.read_format,
            read_entries: config.read_entries,
            read_len: config.read_len,
            observer: config.observer,
            owner_ids: config.owner_ids,
            freq: config.freq,
            target_freq: config.target_freq,
            last_time: config.last_time,
        }
    }
}

/// Per-CPU map from programmable counter index to its registered sampling slot.
///
/// Index `n` (`0..=30`) holds the slot for `PMEVCNTRn_EL0`. `None` means no
/// sampling event currently owns that counter on this CPU.
#[ax_percpu::def_percpu]
static REGISTRY: SamplingRegistry<SampleSlot> = SamplingRegistry::new();

/// Per-CPU wrap state for non-sampling programmable counters.
#[ax_percpu::def_percpu]
static COUNTING_REGISTRY: SamplingRegistry<Arc<IrqMutex<CounterExtender>>> =
    SamplingRegistry::new();

/// Globally unique registry generation. Counter slots may be reused, but an old
/// teardown token can never match the next event that occupies the same index.
static NEXT_REGISTRATION_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Whether [`pmu_overflow_handler`] has been registered with the IRQ framework.
///
/// Registration is process-global and idempotent: the handler walks the per-CPU
/// registry, so a single action installed on all CPUs suffices.
static REGISTERED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Mutates the current CPU's sampling registry without exposing a reference
/// beyond the CPU-local exclusive-access scope.
///
/// # Safety
///
/// The caller must prevent migration, local IRQ re-entry, and remote mutation
/// for the complete callback. Process-context callers use
/// [`NoPreemptIrqSave`]; the overflow handler already runs with local IRQs
/// masked on the CPU that owns the registry.
unsafe fn with_registry_mut<R>(
    operation: impl for<'value> FnOnce(&'value mut SamplingRegistry<SampleSlot>) -> R,
) -> R {
    // SAFETY: the caller establishes the migration and exclusion contract.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            ax_percpu::with_exclusive_cpu(pin, |exclusive| {
                REGISTRY.with_current_mut(exclusive, operation)
            })
        })
    }
    .unwrap_or_else(|error| panic!("perf sampling CPU-local state is invalid: {error}"))
}

unsafe fn with_counting_registry_mut<R>(
    operation: impl for<'value> FnOnce(
        &'value mut SamplingRegistry<Arc<IrqMutex<CounterExtender>>>,
    ) -> R,
) -> R {
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            ax_percpu::with_exclusive_cpu(pin, |exclusive| {
                COUNTING_REGISTRY.with_current_mut(exclusive, operation)
            })
        })
    }
    .unwrap_or_else(|error| panic!("perf counting CPU-local state is invalid: {error}"))
}

/// Registers `slot` for programmable counter `n` on the current CPU.
///
/// Runs on the event's owner CPU. The mutation is performed under
/// [`NoPreemptIrqSave`] so the overflow handler — which reads the same per-CPU
/// array — can never observe a half-written entry.
pub fn register(n: usize, slot: SampleSlot) -> Result<SampleRegistration, RegisterError> {
    if n > MAX_COUNTER {
        return Err(RegisterError::InvalidCounter);
    }
    let owner = PerfCpuId::new(ax_hal::percpu::this_cpu_id());
    let generation = NEXT_REGISTRATION_GENERATION
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |generation| {
            generation.checked_add(1)
        })
        .expect("PMU sampling registration generation exhausted");
    let _guard = NoPreemptIrqSave::new();
    // SAFETY: the guard prevents migration and local IRQ reentry.
    unsafe { with_registry_mut(|registry| registry.register(n, generation, slot)) }?;
    Ok(SampleRegistration::new(owner, n, generation))
}

/// Registers one counting event's wrap state on the current owner CPU.
pub(super) fn register_counting(
    n: usize,
    state: Arc<IrqMutex<CounterExtender>>,
) -> Result<SampleRegistration, RegisterError> {
    if n > MAX_COUNTER {
        return Err(RegisterError::InvalidCounter);
    }
    let owner = PerfCpuId::new(ax_hal::percpu::this_cpu_id());
    let generation = NEXT_REGISTRATION_GENERATION
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |generation| {
            generation.checked_add(1)
        })
        .expect("PMU counting registration generation exhausted");
    let _guard = NoPreemptIrqSave::new();
    unsafe { with_counting_registry_mut(|registry| registry.register(n, generation, state)) }?;
    Ok(SampleRegistration::new(owner, n, generation))
}

/// Clears one exact counting generation on its owner CPU.
pub(super) fn unregister_counting(
    registration: SampleRegistration,
) -> Result<(), SamplingUnregisterError> {
    if registration.owner().as_usize() != ax_hal::percpu::this_cpu_id() {
        return Err(SamplingUnregisterError::WrongCpu);
    }
    let removed = {
        let _guard = NoPreemptIrqSave::new();
        unsafe {
            with_counting_registry_mut(|registry| {
                registry.unregister(registration.counter(), registration.generation())
            })
        }
        .map_err(SamplingUnregisterError::Registry)?
    };
    drop(removed);
    Ok(())
}

/// Failure to remove an owner-CPU sampling registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SamplingUnregisterError {
    /// Teardown ran on a CPU other than the registry owner.
    WrongCpu,
    /// The counter slot no longer carries this generation.
    Registry(UnregisterError),
}

/// Clears one exact sampling generation on its owner CPU.
///
/// Returning successfully proves both that the registry no longer reaches the
/// output and that any local hard-IRQ reader has completed.
pub fn unregister(registration: SampleRegistration) -> Result<(), SamplingUnregisterError> {
    if registration.owner().as_usize() != ax_hal::percpu::this_cpu_id() {
        return Err(SamplingUnregisterError::WrongCpu);
    }
    let removed = {
        let _guard = NoPreemptIrqSave::new();
        // SAFETY: the guard prevents migration and local IRQ reentry.
        unsafe {
            with_registry_mut(|registry| {
                let removed =
                    registry.unregister(registration.counter(), registration.generation())?;
                // Every unregister caller has stopped this generation's
                // hardware. Preserve the final partial period before release.
                removed.count.update(registration.counter());
                Ok(removed)
            })
        }
        .map_err(SamplingUnregisterError::Registry)?
    };
    drop(removed);
    Ok(())
}

/// Rebinds the output of one exact live sampling generation.
///
/// Linux publishes a ring after an already-enabled event is mmap'd. Keep the
/// registered PMU slot and its period/read state intact while replacing only
/// the owned output snapshot that the IRQ handler reads.
pub fn replace_output(
    registration: SampleRegistration,
    output: SampleOutput,
) -> Result<(), SamplingUnregisterError> {
    if registration.owner().as_usize() != ax_hal::percpu::this_cpu_id() {
        return Err(SamplingUnregisterError::WrongCpu);
    }
    let old = {
        let _guard = NoPreemptIrqSave::new();
        // SAFETY: the guard prevents migration and local IRQ reentry.
        unsafe {
            with_registry_mut(|registry| {
                let slot = registry
                    .get_mut(registration.counter())
                    .ok_or(UnregisterError::Stale)?;
                let config = SampleSlotConfig {
                    count: Arc::clone(&slot.count),
                    period: slot.period,
                    sample_type: slot.sample_type,
                    sample_user_lr: slot.sample_user_lr,
                    id: slot.id,
                    read_format: slot.read_format,
                    read_entries: slot.read_entries.clone(),
                    read_len: slot.read_len,
                    observer: slot.observer,
                    owner_ids: slot.owner_ids,
                    freq: slot.freq,
                    target_freq: slot.target_freq,
                    last_time: slot.last_time,
                };
                registry.replace(
                    registration.counter(),
                    registration.generation(),
                    SampleSlot::new(output, config),
                )
            })
        }
        .map_err(SamplingUnregisterError::Registry)?
    };
    drop(old);
    Ok(())
}

/// Ensures [`pmu_overflow_handler`] is registered with the IRQ framework.
///
/// This process-context operation may allocate inside IRQ registration and must
/// run before scheduler hooks can arm a sampling event.
pub fn ensure_pmu_irq_registered() -> Result<(), ax_hal::irq::IrqError> {
    let pmu_irq = pmu_irq()?;
    if REGISTERED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let cpus = ax_hal::irq::CpuMask::first_n(ax_hal::cpu_num());
        // Mirror the timer's unit-data pattern: the handler does not use `data`.
        if let Err(err) = ax_hal::irq::request_percpu_irq(pmu_irq, cpus, pmu_overflow_handler) {
            // Roll back so a later open can retry registration.
            REGISTERED.store(false, Ordering::Release);
            return Err(err);
        }
    }
    Ok(())
}

/// Enables the already-registered PMU PPI on the current owner CPU.
///
/// This is bounded and allocation-free, so a scheduler hook may call it before
/// publishing the local sampling slot.
pub fn enable_local_pmu_irq() -> Result<(), ax_hal::irq::IrqError> {
    ax_hal::irq::set_enable(pmu_irq()?, true)
}

fn service_overflowed_slots(
    registry: &mut SamplingRegistry<SampleSlot>,
    overflow: u32,
    misc: u16,
    interrupted: Option<ax_cpu::pmu::InterruptedContext>,
    ip: usize,
    is_user: bool,
) -> u32 {
    let current = try_current_user_irq_view();

    // Bits serviced from the overflow snapshot captured and cleared before
    // this function runs. A counter that overflows again while being serviced
    // sets a fresh status bit that remains pending after this IRQ returns.
    let mut handled = 0;

    for n in 0..=MAX_COUNTER {
        if overflow & (1 << n) == 0 {
            continue;
        }
        handled |= 1 << n;

        let Some(slot) = registry.get_mut(n) else {
            continue;
        };
        let sample_type = slot.sample_type;
        let id = slot.id;
        let cur_period = slot.period;

        // Freeze before accounting, then reload only after every reader has
        // observed the terminal raw value. This includes IRQ delivery latency.
        ax_cpu::pmu::counter::disable(n);
        slot.count.update(n);
        if !slot.count.period_complete() {
            // This IRQ completed only a hardware chunk of the logical period.
            // Do not emit a premature sample or update frequency timestamps.
            slot.count.rearm(n, cur_period);
            ax_cpu::pmu::counter::enable(n);
            continue;
        }

        let time = ax_runtime::hal::time::monotonic_time_nanos();
        let cpu = ax_hal::percpu::this_cpu_id() as u32;
        let read_len = usize::from(slot.read_len).min(MAX_SAMPLE_READ_EVENTS);
        let mut read_values = [SampleReadValue::default(); MAX_SAMPLE_READ_EVENTS];
        for (entry, value) in slot.read_entries[..read_len].iter().zip(&mut read_values) {
            if sample_type & PERF_SAMPLE_READ != 0 {
                *value = entry.read(n, time);
            }
        }
        let (pid, tid) = slot.owner_ids.map_or_else(
            || {
                current.as_ref().map_or((None, None), |task| {
                    (
                        task.visible_tgid(slot.observer),
                        task.visible_tid(slot.observer),
                    )
                })
            },
            |(pid, tid)| (Some(pid), Some(tid)),
        );
        let mut callchain = [0u64; MAX_CALLCHAIN_ENTRIES];
        let callchain_len = if sample_type & PERF_SAMPLE_CALLCHAIN != 0 {
            build_callchain(interrupted, ip, is_user, &mut callchain)
        } else {
            0
        };
        let mut record = [0u8; SAMPLE_RECORD_MAX_LEN];
        let data = SampleData {
            ip: ip as u64,
            pid,
            tid,
            time,
            addr: 0,
            id,
            stream_id: 0,
            cpu,
            period: cur_period as u64,
            read_format: slot.read_format,
            read_entries: &slot.read_entries[..read_len],
            read_values: &read_values[..read_len],
            callchain: &callchain[..callchain_len],
            user_lr: interrupted
                .filter(|context| {
                    slot.sample_user_lr
                        && context.privilege == ax_cpu::pmu::InterruptedPrivilege::User
                })
                .map(|context| context.lr as u64),
        };
        let len = build_sample(&mut record, sample_type, misc, &data);

        if let Some(ring) = &slot.output.ring {
            write_sample(ring, &slot.output.loss, id, &record[..len]);
        }

        let next_period = if slot.freq {
            let next = if slot.last_time != 0 {
                next_freq_period(
                    cur_period,
                    slot.target_freq,
                    time.saturating_sub(slot.last_time),
                )
            } else {
                cur_period
            };
            slot.period = next;
            slot.last_time = time;
            next
        } else {
            cur_period
        };

        slot.count.rearm(n, next_period);
        ax_cpu::pmu::counter::enable(n);

        if let Some(notify) = &slot.output.notify {
            notify.notify_irq();
        }
    }

    handled
}

/// PMU overflow IRQ handler (hard-IRQ context).
///
/// Reads the interrupted PC and EL *first*, then services every overflowed
/// programmable counter that has a registered sampling slot: builds a
/// `PERF_RECORD_SAMPLE`, writes it into the event's ring, re-arms the counter,
/// and wakes the event's deferred worker. Clears only the overflow bits it
/// actually serviced (write-1-to-clear) at the end.
///
/// Returns [`IrqReturn::Handled`] if any counter overflowed (whether or not a
/// slot was registered for it), else [`IrqReturn::Unhandled`].
///
/// # Safety
///
/// Must only be invoked by the IRQ framework in hard-IRQ context on the core the
/// overflow fired on. Performs no allocation and takes no sleeping locks.
pub fn pmu_overflow_handler(_ctx: IrqContext) -> IrqReturn {
    // Capture the interrupted context before doing anything that could fault or
    // overwrite ELR_EL1 / SPSR_EL1.
    let interrupted = ax_cpu::pmu::interrupted_context();
    let ip = interrupted.map_or_else(
        || ax_cpu::pmu::interrupted_pc() as usize,
        |context| context.pc,
    );
    let is_user = interrupted.map_or_else(ax_cpu::pmu::interrupted_is_user, |context| {
        context.privilege == ax_cpu::pmu::InterruptedPrivilege::User
    });

    let ovf = ax_cpu::pmu::overflow::status();
    if ovf == 0 {
        return IrqReturn::Unhandled;
    }

    // Match Linux arm_pmuv3: acknowledge the complete overflow snapshot before
    // reprogramming any counter. QEMU's PMU model also requires this order to
    // schedule subsequent overflows from a newly preloaded value.
    ax_cpu::pmu::overflow::clear(ovf);

    let misc = if is_user {
        PERF_RECORD_MISC_USER
    } else {
        PERF_RECORD_MISC_KERNEL
    };

    // Publish counting wraps before any sampling slot snapshots a grouped
    // counting member. The overflow snapshot was already acknowledged, so a
    // later raw read cannot rediscover these wraps.
    unsafe {
        with_counting_registry_mut(|registry| {
            for n in 0..=MAX_COUNTER {
                if ovf & (1 << n) != 0
                    && let Some(state) = registry.get_mut(n)
                {
                    state.lock().record_overflow();
                }
            }
        })
    };

    // SAFETY: the handler runs with local IRQs masked on its current CPU, so
    // the registry cannot be re-entered or observed after migration.
    let _handled = unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            // Linux armv8pmu_handle_irq pauses the whole PMU while reading a
            // group, so siblings cannot advance while another slot is reloaded.
            ax_cpu::pmu::with_counters_paused(pin, || {
                with_registry_mut(|registry| {
                    service_overflowed_slots(registry, ovf, misc, interrupted, ip, is_user)
                })
            })
        })
    }
    .expect("PMU IRQ must have a bound CPU-local area");

    debug_assert_eq!(_handled & !ovf, 0);
    IrqReturn::Handled
}

fn build_callchain(
    interrupted: Option<ax_cpu::pmu::InterruptedContext>,
    ip: usize,
    is_user: bool,
    chain: &mut [u64],
) -> usize {
    let Some((marker, frames)) = chain.split_first_mut() else {
        return 0;
    };
    *marker = if is_user {
        (-512i64) as u64
    } else {
        (-128i64) as u64
    };
    let count = match interrupted {
        Some(context) if is_user => {
            super::unwind::user_callchain(ip, context.fp, context.sp, frames)
        }
        Some(context) => super::unwind::kernel_callchain(ip, context.fp, frames),
        None => {
            let Some(leaf) = frames.first_mut() else {
                return 1;
            };
            *leaf = ip as u64;
            1
        }
    };
    1 + count
}

#[cfg(all(test, axtest))]
fn kernel_task_sample_ids_are_empty_for_test() -> bool {
    try_current_user_irq_view().is_none()
}

/// Lays out one `PERF_RECORD_SAMPLE` into `buf` per `sample_type`, returning its
/// total length in bytes.
///
/// The fields are written in the canonical order mandated by `man
/// perf_event_open` (`PERF_RECORD_SAMPLE`), each gated on its `sample_type` bit:
///
/// 1. header — `u32 type = PERF_RECORD_SAMPLE`, `u16 misc`, `u16 size`
///    (back-patched once the body length is known)
/// 2. `IDENTIFIER` → `u64 id`
/// 3. `IP` → `u64 ip`
/// 4. `TID` → `u32 pid`, `u32 tid`
/// 5. `TIME` → `u64 time`
/// 6. `ADDR` → `u64 addr`
/// 7. `ID` → `u64 id`
/// 8. `STREAM_ID` → `u64 stream_id`
/// 9. `CPU` → `u32 cpu`, `u32 res = 0`
/// 10. `PERIOD` → `u64 period`
///
/// `buf` must be at least [`SAMPLE_RECORD_MAX_LEN`] bytes. With
/// `sample_type == PERF_SAMPLE_IP` exactly, the result is the original 16-byte
/// IP-only record (8-byte header + `u64 ip`).
/// The per-sample scalar values [`build_sample`] may emit (those not implied by
/// `sample_type` alone). Gathered by the overflow handler at interrupt time.
struct SampleData<'a> {
    ip: u64,
    pid: Option<TgidNumber>,
    tid: Option<TidNumber>,
    time: u64,
    addr: u64,
    id: u64,
    stream_id: u64,
    cpu: u32,
    period: u64,
    read_format: u64,
    read_entries: &'a [SampleReadEntry],
    read_values: &'a [SampleReadValue],
    callchain: &'a [u64],
    user_lr: Option<u64>,
}

fn build_sample(buf: &mut [u8], sample_type: u64, misc: u16, d: &SampleData) -> usize {
    // Cursor into `buf`. All offsets stay within `SAMPLE_RECORD_MAX_LEN` because
    // at most the header + 9 u64-sized fields are written and the caller passes a
    // buffer of that size. `put!` appends a native-endian scalar and advances the
    // cursor (a macro, not a closure, so it never holds a borrow of `off`).
    let mut off = 0usize;
    macro_rules! put {
        ($v:expr) => {{
            let bytes = $v.to_ne_bytes();
            buf[off..off + bytes.len()].copy_from_slice(&bytes);
            off += bytes.len();
        }};
    }

    // Header: type, misc, and a placeholder size (back-patched below).
    put!(PERF_RECORD_SAMPLE); // u32
    put!(misc); // u16
    let size_off = off;
    put!(0u16); // size placeholder

    // Body, in canonical PERF_RECORD_SAMPLE order, each field gated by its bit.
    if sample_type & PERF_SAMPLE_IDENTIFIER != 0 {
        put!(d.id);
    }
    if sample_type & PERF_SAMPLE_IP != 0 {
        put!(d.ip);
    }
    if sample_type & PERF_SAMPLE_TID != 0 {
        // pid and tid are a packed `u32` pair in one 8-byte slot.
        put!(d.pid.map_or(0, TgidNumber::get));
        put!(d.tid.map_or(0, TidNumber::get));
    }
    if sample_type & PERF_SAMPLE_TIME != 0 {
        put!(d.time);
    }
    if sample_type & PERF_SAMPLE_ADDR != 0 {
        put!(d.addr);
    }
    if sample_type & PERF_SAMPLE_ID != 0 {
        put!(d.id);
    }
    if sample_type & PERF_SAMPLE_STREAM_ID != 0 {
        put!(d.stream_id);
    }
    if sample_type & PERF_SAMPLE_CPU != 0 {
        // cpu and a reserved zero, again a packed `u32` pair.
        put!(d.cpu);
        put!(0u32);
    }
    if sample_type & PERF_SAMPLE_PERIOD != 0 {
        put!(d.period);
    }
    if sample_type & PERF_SAMPLE_READ != 0 {
        if d.read_format & super::PERF_FORMAT_GROUP != 0 {
            put!(d.read_values.len() as u64);
            if d.read_format & super::PERF_FORMAT_TOTAL_TIME_ENABLED != 0 {
                put!(d.read_values.first().map_or(0, |value| value.time_enabled));
            }
            if d.read_format & super::PERF_FORMAT_TOTAL_TIME_RUNNING != 0 {
                put!(d.read_values.first().map_or(0, |value| value.time_running));
            }
            for (entry, value) in d.read_entries.iter().zip(d.read_values) {
                put!(value.value);
                if d.read_format & super::PERF_FORMAT_ID != 0 {
                    put!(entry.id);
                }
                if d.read_format & super::PERF_FORMAT_LOST != 0 {
                    put!(value.lost);
                }
            }
        } else {
            let value = d.read_values.first().copied().unwrap_or_default();
            put!(value.value);
            if d.read_format & super::PERF_FORMAT_TOTAL_TIME_ENABLED != 0 {
                put!(value.time_enabled);
            }
            if d.read_format & super::PERF_FORMAT_TOTAL_TIME_RUNNING != 0 {
                put!(value.time_running);
            }
            if d.read_format & super::PERF_FORMAT_ID != 0 {
                put!(d.read_entries.first().map_or(0, |entry| entry.id));
            }
            if d.read_format & super::PERF_FORMAT_LOST != 0 {
                put!(value.lost);
            }
        }
    }
    if sample_type & PERF_SAMPLE_CALLCHAIN != 0 {
        put!(d.callchain.len() as u64);
        for &entry in d.callchain {
            put!(entry);
        }
    }
    if sample_type & PERF_SAMPLE_REGS_USER != 0 {
        if let Some(lr) = d.user_lr {
            put!(2u64); // PERF_SAMPLE_REGS_ABI_64
            put!(lr);
        } else {
            put!(0u64); // PERF_SAMPLE_REGS_ABI_NONE
        }
    }

    // Back-patch the header's `size` field now that the total length is known.
    buf[size_off..size_off + 2].copy_from_slice(&(off as u16).to_ne_bytes());
    off
}

/// Writes one record into a perf ring buffer, IRQ-safe and self-contained.
///
/// Page 0 of the range described by `ring` is a
/// [`perf_event_mmap_page`]; the data region starts at `ring_vaddr + data_offset`
/// (`data_offset == PAGE_SIZE` for our buffers) and is `data_size` bytes. The
/// record is copied at `data_head % data_size` (split into two copies on wrap),
/// then `data_head` is published with a release fence so a userspace reader that
/// observes the new `data_head` also observes the bytes.
///
/// If the record would overwrite still-unread bytes
/// (`data_head - data_tail + len > data_size`) it is dropped: `data_head` is not
/// advanced. Lost-record accounting is intentionally omitted for M2.
///
/// # Safety
///
/// `ring` must describe a kernel-mapped ring whose header was initialized by
/// `HwPerfEvent::device_mmap`. Its owned lifetime anchor must keep that mapping
/// valid for this call.
unsafe fn ring_write_locked(ring: &PerfRingOutput, record: &[u8]) -> bool {
    let ring_vaddr = ring.ring_vaddr();
    let ring_len = ring.ring_len();
    // Guard the enable-before-mmap case (slot registered with a zero ring) and
    // any ring too small to even hold the header page: there is nowhere to
    // write, and the header pointer would be null/out of bounds.
    if ring_vaddr == 0 || ring_len < core::mem::size_of::<perf_event_mmap_page>() {
        return false;
    }

    let header = ring_vaddr as *mut perf_event_mmap_page;

    // SAFETY: `header` points at the initialized header page.
    let data_offset =
        unsafe { core::ptr::addr_of!((*header).data_offset).read_volatile() } as usize;
    let data_size = unsafe { core::ptr::addr_of!((*header).data_size).read_volatile() } as usize;

    // Defensive: a malformed/zero header (no data region, or a data window that
    // does not fit in the buffer) means there is nowhere safe to write.
    if data_size == 0 || data_offset > ring_len || data_offset + data_size > ring_len {
        return false;
    }

    let len = record.len();
    if len > data_size {
        return false;
    }

    // SAFETY: header page is initialized; these are plain u64 fields.
    let head = unsafe { core::ptr::addr_of!((*header).data_head).read_volatile() };
    let tail = unsafe { core::ptr::addr_of!((*header).data_tail).read_volatile() };

    // Would this record overwrite bytes the reader has not consumed yet? Drop it
    // if so (back-pressure; no lost-record accounting in M2).
    if head.wrapping_sub(tail).wrapping_add(len as u64) > data_size as u64 {
        return false;
    }

    let data_base = ring_vaddr + data_offset;
    let start = (head % data_size as u64) as usize;
    let first = core::cmp::min(len, data_size - start);

    // SAFETY: `data_base + start + first <= data_base + data_size`, within the
    // mapped data region; same for the wrapped remainder below.
    unsafe {
        core::ptr::copy_nonoverlapping(record.as_ptr(), (data_base + start) as *mut u8, first);
        if first < len {
            core::ptr::copy_nonoverlapping(
                record.as_ptr().add(first),
                data_base as *mut u8,
                len - first,
            );
        }
    }

    // Publish the bytes before the new head: a reader observing the updated
    // `data_head` must also observe the record contents.
    core::sync::atomic::fence(Ordering::Release);
    // SAFETY: header page is initialized.
    unsafe {
        core::ptr::addr_of_mut!((*header).data_head).write_volatile(head.wrapping_add(len as u64));
    }
    true
}

fn write_sample(ring: &PerfRingOutput, loss: &LossState, id: u64, sample: &[u8]) {
    let Some(_writer) = ring.try_begin_write() else {
        ring.record_contention_drop();
        loss.record_drop();
        return;
    };

    let pending = loss.pending.load(Ordering::Relaxed);
    if pending != 0 {
        let mut record = [0u8; LOST_RECORD_LEN];
        record[0..4].copy_from_slice(&PERF_RECORD_LOST.to_ne_bytes());
        record[4..6].copy_from_slice(&0u16.to_ne_bytes());
        record[6..8].copy_from_slice(&(LOST_RECORD_LEN as u16).to_ne_bytes());
        record[8..16].copy_from_slice(&id.to_ne_bytes());
        record[16..24].copy_from_slice(&pending.to_ne_bytes());
        // SAFETY: the producer lease is the ring's unique kernel writer.
        if !unsafe { ring_write_locked(ring, &record) } {
            loss.record_drop();
            return;
        }
        loss.pending.fetch_sub(pending, Ordering::Relaxed);
    }

    // SAFETY: the same producer lease covers the sample reservation.
    if unsafe { ring_write_locked(ring, sample) } {
    } else {
        loss.record_drop();
    }
}

/// Write one record into a sampling ring from **process context** (the side-band
/// path: `PERF_RECORD_MMAP2` / `COMM` / `FORK` / `EXIT` emitted at execve / mmap /
/// clone / exit), serialized against every producer sharing the output.
///
/// [`PerfRingOutput`] owns one shared, non-blocking producer gate. Both hard-IRQ
/// and process producers attempt one CAS and drop on contention, so redirected
/// or inherited events remain bounded even when writers run on different CPUs.
///
/// # Safety
///
/// Same contract as [`ring_write`]: `ring` must keep the initialized mapping
/// pinned for the duration of the call.
pub(crate) unsafe fn ring_write_process(ring: &PerfRingOutput, record: &[u8]) {
    let Some(_writer) = ring.try_begin_write() else {
        ring.record_contention_drop();
        return;
    };
    // SAFETY: the caller upholds the mapping contract and the producer lease
    // is the ring's unique kernel writer.
    let _ = unsafe { ring_write_locked(ring, record) };
}

#[cfg(all(test, axtest))]
mod tests {
    #[axtest::axtest]
    fn maximum_period_preload_leaves_irq_latency_headroom() {
        let count = super::SamplingCount::new();
        let _guard = crate::sync::NoPreemptIrqSave::new();
        super::super::percpu::ensure_current_cpu_initialized().unwrap();
        let slot = super::super::percpu::alloc_current_programmable().unwrap();
        ax_cpu::pmu::counter::disable(slot);
        count.preload(slot, u32::MAX);
        let raw = ax_cpu::pmu::counter::read(slot) as u32;
        ax_cpu::pmu::counter::write(slot, 10);
        count.update(slot);
        assert!(
            !count.period_complete(),
            "first hardware chunk is not a full logical sample"
        );
        count.rearm(slot, u32::MAX);
        ax_cpu::pmu::counter::write(slot, 9);
        count.update(slot);
        assert!(count.period_complete());
        assert_eq!(
            count.value(),
            u64::from(u32::MAX) + 9,
            "IRQ latency must survive the logical period boundary"
        );
        count.rearm(slot, u32::MAX);
        assert!(!count.period_complete());
        super::super::percpu::free_current_programmable(slot);
        assert_eq!(
            raw,
            0u32.wrapping_sub(u32::MAX >> 1),
            "hardware chunk must leave overflow latency headroom"
        );
    }

    #[axtest::axtest]
    fn sampling_delta_counts_partial_wrap_and_reload() {
        let mut state = super::SamplingCountState {
            previous: 0u32.wrapping_sub(100),
            total: 0,
            remaining: 100,
        };
        assert_eq!(state.update(0u32.wrapping_sub(60)), 40);
        assert_eq!(state.update(7), 107);
        assert_eq!(state.update(7), 107);
        // Reloading changes the baseline, never the already-accounted count.
        state.previous = 0u32.wrapping_sub(200);
        assert_eq!(state.update(0u32.wrapping_sub(170)), 137);
    }

    #[axtest::axtest]
    fn read_snapshot_retains_callback_until_registry_removal() {
        use super::*;

        unsafe fn read_value(context: *const (), _slot: usize, _now: u64) -> SampleReadValue {
            // SAFETY: the entry owns the AtomicU64 used as callback context.
            let value = unsafe { &*context.cast::<AtomicU64>() }.load(Ordering::Acquire);
            SampleReadValue {
                value,
                ..SampleReadValue::default()
            }
        }

        let owner = Arc::new(AtomicU64::new(17));
        let weak = Arc::downgrade(&owner);
        let entry = SampleReadEntry::owned(Arc::clone(&owner), read_value, 1);
        let mut registry = SamplingRegistry::new();
        assert!(registry.register(0, 1, entry).is_ok());
        drop(owner);
        assert!(
            weak.upgrade().is_some(),
            "closing the fd must retain IRQ callback state"
        );
        assert_eq!(registry.get_mut(0).unwrap().read(0, 0).value, 17);
        drop(registry.unregister(0, 1).unwrap());
        assert!(
            weak.upgrade().is_none(),
            "unregister must release its callback ownership"
        );
    }

    #[cfg(all(test, axtest, target_arch = "aarch64"))]
    #[axtest::axtest]
    fn kernel_task_sample_ids_are_empty() {
        assert!(super::kernel_task_sample_ids_are_empty_for_test());
    }

    #[axtest::axtest]
    fn maximum_sample_record_fits_irq_stack_buffer() {
        use super::*;

        let entries = [const { SampleReadEntry::EMPTY }; MAX_SAMPLE_READ_EVENTS];
        let values = [SampleReadValue::default(); MAX_SAMPLE_READ_EVENTS];
        let callchain = [0u64; MAX_CALLCHAIN_ENTRIES];
        let data = SampleData {
            ip: 1,
            pid: None,
            tid: None,
            time: 2,
            addr: 3,
            id: 4,
            stream_id: 5,
            cpu: 6,
            period: 7,
            read_format: super::super::PERF_FORMAT_GROUP
                | super::super::PERF_FORMAT_TOTAL_TIME_ENABLED
                | super::super::PERF_FORMAT_TOTAL_TIME_RUNNING
                | super::super::PERF_FORMAT_ID
                | super::super::PERF_FORMAT_LOST,
            read_entries: &entries,
            read_values: &values,
            callchain: &callchain,
            user_lr: Some(8),
        };
        let mut record = [0u8; SAMPLE_RECORD_MAX_LEN];
        let sample_type = PERF_SAMPLE_IDENTIFIER
            | PERF_SAMPLE_IP
            | PERF_SAMPLE_TID
            | PERF_SAMPLE_TIME
            | PERF_SAMPLE_ADDR
            | PERF_SAMPLE_ID
            | PERF_SAMPLE_STREAM_ID
            | PERF_SAMPLE_CPU
            | PERF_SAMPLE_PERIOD
            | PERF_SAMPLE_READ
            | PERF_SAMPLE_CALLCHAIN
            | PERF_SAMPLE_REGS_USER;

        assert_eq!(
            build_sample(&mut record, sample_type, PERF_RECORD_MISC_USER, &data),
            SAMPLE_RECORD_MAX_LEN
        );
    }
}
