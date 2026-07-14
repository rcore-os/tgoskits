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

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use ax_hal::irq::{IrqContext, IrqId, IrqReturn};
use ax_kernel_guard::NoPreemptIrqSave;
use ax_task::IrqNotify;
use kbpf_basic::linux_bpf::perf_event_mmap_page;

use crate::task::AsThread;

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
/// `PERF_RECORD_LOST` discriminant: an in-band record telling `perf report`/
/// `perf script` how many samples the ring dropped, so they show "LOST n events!"
/// and place the gap on the timeline (the read-only `PERF_FORMAT_LOST` total is a
/// coarser substitute).
const PERF_RECORD_LOST: u32 = 2;
/// Byte length of a `PERF_RECORD_LOST`: `perf_event_header` (8) + `u64 id` +
/// `u64 lost`.
const PERF_RECORD_LOST_LEN: usize = 8 + 8 + 8;
/// `PERF_RECORD_MISC_KERNEL`: the sample landed in kernel (EL1) context.
const PERF_RECORD_MISC_KERNEL: u16 = 1;
/// `PERF_RECORD_MISC_USER`: the sample landed in user (EL0) context.
const PERF_RECORD_MISC_USER: u16 = 2;

/// Max u64 words in the `PERF_SAMPLE_READ` block: for a single event this is
/// `value` (+ `id` if `PERF_FORMAT_ID`) ≤ 2; for a group-leader read
/// (`PERF_FORMAT_GROUP`) it is `nr`, then per event (leader + up to
/// [`MAX_GROUP_MEMBERS`] members) `value` (+ `id`) — the worst case, which bounds
/// both the handler's on-stack scratch and the record buffer.
const MAX_GROUP_READ_WORDS: usize = 1 + (1 + MAX_GROUP_MEMBERS) * 2;

/// Upper bound on a single `PERF_RECORD_SAMPLE` we emit: 8-byte header, at most
/// nine 8-byte scalar fields (IDENTIFIER, IP, TID(pid+tid), TIME, ADDR, ID,
/// STREAM_ID, CPU(cpu+res), PERIOD), then an optional `PERF_SAMPLE_READ` block
/// ([`MAX_GROUP_READ_WORDS`] u64, worst case a group-leader read) and an optional
/// callchain block — a `u64 nr` count followed by up to two `PERF_CONTEXT_*`
/// markers and `2 * MAX_STACK_DEPTH` instruction pointers (a kernel + a user
/// region). [`build_sample`] writes into a stack buffer of this size and returns
/// the actual length.
const SAMPLE_RECORD_MAX_LEN: usize =
    8 + 9 * 8 + MAX_GROUP_READ_WORDS * 8 + (2 + 2 * MAX_STACK_DEPTH) * 8;

// `perf_event_sample_format` bits (see `man perf_event_open`). The scalar fields
// below plus `PERF_SAMPLE_CALLCHAIN` are supported; every other bit (READ, RAW,
// BRANCH_STACK, REGS_USER/INTR, STACK_USER, WEIGHT, DATA_SRC, TRANSACTION,
// PHYS_ADDR, …) is rejected at open time.
/// `PERF_SAMPLE_IP`: instruction pointer. Always set by real `perf` for samples.
const PERF_SAMPLE_IP: u64 = 1 << 0;
/// `PERF_SAMPLE_TID`: thread + process id (`u32 pid, u32 tid`).
const PERF_SAMPLE_TID: u64 = 1 << 1;
/// `PERF_SAMPLE_TIME`: monotonic timestamp (`u64`).
const PERF_SAMPLE_TIME: u64 = 1 << 2;
/// `PERF_SAMPLE_ADDR`: data address (`u64`); always 0 for our IP samples.
const PERF_SAMPLE_ADDR: u64 = 1 << 3;
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
/// `PERF_SAMPLE_CALLCHAIN`: per-sample call stack — a `u64 nr` count then `nr`
/// u64 instruction pointers, split into kernel/user regions by the
/// `PERF_CONTEXT_*` markers below. Set by `perf record -g` / `--call-graph fp`.
const PERF_SAMPLE_CALLCHAIN: u64 = 1 << 5;
/// `PERF_SAMPLE_READ`: each sample carries the event's read value (`read(2)`'s
/// `read_format` block). Supported for a single event with `read_format` in
/// `{0, PERF_FORMAT_ID}` — the sample emits the running count and, if requested,
/// the event id. Group-leader sampling (`read_format & PERF_FORMAT_GROUP`) and
/// the `TOTAL_TIME_*` fields need per-event/per-group accounting reachable from
/// the IRQ handler and are rejected at open (see `perf_event_open_hw`).
pub const PERF_SAMPLE_READ: u64 = 1 << 10;

/// `read_format` bit `PERF_FORMAT_ID` (mirrors `super::PERF_FORMAT_ID`), needed
/// here to lay out the `PERF_SAMPLE_READ` block.
const READ_FORMAT_ID: u64 = 1 << 2;
/// `read_format` bit `PERF_FORMAT_GROUP` (mirrors `super::PERF_FORMAT_GROUP`):
/// a group-leader sample carries the WHOLE group's counters (`nr`, then the
/// leader's value and each member's), not just the leader's. Supported for a
/// **per-task** sampling leader — its counting members' live counts are read from
/// the overflow handler — and rejected for the system-wide path (see
/// `super::hw::perf_event_open_hw`). Exposed for that open-time gate.
pub const READ_FORMAT_GROUP: u64 = 1 << 3;

/// Callchain marker: the entries that follow are kernel (EL1) instruction
/// pointers (Linux `PERF_CONTEXT_KERNEL`). Counts as one callchain entry.
const PERF_CONTEXT_KERNEL: u64 = (-128i64) as u64;
/// Callchain marker: the entries that follow are user (EL0) instruction pointers
/// (Linux `PERF_CONTEXT_USER`). Counts as one callchain entry.
const PERF_CONTEXT_USER: u64 = (-512i64) as u64;

/// Per-region cap on callchain depth (the kernel and user regions are bounded
/// separately). Sizes the fixed on-stack chain and record buffers, so it stays
/// allocation-free in the overflow handler.
const MAX_STACK_DEPTH: usize = 64;

/// Every `sample_type` bit the sampling backend can emit a well-formed
/// `PERF_RECORD_SAMPLE` for. A sampling event whose `sample_type` sets any bit
/// outside this mask is rejected at open ([`super::hw`] reuses this constant);
/// real `perf record` sets `IP|TID|TIME|PERIOD`, and `-g` adds `CALLCHAIN`, all
/// within the mask.
pub const SUPPORTED_SAMPLE_TYPE: u64 = PERF_SAMPLE_IP
    | PERF_SAMPLE_TID
    | PERF_SAMPLE_TIME
    | PERF_SAMPLE_ADDR
    | PERF_SAMPLE_ID
    | PERF_SAMPLE_CPU
    | PERF_SAMPLE_PERIOD
    | PERF_SAMPLE_STREAM_ID
    | PERF_SAMPLE_IDENTIFIER
    | PERF_SAMPLE_CALLCHAIN
    | PERF_SAMPLE_READ;

/// Whether a sampling event's `PERF_SAMPLE_READ` request is supported. `true`
/// unless `PERF_SAMPLE_READ` is combined with a `read_format` bit outside
/// `{PERF_FORMAT_ID, PERF_FORMAT_GROUP}`: the leader value, per-member ids, and
/// (group) member counts are all reachable from the IRQ handler, but the
/// time-format (`TOTAL_TIME_*`) and `LOST` reads need per-event/per-group
/// accounting that is not, so they stay rejected. Note `PERF_FORMAT_GROUP` here
/// is gated further to the per-task path by [`super::hw::perf_event_open_hw`]
/// (system-wide group-leader sampling has no member plumbing in v1). Checked at
/// open.
pub fn sample_read_supported(sample_type: u64, read_format: u64) -> bool {
    sample_type & PERF_SAMPLE_READ == 0 || read_format & !(READ_FORMAT_ID | READ_FORMAT_GROUP) == 0
}

/// Maximum number of counting members a group-leader sampling event carries in
/// its `PERF_SAMPLE_READ | PERF_FORMAT_GROUP` block. Bounds the fixed member
/// table in [`SampleSlot`] (kept `Copy`/alloc-free for the per-CPU registry) and
/// the on-stack record buffer. A per-task PMU has ≤ `PMCR.N` (~6) programmable
/// counters, so a leader plus this many members covers any realistically
/// co-schedulable sampled group; extra members are dropped from the read (warned
/// at link time — see [`super::task::link_group_member`]).
pub const MAX_GROUP_MEMBERS: usize = 7;

/// [`GroupMember::slot`] sentinel matching [`super::task`]'s `NO_SLOT`: the member
/// holds no programmable counter this slice, so only its `accumulated` (not a
/// live banked-counter read) contributes to the group value.
const NO_SLOT: usize = usize::MAX;

/// One counting group member, baked into the leader's [`SampleSlot`] at slice-arm
/// time so the overflow handler can emit its live value in a `PERF_FORMAT_GROUP`
/// read WITHOUT walking the process-context group list from hard-IRQ.
///
/// The pointers target atomics on the member's [`super::task::PerTaskCounter`],
/// which stays alive (pinned by its `Thread`'s counter list) for as long as this
/// slot is registered — the same raw-pointer soundness discipline as
/// [`SampleSlot::notify`] / [`SampleSlot::lost`]. All four are read with plain
/// atomic loads (IRQ-safe); the live banked-counter read is guarded by the
/// member's `running` / `last_cpu` / `slot`, so a rotated-off or cross-core
/// member contributes only its `accumulated` (Linux "last value" semantics that
/// userspace scales via `time_running`).
#[derive(Clone, Copy)]
pub struct GroupMember {
    /// Unique event id for the member's `PERF_FORMAT_ID` entry (the wrapper id,
    /// mirrored onto the ptc, so it matches `PERF_EVENT_IOC_ID`).
    pub id: u64,
    /// `*const AtomicU64` — the member's accumulated completed-slice count.
    pub accumulated: *const (),
    /// `*const AtomicUsize` — the member's current programmable counter index, or
    /// [`NO_SLOT`] when it holds none this slice.
    pub slot: *const (),
    /// `*const AtomicUsize` — the logical CPU the member was last scheduled onto;
    /// the banked `PMEVCNTRn` is valid to read only when this is the sampling core.
    pub last_cpu: *const (),
    /// `*const AtomicBool` — whether the member holds a HW counter right now.
    pub running: *const (),
}

impl GroupMember {
    /// An empty descriptor for the fixed table's unused entries (null pointers,
    /// never dereferenced — the handler only reads indices `< n_members`). Also
    /// the initializer the per-task arm path fills the table from.
    pub const EMPTY: GroupMember = GroupMember {
        id: 0,
        accumulated: core::ptr::null(),
        slot: core::ptr::null(),
        last_cpu: core::ptr::null(),
        running: core::ptr::null(),
    };
}

/// Everything the overflow handler needs for one counter, in a lock-free,
/// alloc-free `Copy` POD.
///
/// Stored in the per-CPU [`REGISTRY`] at the counter's index while the event is
/// enabled. See the module docs for the `notify`-pointer soundness argument.
#[derive(Clone, Copy)]
pub struct SampleSlot {
    /// Kernel virtual address of the ring buffer's first page
    /// (`perf_event_mmap_page`). The data region follows at `data_offset`.
    pub ring_vaddr: usize,
    /// Total ring length in bytes (header page + data region).
    pub ring_len: usize,
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
    /// Raw pointer to the owning event's [`IrqNotify`], woken after each sample.
    /// Kept alive by the event's strong `Arc<IrqNotify>` for as long as the slot
    /// is registered (see module docs).
    pub notify: *const (),
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
    /// Raw pointer to the owning event's lost-sample `AtomicU64`, bumped each time
    /// a `PERF_RECORD_SAMPLE` is dropped because the ring is full. Read back by
    /// `read(perf_fd)` for `PERF_FORMAT_LOST`. Kept alive by the event for as long
    /// as the slot is registered (teardown unregisters first), exactly like
    /// [`notify`](Self::notify). Null when the event tracks no lost count.
    pub lost: *const (),
    /// Raw pointer to the owning event's *reported* lost `AtomicU64`: how many of
    /// [`lost`](Self::lost) have already been emitted as `PERF_RECORD_LOST`
    /// records. The handler flushes `lost - lost_reported` as an in-band record
    /// before a sample whenever the ring has room. Kept alive exactly like
    /// [`lost`](Self::lost); null when the event emits no in-band lost records.
    pub lost_reported: *const (),
    /// Real userspace `(tgid, tid)` of the event owner for a **per-task** sampling
    /// event, captured at slice-arm time when the monitored [`Thread`] is known.
    /// The overflow handler prefers this over `current()` so a sample is
    /// attributed to the monitored task even when the overflow IRQ is serviced
    /// after a context switch away from it (per-task skid). `None` for
    /// **system-wide** slots, where the handler attributes the sample to the
    /// interrupted `current()` — matching the sampled IP (Linux `perf record -a`
    /// semantics).
    ///
    /// [`Thread`]: crate::task::Thread
    pub owner_ids: Option<(u32, u32)>,
    /// `attr.read_format` — which fields a `PERF_SAMPLE_READ` block carries (only
    /// `value` and, if `PERF_FORMAT_ID`, the id are supported; validated at open).
    pub read_format: u64,
    /// Running event count emitted in the `PERF_SAMPLE_READ` block: incremented by
    /// the sampling `period` each overflow (each overflow means ~`period` more
    /// events counted). `0` for an event without `PERF_SAMPLE_READ`. Mutated in
    /// place by the handler.
    pub read_value: u64,
    /// Raw pointer to a persistent `AtomicU64` the handler mirrors
    /// [`read_value`](Self::read_value) into after each overflow, so a **per-task**
    /// event's running count survives slice re-arming (the slot is rebuilt each
    /// slice — [`super::task::arm_slice`] seeds `read_value` back from this sink).
    /// Null for a **system-wide** slot, whose registration persists for the whole
    /// run so its `read_value` already accumulates in place. Kept alive exactly
    /// like [`lost`](Self::lost) (the owning ptc outlives the slot).
    pub read_value_sink: *const (),
    /// Group-leader sampling (`read_format & PERF_FORMAT_GROUP`): the counting
    /// members whose live values this leader's `PERF_SAMPLE_READ` block emits,
    /// baked in at slice-arm time ([`super::task::arm_slice`]). Only indices `<
    /// n_members` are populated; the rest are [`GroupMember::EMPTY`].
    pub members: [GroupMember; MAX_GROUP_MEMBERS],
    /// Number of populated entries in [`members`](Self::members) (`<=
    /// MAX_GROUP_MEMBERS`). `0` for a single-event read or a non-group leader.
    pub n_members: u8,
}

// SAFETY: `SampleSlot` is a plain bag of integers plus raw pointers (`notify`,
// `lost`, `lost_reported`, and the per-member `GroupMember` atomics). Every
// pointer is only ever dereferenced from the overflow handler on the same CPU
// that registered the slot, and the registry is mutated only under a
// local-IRQ-off critical section, so there is no cross-thread aliasing of the
// pointees through this type. Marking it `Send` lets it live inside the per-CPU
// static; it is never actually moved across CPUs (the registry is per-CPU
// regardless, and the member atomics belong to the same task pinned on this
// core for the slice).
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
    let _guard = NoPreemptIrqSave::new();
    // SAFETY: preemption and local IRQs are disabled by `_guard`, so we hold
    // exclusive access to this CPU's `REGISTRY` for the critical section.
    let registry = unsafe { REGISTRY.current_ref_mut_raw() };
    registry[n] = Some(slot);
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
    let _guard = NoPreemptIrqSave::new();
    // SAFETY: see `register`.
    let registry = unsafe { REGISTRY.current_ref_mut_raw() };
    registry[n] = None;
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
    // Guarantee this core's PMU is brought up (PMCR.E set, clean slate) before
    // we arm an overflow on it. On secondary cores nothing else does this, so
    // without it the counter would never count / the overflow never fire.
    super::percpu::ensure_core_inited();

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
    let ip = ax_cpu::pmu::interrupted_pc();
    let is_user = ax_cpu::pmu::interrupted_is_user();

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
        // unregister disable local IRQs). Take a mutable borrow so frequency
        // mode can write the adapted period/`last_time` back into the slot.
        let registry = unsafe { REGISTRY.current_ref_mut_raw() };
        let Some(slot) = registry[n].as_mut() else {
            // Overflow on a counter with no sampling slot (e.g. a counting-only
            // event that happened to wrap with its IRQ somehow set): just clear
            // it below. Do not re-arm — counting events manage their own value.
            continue;
        };

        // Snapshot the fields the record + re-arm need (copied out so the slot
        // can be mutated below without aliasing the borrow).
        let sample_type = slot.sample_type;
        let id = slot.id;
        let read_format = slot.read_format;
        let notify_ptr = slot.notify;
        let lost_ptr = slot.lost;
        let lost_reported_ptr = slot.lost_reported;
        let ring_vaddr = slot.ring_vaddr;
        let ring_len = slot.ring_len;
        let cur_period = slot.period;
        let owner_ids = slot.owner_ids;

        // Build one PERF_RECORD_SAMPLE honouring the event's `sample_type`
        // (validated at open to set IP and only supported bits). pid/tid are the
        // real userspace (tgid, tid) so a sample keys on the SAME ids the
        // COMM/MMAP2 side-band records carry (process tgid + thread tid) and
        // `perf report` can join it to the right process/DSO map. For a per-task
        // event use the owner ids captured at slice-arm time: the overflow IRQ
        // can be serviced after a context switch away from the monitored task, so
        // `current()` would misattribute the sample; the captured owner is always
        // the right task. For a system-wide event (`owner_ids == None`) attribute
        // to the interrupted `current()`, which matches the sampled IP. The
        // `current()` reads are IRQ-safe: `try_as_thread` is a lock-free
        // `task_ext` downcast, `Process::pid` is a plain field read, and
        // `Thread::tid` is an atomic load; a kernel task has no `Thread`, so fall
        // back to the scheduler id. time/cpu are the real interrupt-time values.
        let (pid, tid) = match owner_ids {
            Some(ids) => ids,
            None => {
                let curr = ax_task::current();
                match curr.try_as_thread() {
                    Some(thr) => (thr.proc_data.proc.pid() as u32, thr.tid()),
                    None => {
                        let id = curr.id().as_u64() as u32;
                        (id, id)
                    }
                }
            }
        };
        let time = ax_runtime::hal::time::monotonic_time_nanos();
        let cpu = ax_hal::percpu::this_cpu_id() as u32;
        // Call stack for PERF_SAMPLE_CALLCHAIN (alloc-free, fixed on-stack buffer;
        // empty unless the event requested it). Kernel frames are unwound from the
        // interrupted x29; the user region is the leaf IP in M4a.
        let mut chain = [0u64; 2 + 2 * MAX_STACK_DEPTH];
        let nchain = if sample_type & PERF_SAMPLE_CALLCHAIN != 0 {
            build_callchain(ip, is_user, &mut chain)
        } else {
            0
        };
        // PERF_SAMPLE_READ block. Each overflow means ~`period` more events were
        // counted, so advance the leader's running total by the period. For a
        // single event the block is `value` (+ `id` if `PERF_FORMAT_ID`); for a
        // group-leader read (`PERF_FORMAT_GROUP`, per-task only) it is `nr`, then
        // the leader's value (+ id), then each counting member's live value (+ id)
        // — assembled alloc-free from the baked member table (`build_group_read`).
        let mut read_blk = [0u64; MAX_GROUP_READ_WORDS];
        let nread = if sample_type & PERF_SAMPLE_READ != 0 {
            slot.read_value = slot.read_value.wrapping_add(cur_period as u64);
            // Persist the running count so a per-task event's leader value stays
            // monotonic across slice re-arming (a system-wide slot persists in
            // place and leaves this sink null).
            if !slot.read_value_sink.is_null() {
                // SAFETY: the sink targets the owning ptc's `AtomicU64`, kept alive
                // while the slot is registered (teardown unregisters first), exactly
                // like the `lost` pointer.
                unsafe {
                    (*(slot.read_value_sink as *const AtomicU64))
                        .store(slot.read_value, Ordering::Relaxed)
                };
            }
            if read_format & READ_FORMAT_GROUP != 0 {
                build_group_read(slot, cpu as usize, &mut read_blk)
            } else {
                read_blk[0] = slot.read_value;
                if read_format & READ_FORMAT_ID != 0 {
                    read_blk[1] = id;
                    2
                } else {
                    1
                }
            }
        } else {
            0
        };
        let mut record = [0u8; SAMPLE_RECORD_MAX_LEN];
        let data = SampleData {
            ip,
            pid,
            tid,
            time,
            addr: 0,
            id,
            stream_id: 0,
            cpu,
            period: cur_period as u64,
            callchain: &chain[..nchain],
            read: &read_blk[..nread],
        };
        let len = build_sample(&mut record, sample_type, misc, &data);

        // Flush any not-yet-reported dropped samples as an in-band
        // `PERF_RECORD_LOST` before this sample, so `perf report` shows
        // "LOST n events!" on the timeline. Emitted only when the ring has room
        // (it was full when the drops happened); otherwise it stays pending and
        // is retried at the next sample, once userspace has drained the ring.
        if !lost_ptr.is_null() && !lost_reported_ptr.is_null() {
            // SAFETY: both pointers target the owning event's `AtomicU64`s, kept
            // alive while the slot is registered (teardown unregisters first).
            let total = unsafe { (*(lost_ptr as *const AtomicU64)).load(Ordering::Relaxed) };
            let reported =
                unsafe { (*(lost_reported_ptr as *const AtomicU64)).load(Ordering::Relaxed) };
            if total > reported {
                let mut lost_rec = [0u8; PERF_RECORD_LOST_LEN];
                build_lost_record(&mut lost_rec, id, total - reported);
                // SAFETY: as for the sample write below.
                if unsafe { ring_write(ring_vaddr, ring_len, &lost_rec) } {
                    // SAFETY: as above.
                    unsafe {
                        (*(lost_reported_ptr as *const AtomicU64)).store(total, Ordering::Relaxed)
                    };
                }
            }
        }

        // SAFETY: `ring_vaddr`/`ring_len` describe live, kernel-mapped pages for
        // as long as the slot is registered (the event pins them, and teardown
        // unregisters before freeing). `ring_write` only touches that region.
        let wrote = unsafe { ring_write(ring_vaddr, ring_len, &record[..len]) };
        if !wrote && !lost_ptr.is_null() {
            // The ring was full; account the dropped sample for PERF_FORMAT_LOST.
            // SAFETY: `lost_ptr` points at the owning event's `AtomicU64`, kept
            // alive while the slot is registered (teardown unregisters first).
            unsafe { (*(lost_ptr as *const AtomicU64)).fetch_add(1, Ordering::Relaxed) };
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

        // Wake the deferred worker so it can deliver POLLIN. A redirected event
        // (`PERF_EVENT_IOC_SET_OUTPUT` into another event's ring) writes into the
        // leader's ring but has no notify of its own — its `notify` is null, and
        // the leader's own poller re-checks `data_head` on its next poll. The
        // pointer, when non-null, is valid: the owning event holds the backing
        // `Arc<IrqNotify>` while registered (see the module-level soundness note).
        if !notify_ptr.is_null() {
            let notify = unsafe { &*(notify_ptr as *const IrqNotify) };
            notify.notify_irq();
        }
    }

    // Clear exactly the overflow bits we serviced.
    ax_cpu::pmu::overflow::clear(handled);
    IrqReturn::Handled
}

/// Fills `chain` with the interrupted call stack for `PERF_SAMPLE_CALLCHAIN`,
/// returning the number of `u64` entries written.
///
/// The layout mirrors Linux: a `PERF_CONTEXT_*` region marker followed by that
/// region's instruction pointers, leaf first — `[PERF_CONTEXT_KERNEL, ip, ra0,
/// …]` for a kernel sample, `[PERF_CONTEXT_USER, ip, ra0, …]` for a user sample.
/// Kernel frames are unwound from the interrupted `x29` via
/// [`super::unwind::kernel_callchain`], user frames via
/// [`super::unwind::user_callchain`] (through the IRQ-safe no-fault `TTBR0`
/// reader). If no frame pointer was published the region degrades to
/// `[marker, ip]` (never empty, so the sample is never dropped). Deep frames
/// appear only when the sampled code keeps frame pointers (the kernel when built
/// with `-Cforce-frame-pointers`, user binaries built `-fno-omit-frame-pointer`).
/// Allocation-free and safe from the overflow handler.
fn build_callchain(ip: u64, is_user: bool, chain: &mut [u64]) -> usize {
    // `chain` is the handler's fixed `[u64; 2 + 2 * MAX_STACK_DEPTH]` buffer, so
    // the leading fixed-index writes below are always in bounds. Each region is
    // capped at `MAX_STACK_DEPTH` frames (plus its one-word marker).
    chain[0] = if is_user {
        PERF_CONTEXT_USER
    } else {
        PERF_CONTEXT_KERNEL
    };
    let region_end = (1 + MAX_STACK_DEPTH).min(chain.len());
    let region = &mut chain[1..region_end];
    match ax_cpu::pmu::interrupted_fp() {
        Some(fp) if is_user => {
            // Bound the user walk to the interrupted user stack (SP_EL0); fall back
            // to the frame pointer itself as the window anchor if unavailable.
            let sp = ax_cpu::pmu::interrupted_sp().unwrap_or(fp);
            1 + super::unwind::user_callchain(ip as usize, fp, sp, region)
        }
        Some(fp) => 1 + super::unwind::kernel_callchain(ip as usize, fp, region),
        None => {
            // No frame pointer published (e.g. sampled on a path that does not
            // plumb it): emit the leaf IP alone rather than dropping the region.
            chain[1] = ip;
            2
        }
    }
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
/// 11. `CALLCHAIN` → `u64 nr`, then `nr` u64 entries (`PERF_CONTEXT_*` markers +
///     instruction pointers) from `d.callchain`
///
/// `buf` must be at least [`SAMPLE_RECORD_MAX_LEN`] bytes. With
/// `sample_type == PERF_SAMPLE_IP` exactly, the result is the original 16-byte
/// IP-only record (8-byte header + `u64 ip`).
/// The per-sample values [`build_sample`] may emit (those not implied by
/// `sample_type` alone). Gathered by the overflow handler at interrupt time.
struct SampleData<'a> {
    ip: u64,
    pid: u32,
    tid: u32,
    time: u64,
    addr: u64,
    id: u64,
    stream_id: u64,
    cpu: u32,
    period: u64,
    /// The `PERF_SAMPLE_CALLCHAIN` entries (`PERF_CONTEXT_*` markers + IPs), or an
    /// empty slice when the event did not request a callchain. Emitted verbatim as
    /// the `nr` count followed by the entries. Borrows the handler's fixed on-stack
    /// chain buffer.
    callchain: &'a [u64],
    /// The `PERF_SAMPLE_READ` block (`value`, optionally `id`), or an empty slice
    /// when the event did not request `PERF_SAMPLE_READ`. Emitted verbatim.
    read: &'a [u64],
}

/// Serialize a `PERF_RECORD_LOST` into `buf`: `perf_event_header` (`type`,
/// `misc = 0`, `size`) followed by the event `id` and the `lost` count, all
/// native-endian — the layout `perf report` expects for a type-2 record.
fn build_lost_record(buf: &mut [u8; PERF_RECORD_LOST_LEN], id: u64, lost: u64) {
    buf[0..4].copy_from_slice(&PERF_RECORD_LOST.to_ne_bytes());
    buf[4..6].copy_from_slice(&0u16.to_ne_bytes()); // misc
    buf[6..8].copy_from_slice(&(PERF_RECORD_LOST_LEN as u16).to_ne_bytes());
    buf[8..16].copy_from_slice(&id.to_ne_bytes());
    buf[16..24].copy_from_slice(&lost.to_ne_bytes());
}

/// Assemble a group-leader `PERF_SAMPLE_READ` block into `out`, returning the
/// number of `u64` words written. The layout mirrors Linux's `PERF_FORMAT_GROUP`
/// read (v1 supports `GROUP`, optionally `| PERF_FORMAT_ID`; `TOTAL_TIME_*` /
/// `LOST` are rejected at open): `nr`, then the leader's `value` (+ `id`), then
/// per counting member its `value` (+ `id`).
///
/// The leader's value is its synthetic period-advanced running count — a sampling
/// leader has no count-from-zero (its counter is preloaded to wrap after
/// `period`). Each member's value is `accumulated + live-slice`: the live slice
/// (one banked `PMEVCNTRn` read) is added only when the member holds a counter on
/// THIS core (`running && last_cpu == this_cpu && slot != NO_SLOT`), exactly the
/// guard [`super::task::read_values`] uses; a degraded / rotated-off / cross-core
/// member contributes just its `accumulated` (Linux "last value" semantics).
///
/// `out` is the handler's fixed `[u64; MAX_GROUP_READ_WORDS]`, sized so the worst
/// case (`nr` + (leader + [`MAX_GROUP_MEMBERS`]) * (value + id)) always fits.
fn build_group_read(slot: &SampleSlot, this_cpu: usize, out: &mut [u64]) -> usize {
    let want_id = slot.read_format & READ_FORMAT_ID != 0;
    let n = slot.n_members as usize;
    let mut w = 0usize;
    out[w] = 1 + n as u64; // nr: leader + members
    w += 1;
    // Leader entry: the synthetic running count (advanced by the caller).
    out[w] = slot.read_value;
    w += 1;
    if want_id {
        out[w] = slot.id;
        w += 1;
    }
    for m in &slot.members[..n] {
        // SAFETY: for indices `< n_members` every pointer targets a live atomic on
        // the member's `PerTaskCounter`, pinned by its `Thread`'s counter list for
        // as long as this slot is registered (unregistered before the ptc drops).
        let value = unsafe {
            let acc = (*(m.accumulated as *const AtomicU64)).load(Ordering::Relaxed);
            let running = (*(m.running as *const AtomicBool)).load(Ordering::Relaxed);
            let last_cpu = (*(m.last_cpu as *const AtomicUsize)).load(Ordering::Relaxed);
            let cslot = (*(m.slot as *const AtomicUsize)).load(Ordering::Relaxed);
            if running && last_cpu == this_cpu && cslot != NO_SLOT {
                // The banked `PMEVCNTRn` read is per-PE, valid only on the sampling
                // core — guaranteed by the guard above.
                acc.wrapping_add(ax_cpu::pmu::counter::read(cslot))
            } else {
                acc
            }
        };
        out[w] = value;
        w += 1;
        if want_id {
            out[w] = m.id;
            w += 1;
        }
    }
    w
}

fn build_sample(buf: &mut [u8], sample_type: u64, misc: u16, d: &SampleData<'_>) -> usize {
    // Cursor into `buf`. All offsets stay within `SAMPLE_RECORD_MAX_LEN` because
    // at most the header + 9 u64 scalar fields + the callchain block (`nr` plus at
    // most `2 + 2*MAX_STACK_DEPTH` entries) are written, and the caller passes a
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
        put!(d.pid);
        put!(d.tid);
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
        // The `read_format` block (`value`, optionally `id`), pre-built by the
        // handler; empty when the event did not request `PERF_SAMPLE_READ`.
        for &v in d.read {
            put!(v);
        }
    }
    if sample_type & PERF_SAMPLE_CALLCHAIN != 0 {
        // `u64 nr` count (the `PERF_CONTEXT_*` markers count as entries) followed
        // by the entries themselves, exactly as Linux lays out the block.
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
/// Returns `true` if the record was written, `false` if it was dropped because it
/// would overwrite still-unread bytes (`data_head - data_tail + len > data_size`);
/// on drop `data_head` is not advanced and the caller bumps the event's
/// `PERF_FORMAT_LOST` counter so `perf record` can report the loss.
///
/// # Safety
///
/// `ring_vaddr` must point at a live, kernel-mapped ring of `ring_len` bytes
/// (header page + data region) whose header was initialized by
/// `HwPerfEvent::device_mmap`. The caller must ensure no concurrent kernel
/// writer touches the same ring (guaranteed here: one counter ⇒ one writer, and
/// the handler runs with local IRQs masked).
unsafe fn ring_write(ring_vaddr: usize, ring_len: usize, record: &[u8]) -> bool {
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
    // if so (back-pressure). Returning `false` lets the caller bump the event's
    // lost-sample counter (`PERF_FORMAT_LOST`).
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
pub unsafe fn ring_write_process(ring_vaddr: usize, ring_len: usize, record: &[u8]) {
    let _guard = NoPreemptIrqSave::new();
    // SAFETY: caller upholds the ring liveness contract; IRQs are masked so the
    // overflow handler cannot race this write on the current core.
    unsafe { ring_write(ring_vaddr, ring_len, record) };
}
