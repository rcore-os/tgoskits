//! PMU overflow-IRQ sampling backend (`perf record`).
//!
//! This is the IRQ half of hardware-PMU sampling. A sampling perf event
//! ([`super::hw::HwPerfEvent`] with `sample_period > 0`) preloads a programmable
//! counter so it overflows after `period` events; the overflow raises the PMUv3
//! interrupt (PPI 7 / INTID 23). [`pmu_overflow_handler`] runs in hard-IRQ
//! context, reads the interrupted PC, builds one `PERF_RECORD_SAMPLE` per
//! overflowed counter, writes it into that event's mmap ring buffer, re-arms the
//! counter, and wakes a deferred worker (via [`ax_task::IrqNotify`]) that
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
//! allocating or taking a lock. [`REGISTRY`] is a fixed `[Option<SampleSlot>; 32]`
//! per CPU (index = programmable counter index). A [`SampleSlot`] is a small
//! `Copy` POD carrying exactly the raw values the handler needs. `register` /
//! `unregister` mutate the *current* CPU's array under a local-IRQ-off critical
//! section ([`NoPreemptIrqSave`]) so they never race the handler. M2 is
//! single-core, so the event's core is always cpu0.
//!
//! # `notify` raw pointer soundness
//!
//! `SampleSlot::notify` is a raw `*const IrqNotify`. It is valid for the whole
//! time the slot is registered because the owning event holds a strong
//! `Arc<IrqNotify>` for its entire life, and teardown
//! ([`super::hw::HwPerfEvent`]'s disable/Drop) calls [`unregister`] — clearing
//! the slot — *before* dropping that `Arc`. The handler therefore only ever
//! dereferences a pointer whose target is still alive.

use alloc::sync::Arc;
use core::{fmt, sync::atomic::{AtomicU64, Ordering}};

use ax_alloc::GlobalPage;
use ax_hal::irq::{IrqContext, IrqId, IrqReturn};
use ax_task::IrqNotify;
use kbpf_basic::linux_bpf::perf_event_mmap_page;

use crate::{
    sync::{IrqMutex, PreemptIrqSaveGuard},
    task::{AsThread, PidNamespaceId, TgidNumber, TidNumber},
};

fn pmu_irq() -> Result<IrqId, ax_hal::irq::IrqError> {
    ax_hal::pmu::irq()
}

/// Maximum programmable counter index (matches [`ax_cpu::pmu::counter`] /
/// [`ax_cpu::pmu::overflow`]); the registry is sized one past this for indexing.
const MAX_COUNTER: usize = 30;

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
/// `PERF_RECORD_LOST`: reports samples dropped since the previous successful
/// loss record for this source event.
const PERF_RECORD_LOST: u32 = 2;
/// `PERF_RECORD_MISC_KERNEL`: the sample landed in kernel (EL1) context.
const PERF_RECORD_MISC_KERNEL: u16 = 1;
/// `PERF_RECORD_MISC_USER`: the sample landed in user (EL0) context.
const PERF_RECORD_MISC_USER: u16 = 2;

/// Maximum number of entries in the prebuilt `PERF_SAMPLE_READ` snapshot.
/// ARM PMUv3 exposes at most 31 programmable slots and a hardware group cannot
/// run more events than that at once.
pub const MAX_SAMPLE_READ_EVENTS: usize = 31;
/// Upper bound on a sample: fixed scalar fields, a maximum-sized group READ,
/// and the bounded frame-pointer callchain. The handler uses this stack buffer
/// only while local IRQs are masked; no allocation is performed.
const MAX_STACK_DEPTH: usize = 64;
const MAX_CALLCHAIN_ENTRIES: usize = 1 + MAX_STACK_DEPTH;
const SAMPLE_READ_MAX_U64S: usize = 3 + MAX_SAMPLE_READ_EVENTS * 3;
const SAMPLE_RECORD_MAX_LEN: usize =
    8 + 9 * 8 + SAMPLE_READ_MAX_U64S * 8 + (1 + MAX_CALLCHAIN_ENTRIES) * 8;
const LOST_RECORD_LEN: usize = 8 + 2 * 8;

/// One physical perf mmap ring and its IRQ-safe multi-producer writer.
///
/// The endpoint owns the pages, so an enabled producer can never retain a raw
/// address after `munmap`. Redirected events share this same `Arc`, hence PMU
/// IRQs on different CPUs and process-context side-band writers serialize on
/// one lock before reserving `data_head`.
pub struct RingEndpoint {
    _pages: Arc<GlobalPage>,
    ring_vaddr: usize,
    ring_len: usize,
    writer: IrqMutex<()>,
    notify: Arc<IrqNotify>,
}

impl fmt::Debug for RingEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RingEndpoint")
            .field("ring_vaddr", &self.ring_vaddr)
            .field("ring_len", &self.ring_len)
            .finish_non_exhaustive()
    }
}

impl RingEndpoint {
    pub fn new(
        pages: Arc<GlobalPage>,
        ring_vaddr: usize,
        ring_len: usize,
        notify: Arc<IrqNotify>,
    ) -> Arc<Self> {
        Arc::new(Self {
            _pages: pages,
            ring_vaddr,
            ring_len,
            writer: IrqMutex::new(()),
            notify,
        })
    }

    pub fn ring_vaddr(&self) -> usize {
        self.ring_vaddr
    }

    pub fn ring_len(&self) -> usize {
        self.ring_len
    }

    pub fn notify(&self) -> Arc<IrqNotify> {
        self.notify.clone()
    }

    pub fn has_data(&self) -> bool {
        let header = self.ring_vaddr as *const perf_event_mmap_page;
        // SAFETY: `self.pages` pins the initialized header for this endpoint.
        let (head, tail) = unsafe {
            (
                core::ptr::addr_of!((*header).data_head).read_volatile(),
                core::ptr::addr_of!((*header).data_tail).read_volatile(),
            )
        };
        head != tail
    }

    /// Writes an ordinary record from process context or an IRQ source that
    /// does not need per-source loss accounting.
    pub fn write_record(&self, record: &[u8]) -> bool {
        let _writer = self.writer.lock();
        // SAFETY: the endpoint owns the ring pages and the writer lock is the
        // unique kernel reservation mechanism for its `data_head`.
        let written = unsafe { ring_write_locked(self.ring_vaddr, self.ring_len, record) };
        if written {
            self.notify.notify_irq();
        }
        written
    }

    /// Writes one sample, flushing this source's pending `PERF_RECORD_LOST`
    /// first. Both records share the same reservation critical section.
    fn write_sample(&self, loss: &LossState, id: u64, sample: &[u8]) {
        let _writer = self.writer.lock();
        let pending = loss.pending.load(Ordering::Relaxed);
        if pending != 0 {
            let mut record = [0u8; LOST_RECORD_LEN];
            record[0..4].copy_from_slice(&PERF_RECORD_LOST.to_ne_bytes());
            record[4..6].copy_from_slice(&0u16.to_ne_bytes());
            record[6..8].copy_from_slice(&(LOST_RECORD_LEN as u16).to_ne_bytes());
            record[8..16].copy_from_slice(&id.to_ne_bytes());
            record[16..24].copy_from_slice(&pending.to_ne_bytes());
            // SAFETY: protected by this endpoint's writer lock.
            if !unsafe { ring_write_locked(self.ring_vaddr, self.ring_len, &record) } {
                loss.record_drop();
                return;
            }
            loss.pending.store(0, Ordering::Relaxed);
        }

        // SAFETY: protected by this endpoint's writer lock.
        if unsafe { ring_write_locked(self.ring_vaddr, self.ring_len, sample) } {
            self.notify.notify_irq();
        } else {
            loss.record_drop();
        }
    }
}

/// Loss accounting belongs to the source event, not the shared output ring.
/// Redirecting several events therefore preserves each source's event id and
/// dropped-sample total while serializing their records through one endpoint.
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

// `perf_event_sample_format` bits from Linux v7.1 UAPI. Only the fields below
// are supported; every other bit (RAW,
// BRANCH_STACK, REGS_USER/INTR, STACK_USER, WEIGHT, DATA_SRC, TRANSACTION,
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
    | PERF_SAMPLE_IDENTIFIER;

/// One value returned by an IRQ-safe `PERF_SAMPLE_READ` callback.
#[derive(Clone, Copy, Default)]
pub struct SampleReadValue {
    pub value: u64,
    pub time_enabled: u64,
    pub time_running: u64,
    pub lost: u64,
}

/// Type-erased, prebuilt read source used by the PMU overflow handler.
///
/// The callback and its context are installed while the corresponding event is
/// strongly owned by the task or system-wide perf fd. Teardown unregisters the
/// `SampleSlot` synchronously before that owner can be released.
#[derive(Clone, Copy)]
pub struct SampleReadEntry {
    context: *const (),
    callback: Option<unsafe fn(*const (), usize, u64, u32, bool) -> SampleReadValue>,
    pub id: u64,
}

impl SampleReadEntry {
    pub const EMPTY: Self = Self {
        context: core::ptr::null(),
        callback: None,
        id: 0,
    };

    pub fn new(
        context: *const (),
        callback: unsafe fn(*const (), usize, u64, u32, bool) -> SampleReadValue,
        id: u64,
    ) -> Self {
        Self {
            context,
            callback: Some(callback),
            id,
        }
    }

    fn read(self, slot: usize, now: u64, period: u32, account_source: bool) -> SampleReadValue {
        let Some(callback) = self.callback else {
            return SampleReadValue::default();
        };
        // SAFETY: the creator guarantees `context` remains live until the
        // enclosing SampleSlot is unregistered; the callback is IRQ-safe.
        unsafe { callback(self.context, slot, now, period, account_source) }
    }
}

/// Everything the overflow handler needs for one counter, in a lock-free,
/// alloc-free `Copy` POD.
///
/// Stored in the per-CPU [`REGISTRY`] at the counter's index while the event is
/// enabled. See the module docs for the `notify`-pointer soundness argument.
#[derive(Clone, Copy)]
pub struct SampleSlot {
    /// Stable output endpoint, held alive by the registered event until this
    /// slot is unregistered. Null means the event was enabled before mmap.
    pub endpoint: *const RingEndpoint,
    /// Stable per-source loss accounting, with the same lifetime as the slot.
    pub loss: *const LossState,
    /// Sampling period: the counter is re-armed to overflow after this many
    /// events via [`ax_cpu::pmu::counter::preload`]. Also emitted as the
    /// `PERF_SAMPLE_PERIOD` field of each record.
    pub period: u32,
    /// `attr.sample_type`: the set of scalar fields each record carries (see
    /// [`build_sample`]). Validated against [`SUPPORTED_SAMPLE_TYPE`] at open.
    pub sample_type: u64,
    /// Event id emitted for the `PERF_SAMPLE_ID` / `PERF_SAMPLE_IDENTIFIER`
    /// fields. `0` when the event was opened without per-event ids (the common
    /// case in this single-group implementation).
    pub id: u64,
    /// `attr.read_format` controlling a requested `PERF_SAMPLE_READ` payload.
    pub read_format: u64,
    /// Leader-first, fixed-capacity read sources built before IRQ entry.
    pub read_entries: [SampleReadEntry; MAX_SAMPLE_READ_EVENTS],
    /// Number of valid entries in `read_entries` (at least one for READ).
    pub read_len: u8,
    /// PID namespace view captured by the event owner.
    pub observer: PidNamespaceId,
    /// Fixed owner identity for a task event; system-wide sources use `None`
    /// and attribute the task interrupted on their CPU.
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

// SAFETY: `SampleSlot` is a plain bag of integers plus a raw pointer. The
// pointer is only ever dereferenced from the overflow handler on the same CPU
// that registered the slot, and the registry is mutated only under a
// local-IRQ-off critical section, so there is no cross-thread aliasing of the
// pointee through this type. Marking it `Send` lets it live inside the per-CPU
// static; it is never actually moved across CPUs (single-core in M2, and the
// registry is per-CPU regardless).
unsafe impl Send for SampleSlot {}

/// Per-CPU map from programmable counter index to its registered sampling slot.
///
/// Index `n` (`0..=30`) holds the slot for `PMEVCNTRn_EL0`. `None` means no
/// sampling event currently owns that counter on this CPU.
#[ax_percpu::def_percpu]
static REGISTRY: [Option<SampleSlot>; 32] = [None; 32];

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
    operation: impl for<'value> FnOnce(&'value mut [Option<SampleSlot>; 32]) -> R,
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

/// Registers `slot` for programmable counter `n` on the current CPU.
///
/// Runs in process context on the event's core (cpu0 under smp1). The mutation
/// is performed under [`NoPreemptIrqSave`] so the overflow handler — which reads
/// the same per-CPU array — can never observe a half-written entry, and so the
/// current CPU's view of `REGISTRY` is the one being updated.
pub fn register(n: usize, slot: SampleSlot) {
    if n > MAX_COUNTER {
        return;
    }
    let _guard = PreemptIrqSaveGuard::new();
    // SAFETY: preemption and local IRQs are disabled by `_guard`, so we hold
    // exclusive access to this CPU's `REGISTRY` for the critical section.
    unsafe { with_registry_mut(|registry| registry[n] = Some(slot)) };
}

/// Clears the sampling slot for programmable counter `n` on the current CPU.
///
/// Mirror of [`register`]. Teardown calls this *before* the owning event drops
/// its `Arc<IrqNotify>`, so once this returns the handler can no longer reach a
/// stale `notify` pointer for counter `n`.
pub fn unregister(n: usize) {
    if n > MAX_COUNTER {
        return;
    }
    let _guard = PreemptIrqSaveGuard::new();
    // SAFETY: see `register`.
    unsafe { with_registry_mut(|registry| registry[n] = None) };
}

/// Ensures [`pmu_overflow_handler`] is registered with the IRQ framework and the
/// PMU overflow line is enabled on the current core.
///
/// Idempotent: the first caller installs the per-CPU action for the PMU IRQ
/// across all online CPUs. Every caller (re-)enables INTID 23 on the *current*
/// core. The explicit `set_enable` is required: the framework's per-core line
/// enable runs at `cpu_online`/boot, before this handler is ever registered, so
/// under smp1 the PMU PPI would otherwise stay masked and the overflow IRQ would
/// never fire on cpu0.
pub fn ensure_pmu_irq_registered() {
    let pmu_irq = match pmu_irq() {
        Ok(irq) => irq,
        Err(err) => {
            warn!("perf sampling: failed to resolve PMU overflow IRQ: {err:?}");
            return;
        }
    };

    if REGISTERED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let cpus = ax_hal::irq::CpuMask::first_n(ax_hal::cpu_num());
        // Mirror the timer's unit-data pattern: the handler does not use `data`.
        if let Err(err) = ax_hal::irq::request_percpu_irq(pmu_irq, cpus, pmu_overflow_handler) {
            // Roll back so a later open can retry registration.
            REGISTERED.store(false, Ordering::Release);
            warn!("perf sampling: failed to register PMU overflow IRQ: {err:?}");
            return;
        }
    }
    // Enable the PMU PPI on the core this sampling event runs on. Required even
    // when the action was registered by an earlier event: the per-core line is
    // not auto-enabled for runtime-registered PPIs.
    if let Err(err) = ax_hal::irq::set_enable(pmu_irq, true) {
        warn!("perf sampling: failed to enable PMU overflow IRQ {pmu_irq:?}: {err:?}");
    }
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

    let misc = if is_user {
        PERF_RECORD_MISC_USER
    } else {
        PERF_RECORD_MISC_KERNEL
    };

    // Bits we have serviced; cleared (write-1-to-clear) at the very end so a
    // counter is not re-armed and re-cleared in a way that drops a concurrent
    // overflow we have not looked at.
    let mut handled: u32 = 0;

    for n in 0..=MAX_COUNTER {
        if ovf & (1 << n) == 0 {
            continue;
        }
        handled |= 1 << n;

        // SAFETY: we run on the core that took the IRQ with local IRQs masked,
        // so this CPU's `REGISTRY` is not being mutated concurrently (register /
        // unregister disable local IRQs). The mutable borrow remains inside the
        // scoped callback while frequency mode updates `period`/`last_time`.
        let sample = |registry: &mut [Option<SampleSlot>; 32]| {
            let Some(slot) = registry[n].as_mut() else {
                // A counting-only counter may wrap without a sampling slot.
                // Clear it below but leave re-arming to its owner.
                return false;
            };

            // Snapshot the fields the record + re-arm need (copied out so the slot
            // can be mutated below without aliasing the borrow).
            let sample_type = slot.sample_type;
            let id = slot.id;
            let endpoint = slot.endpoint;
            let loss = slot.loss;
            let cur_period = slot.period;

            // Build one PERF_RECORD_SAMPLE honouring the event's `sample_type`
            // (validated at open to set IP and only supported bits). PID fields
            // use the view captured by the perf event; the scheduler TaskId
            // never crosses the Linux perf ABI boundary. A system-wide counter
            // can overflow while a kernel task is running, in which case there
            // is no Linux PID identity and the wire fields remain zero.
            let (pid, tid) = slot
                .owner_ids
                .map_or_else(|| current_sample_ids(slot.observer), |(pid, tid)| {
                    (Some(pid), Some(tid))
                });
            let time = ax_runtime::hal::time::monotonic_time_nanos();
            let cpu = ax_hal::percpu::this_cpu_id() as u32;
            let read_len = usize::from(slot.read_len).min(MAX_SAMPLE_READ_EVENTS);
            let mut read_values = [SampleReadValue::default(); MAX_SAMPLE_READ_EVENTS];
            if read_len != 0 {
                // The first entry is the sampling source. Account the completed
                // period on every overflow even if userspace did not request a
                // READ payload, so a later read(perf_fd) sees the sample count.
                read_values[0] = slot.read_entries[0].read(n, time, cur_period, true);
                if sample_type & PERF_SAMPLE_READ != 0 {
                    for (index, value) in read_values
                        .iter_mut()
                        .enumerate()
                        .take(read_len)
                        .skip(1)
                    {
                        *value = slot.read_entries[index].read(n, time, cur_period, false);
                    }
                }
            }
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
            };
            let len = build_sample(&mut record, sample_type, misc, &data);

            if !endpoint.is_null() && !loss.is_null() {
                // SAFETY: unregister happens before either owner Arc is
                // released or exchanged, so both pointers remain valid for the
                // complete handler critical section.
                unsafe { (&*endpoint).write_sample(&*loss, id, &record[..len]) };
            }

            // Frequency mode: adapt the period toward the target rate and persist it
            // (plus the sample timestamp) in the slot for the next interval. Fixed
            // mode re-arms with the unchanged period.
            let next_period = if slot.freq {
                let np = if slot.last_time != 0 {
                    next_freq_period(
                        cur_period,
                        slot.target_freq,
                        time.saturating_sub(slot.last_time),
                    )
                } else {
                    cur_period
                };
                slot.period = np;
                slot.last_time = time;
                np
            } else {
                cur_period
            };

            // Re-arm the counter for the next sample.
            ax_cpu::pmu::counter::preload(n, next_period);

            true
        };
        // SAFETY: the handler runs with local IRQs masked on its current CPU,
        // so the registry cannot be re-entered or accessed after migration.
        let sampled = unsafe { with_registry_mut(sample) };
        if !sampled {
            continue;
        }
    }

    // Clear exactly the overflow bits we serviced.
    ax_cpu::pmu::overflow::clear(handled);
    IrqReturn::Handled
}

fn build_callchain(
    interrupted: Option<ax_cpu::pmu::InterruptedContext>,
    ip: usize,
    is_user: bool,
    chain: &mut [u64],
) -> usize {
    let Some((marker, region)) = chain.split_first_mut() else {
        return 0;
    };
    *marker = if is_user {
        (-512i64) as u64
    } else {
        (-128i64) as u64
    };
    let frames = match interrupted {
        Some(context) if is_user => {
            super::unwind::user_callchain(ip, context.fp, context.sp, region)
        }
        Some(context) => super::unwind::kernel_callchain(ip, context.fp, region),
        None => {
            let Some(leaf) = region.first_mut() else {
                return 1;
            };
            *leaf = ip as u64;
            1
        }
    };
    1 + frames
}

fn current_sample_ids(observer: PidNamespaceId) -> (Option<TgidNumber>, Option<TidNumber>) {
    let task = ax_task::current();
    let Some(thread) = task.try_as_thread() else {
        return (None, None);
    };
    let tid = thread
        .pid_identity()
        .visible_number_in(observer)
        .map(TidNumber::from);
    let pid = thread
        .proc_data
        .identity()
        .visible_number_in(observer)
        .map(TgidNumber::from);
    (pid, tid)
}

#[cfg(all(test, axtest))]
fn kernel_task_sample_ids_are_empty_for_test() -> bool {
    let (pid, tid) = current_sample_ids(crate::task::ROOT_PID_NS.id());
    pid.is_none() && tid.is_none()
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
/// 11. `READ` → Linux single/group `read_format` layout
/// 12. `CALLCHAIN` → `u64 nr`, followed by context markers and IPs
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
            let id = d.read_entries.first().map_or(0, |entry| entry.id);
            put!(value.value);
            if d.read_format & super::PERF_FORMAT_TOTAL_TIME_ENABLED != 0 {
                put!(value.time_enabled);
            }
            if d.read_format & super::PERF_FORMAT_TOTAL_TIME_RUNNING != 0 {
                put!(value.time_running);
            }
            if d.read_format & super::PERF_FORMAT_ID != 0 {
                put!(id);
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

    // Back-patch the header's `size` field now that the total length is known.
    buf[size_off..size_off + 2].copy_from_slice(&(off as u16).to_ne_bytes());
    off
}

/// Writes one record into a perf ring buffer, IRQ-safe and self-contained.
///
/// Page 0 of `[ring_vaddr, ring_vaddr + ring_len)` is a
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
/// `ring_vaddr` must point at a live, kernel-mapped ring of `ring_len` bytes
/// (header page + data region) whose header was initialized by
/// `HwPerfEvent::device_mmap`. The caller must ensure no concurrent kernel
/// writer touches the same ring (guaranteed here: one counter ⇒ one writer, and
/// the handler runs with local IRQs masked).
unsafe fn ring_write_locked(ring_vaddr: usize, ring_len: usize, record: &[u8]) -> bool {
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

/// Write one record into a sampling ring from **process context** (the side-band
/// path: `PERF_RECORD_MMAP2` / `COMM` / `FORK` / `EXIT` emitted at execve / mmap /
/// clone / exit), serialized against the overflow handler.
///
/// The overflow handler ([`pmu_overflow_handler`]) writes the same ring in hard-
/// IRQ context on this core; a process-context writer must therefore mask local
/// IRQs ([`NoPreemptIrqSave`]) so the handler cannot run mid-write and interleave
/// a sample at the same `data_head`. On a single core this fully serializes the
/// two writers (M2 scope). The actual copy + head publish reuses [`ring_write`].
///
/// # Safety
///
/// Same contract as [`ring_write`]: `ring_vaddr`/`ring_len` must describe a live,
/// kernel-mapped ring (header page + data region) whose pages stay pinned for the
/// duration of the call (the event holds the backing `Arc` while the slot/ring is
/// registered).
pub fn ring_write_process(endpoint: &RingEndpoint, record: &[u8]) -> bool {
    endpoint.write_record(record)
}

#[cfg(test)]
mod tests {
    #[cfg(all(test, axtest, target_arch = "aarch64"))]
    #[axtest::axtest]
    fn kernel_task_sample_ids_are_empty() {
        assert!(super::kernel_task_sample_ids_are_empty_for_test());
    }
}
