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

use core::sync::atomic::Ordering;

use ax_hal::irq::{IrqContext, IrqId, IrqReturn};
use ax_kernel_guard::NoPreemptIrqSave;
use ax_task::IrqNotify;
use kbpf_basic::linux_bpf::perf_event_mmap_page;

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
/// `PERF_RECORD_MISC_KERNEL`: the sample landed in kernel (EL1) context.
const PERF_RECORD_MISC_KERNEL: u16 = 1;
/// `PERF_RECORD_MISC_USER`: the sample landed in user (EL0) context.
const PERF_RECORD_MISC_USER: u16 = 2;

/// Upper bound on a single `PERF_RECORD_SAMPLE` we emit: 8-byte header plus at
/// most nine 8-byte scalar fields (IDENTIFIER, IP, TID(pid+tid), TIME, ADDR, ID,
/// STREAM_ID, CPU(cpu+res), PERIOD). [`build_sample`] writes into a stack buffer
/// of this size and returns the actual length.
const SAMPLE_RECORD_MAX_LEN: usize = 8 + 9 * 8;

// `perf_event_sample_format` bits (see `man perf_event_open`). Only the scalar
// fields below are supported; every other bit (READ, CALLCHAIN, RAW,
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

/// Every `sample_type` bit the sampling backend can emit a well-formed
/// `PERF_RECORD_SAMPLE` for. A sampling event whose `sample_type` sets any bit
/// outside this mask is rejected at open ([`super::hw`] reuses this constant);
/// real `perf record` sets `IP|TID|TIME|PERIOD`, all within the mask.
pub const SUPPORTED_SAMPLE_TYPE: u64 = PERF_SAMPLE_IP
    | PERF_SAMPLE_TID
    | PERF_SAMPLE_TIME
    | PERF_SAMPLE_ADDR
    | PERF_SAMPLE_ID
    | PERF_SAMPLE_CPU
    | PERF_SAMPLE_PERIOD
    | PERF_SAMPLE_STREAM_ID
    | PERF_SAMPLE_IDENTIFIER;

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
        let notify_ptr = slot.notify;
        let ring_vaddr = slot.ring_vaddr;
        let ring_len = slot.ring_len;
        let cur_period = slot.period;

        // Build one PERF_RECORD_SAMPLE honouring the event's `sample_type`
        // (validated at open to set IP and only supported bits). pid/tid are
        // best-effort: the interrupted task's scheduler id (non-zero, stable per
        // task) — enough for perf to parse + count samples; precise user TID is a
        // future refinement. time/cpu are the real interrupt-time values.
        let tid = ax_task::current().id().as_u64() as u32;
        let time = ax_runtime::hal::time::monotonic_time_nanos();
        let cpu = ax_hal::percpu::this_cpu_id() as u32;
        let mut record = [0u8; SAMPLE_RECORD_MAX_LEN];
        let data = SampleData {
            ip,
            pid: tid, // best-effort: same scheduler id for pid and tid
            tid,
            time,
            addr: 0,
            id,
            stream_id: 0,
            cpu,
            period: cur_period as u64,
        };
        let len = build_sample(&mut record, sample_type, misc, &data);

        // SAFETY: `ring_vaddr`/`ring_len` describe live, kernel-mapped pages for
        // as long as the slot is registered (the event pins them, and teardown
        // unregisters before freeing). `ring_write` only touches that region.
        unsafe { ring_write(ring_vaddr, ring_len, &record[..len]) };

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
struct SampleData {
    ip: u64,
    pid: u32,
    tid: u32,
    time: u64,
    addr: u64,
    id: u64,
    stream_id: u64,
    cpu: u32,
    period: u64,
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
unsafe fn ring_write(ring_vaddr: usize, ring_len: usize, record: &[u8]) {
    // Guard the enable-before-mmap case (slot registered with a zero ring) and
    // any ring too small to even hold the header page: there is nowhere to
    // write, and the header pointer would be null/out of bounds.
    if ring_vaddr == 0 || ring_len < core::mem::size_of::<perf_event_mmap_page>() {
        return;
    }

    let header = ring_vaddr as *mut perf_event_mmap_page;

    // SAFETY: `header` points at the initialized header page.
    let data_offset =
        unsafe { core::ptr::addr_of!((*header).data_offset).read_volatile() } as usize;
    let data_size = unsafe { core::ptr::addr_of!((*header).data_size).read_volatile() } as usize;

    // Defensive: a malformed/zero header (no data region, or a data window that
    // does not fit in the buffer) means there is nowhere safe to write.
    if data_size == 0 || data_offset > ring_len || data_offset + data_size > ring_len {
        return;
    }

    let len = record.len();
    if len > data_size {
        return;
    }

    // SAFETY: header page is initialized; these are plain u64 fields.
    let head = unsafe { core::ptr::addr_of!((*header).data_head).read_volatile() };
    let tail = unsafe { core::ptr::addr_of!((*header).data_tail).read_volatile() };

    // Would this record overwrite bytes the reader has not consumed yet? Drop it
    // if so (back-pressure; no lost-record accounting in M2).
    if head.wrapping_sub(tail).wrapping_add(len as u64) > data_size as u64 {
        return;
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
