//! Per-task hardware-PMU `perf` counting (`perf stat -- cmd`).
//!
//! Where [`super::hw`] in `pid <= 0` mode counts on the *current* CPU
//! system-wide (M0–M2), this module counts a *specific task*: the counter is
//! programmed onto hardware only while the target task is the running task, and
//! its per-slice deltas are accumulated across context switches. That is what
//! makes `perf stat -- /bin/true` attribute events to the workload rather than
//! to whatever happened to run on the CPU.
//!
//! ## Ownership and lifetime
//!
//! A [`PerTaskCounter`] is shared (`Arc`) between two places:
//!
//! * the target [`Thread`]'s `perf_counters` list, walked by the scheduler
//!   hooks ([`perf_sched_in`] / [`perf_sched_out`]) and the exec/exit hooks, and
//! * the [`super::hw::HwPerfEvent`] behind the perf fd, which serves
//!   `read(perf_fd)` / `ioctl(ENABLE/DISABLE/RESET)` and frees the HW counter on
//!   `Drop`.
//!
//! Both can outlive the other (the fd can be `close`d while the task runs, or
//! the task can exit while the fd is still open), so the HW counter is freed via
//! the idempotent [`free_hw`] from whichever side reaches end-of-life first
//! ([`HwPerfEvent::drop`] or [`on_task_exit`]).
//!
//! ## Hot-path cost
//!
//! The scheduler hooks run inside `switch_to` with IRQs disabled and preemption
//! off: no allocation, no sleeping locks. They early-return on a single relaxed
//! load of [`PERF_TASK_ACTIVE`] when no per-task counter exists anywhere, so the
//! common (perf-unused) case is one atomic load per switch.
//!
//! ## Per-task sampling (`perf record -- cmd`, M3-pt-rec)
//!
//! A per-task event opened with `pid > 0` AND `sample_period > 0` (and
//! `sample_type == PERF_SAMPLE_IP`) behaves like an [M2 sampling
//! event](super::sampling) *while the attached task is running*, and fires no
//! samples while it is not — so the samples are attributed to the task.
//!
//! This reuses the M2 IRQ backend wholesale. The mechanism is:
//!
//! * `mmap(perf_fd)` allocates the ring (in [`super::hw::HwPerfEvent::device_mmap`])
//!   and stashes the ring vaddr/len + the page/notify anchors onto the shared
//!   [`PerTaskCounter`] via [`PerTaskCounter::set_ring`].
//! * [`perf_sched_in`] arms the slice: `preload` the counter to overflow after
//!   `sample_period` events, `register` a [`SampleSlot`](super::sampling::SampleSlot)
//!   pointing at the ptc's ring + notify, and `enable_irq` the overflow line.
//! * [`perf_sched_out`] disarms the slice: stop the counter, `disable_irq`, and
//!   `unregister` the slot — so the next time some *other* task runs, an overflow
//!   on this counter cannot fire a sample into our ring.
//!
//! The IRQ-half (the overflow handler writing `PERF_RECORD_SAMPLE` and re-arming)
//! is exactly the M2 [`super::sampling::pmu_overflow_handler`] — nothing here
//! runs in IRQ context except via the registered slot.
//!
//! ## Scope / deferrals
//!
//! Single-core M2 scope: no counter multiplexing (so `time_running ==
//! time_enabled`), no cross-core migration. Sampling supports both fixed-period
//! (`-c <period>`) and frequency mode (`-F`, `sample_freq`); in frequency mode
//! the overflow handler adapts the period per slice toward the target rate.
//! `attr.inherit` (following `fork`/`clone` children) is deferred: the counter
//! follows the single attached task only.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use ax_alloc::GlobalPage;
use ax_kspin::SpinNoIrq;
use ax_runtime::hal::paging::MappingFlags;
use ax_task::IrqNotify;

use super::{
    hw,
    sampling::{self, SampleSlot},
    sideband::{self, Mmap2Info, SidebandTarget},
};
use crate::task::Thread;

// `PROT_*` / `MAP_*` values for the `prot`/`flags` fields of MMAP2 records.
const PROT_READ: u32 = 1;
const PROT_WRITE: u32 = 2;
const PROT_EXEC: u32 = 4;
const MAP_SHARED: u32 = 1;
const MAP_PRIVATE: u32 = 2;

/// Number of per-task counters currently attached anywhere in the system.
///
/// Incremented by [`attach`] and decremented by [`free_hw`] (when the HW counter
/// is released). The scheduler hooks early-return while this is `0`, so an
/// idle perf subsystem costs one relaxed atomic load per context switch.
static PERF_TASK_ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// A hardware counter bound to one specific task.
///
/// Interior-mutable and allocation-free so the scheduler hooks can drive it with
/// IRQs disabled. The counter occupies a *programmable* PMU slot (`n`) even for
/// `CPU_CYCLES` (ARM event `0x11`), so it never contends with a system-wide
/// cycle-counter event using the dedicated `PMCCNTR_EL0`.
///
/// State machine (per slice):
///
/// * `enabled` — userspace wants this event counting (set at open if
///   `!disabled`, by `enable_on_exec` on exec, or by `ioctl(ENABLE)`).
/// * `running` — the event is programmed onto HW *right now* (i.e. the target
///   task is the running task and `enabled`). Set in [`perf_sched_in`], cleared
///   in [`perf_sched_out`].
///
/// Because [`ax_cpu::pmu::counter::configure`] resets the counter to 0, each
/// slice starts at 0 and the slice delta is exactly `counter::read(n)` at
/// sched-out time; [`PerTaskCounter::accumulated`] sums those deltas.
#[derive(Debug)]
pub struct PerTaskCounter {
    /// Programmable PMU counter index (`0..num_counters`) reserved from the M1
    /// allocator. Per-task events never use the dedicated cycle counter.
    n: usize,
    /// ARM PMUv3 event number programmed into `PMEVTYPERn_EL0`.
    event: u16,
    /// `attr.exclude_user`: do not count EL0 (`PMEVTYPERn_EL0.U`).
    exclude_user: bool,
    /// `attr.exclude_kernel`: do not count EL1 (`PMEVTYPERn_EL0.P`).
    exclude_kernel: bool,
    /// `attr.read_format`, controlling which fields `read(perf_fd)` emits.
    read_format: u64,
    /// `attr.enable_on_exec`: start counting only when the attached task
    /// `execve`s a new image (consumed by [`on_exec`]).
    enable_on_exec: bool,

    /// Userspace wants this event counting (see the struct-level state machine).
    enabled: AtomicBool,
    /// The event is programmed onto HW right now (target task is running).
    running: AtomicBool,
    /// Sum of completed-slice deltas (raw event count).
    accumulated: AtomicU64,
    /// Accumulated enabled time across past windows (ns).
    time_enabled_ns: AtomicU64,
    /// Accumulated running time across past windows (ns). Equal to
    /// `time_enabled_ns` with no multiplexing.
    time_running_ns: AtomicU64,
    /// Monotonic ns timestamp of the last [`perf_sched_in`] (live slice start).
    last_in_ns: AtomicU64,
    /// Monotonic ns timestamp at which the event last became `enabled`.
    /// Unused for the no-multiplexing timing math but kept for parity with the
    /// system-wide path and future multiplexing accounting.
    enabled_at_ns: AtomicU64,
    /// The attached task has exited: the hooks must stop touching HW for it.
    dead: AtomicBool,
    /// The HW counter slot has been released back to the allocator. Guards
    /// [`free_hw`] against double-free across the fd-`Drop` / task-exit race.
    hw_freed: AtomicBool,

    // --- Per-task sampling (`perf record -- cmd`) ---
    /// This event samples (`sample_period > 0`): the scheduler hooks arm/disarm
    /// the overflow-IRQ path each slice instead of plain counting.
    is_sampling: bool,
    /// Sampling period (events between overflows); `0` for counting events. The
    /// counter is `preload`ed to overflow after this many events each slice. In
    /// frequency mode this is the per-slice initial estimate the handler adapts.
    sample_period: u32,
    /// `attr.sample_type`. For sampling this is exactly `PERF_SAMPLE_IP`.
    sample_type: u64,
    /// Frequency mode (`attr.freq`): the overflow handler re-derives the period
    /// after each sample to converge on `freq_target` Hz. Fixed period when false.
    freq: bool,
    /// Target sample rate (Hz) for frequency mode; `0` in fixed-period mode.
    freq_target: u32,
    /// Unique event id emitted in `PERF_SAMPLE_ID` / `IDENTIFIER` records (set
    /// once via [`set_sample_id`](Self::set_sample_id) from the `PerfEvent`
    /// wrapper, before any scheduler hook runs); `0` until then.
    sample_id: AtomicU64,
    /// `attr.comm`: this event wants `PERF_RECORD_COMM` side-band records.
    want_comm: bool,
    /// `attr.mmap2`: this event wants `PERF_RECORD_MMAP2` side-band records.
    want_mmap2: bool,
    /// `attr.task`: this event wants `PERF_RECORD_FORK` / `EXIT` side-band records.
    want_task: bool,
    /// `attr.sample_id_all`: side-band records carry the sample-id trailer.
    sample_id_all: bool,

    /// Kernel virtual address of the ring's first page (`perf_event_mmap_page`),
    /// or `0` until `mmap(perf_fd)` runs ([`set_ring`](Self::set_ring)). Read by
    /// [`perf_sched_in`] (IRQ-off hot path) to build the [`SampleSlot`].
    ring_vaddr: AtomicUsize,
    /// Total ring length in bytes (header page + data region); `0` until mapped.
    ring_len: AtomicUsize,
    /// Raw pointer to the live [`IrqNotify`], or null until mapped. Copied into
    /// the [`SampleSlot`] in [`perf_sched_in`] so the overflow handler can wake
    /// the poll worker. Kept alive by the `Arc<IrqNotify>` in [`SamplingAnchors`]
    /// for as long as a slot may reference it (the slot is unregistered before
    /// the `Arc` drops in [`free_hw`]).
    notify_ptr: AtomicUsize,

    /// Strong anchors keeping the ring pages + notify alive, plus the deferred
    /// poll machinery. Set in process context by [`set_ring`](Self::set_ring),
    /// read in process context (`poll`/`register`/`free_hw`); never touched by
    /// the IRQ handler (which reaches the ring/notify through the registered
    /// [`SampleSlot`]'s raw pointers). Behind a [`SpinNoIrq`] so the hot-path
    /// hooks (which only read the atomics above) never block on it.
    anchors: SpinNoIrq<Option<SamplingAnchors>>,

    /// `PERF_EVENT_IOC_SET_OUTPUT` redirect anchor: when this event's samples are
    /// redirected into *another* event's ring, this pins that ring's pages while
    /// we may write into them. `ring_vaddr`/`ring_len` then point at the target
    /// ring and `notify_ptr` stays `0` (the target's poller re-checks
    /// `data_head`; the overflow handler guards the null notify). Set by
    /// [`set_redirect_ring`](Self::set_redirect_ring) instead of [`set_ring`](Self::set_ring).
    redirect_anchor: SpinNoIrq<Option<Arc<dyn Any + Send + Sync>>>,
}

/// Strong references that keep a per-task sampling event's ring + notify alive,
/// plus the `axpoll` readiness machinery the perf fd polls.
///
/// Mirrors the M2 `hw::SamplingState`/`RingState`, but lives on the
/// [`PerTaskCounter`] (the task side) rather than the `HwPerfEvent` (the fd
/// side), because the slot the IRQ handler uses is built from the task side in
/// [`perf_sched_in`]. Set once by [`PerTaskCounter::set_ring`].
struct SamplingAnchors {
    /// The contiguous ring pages. Holding this `Arc` keeps the kernel mapping
    /// (`ring_vaddr`/`ring_len`) live; it drops only in [`free_hw`], after the
    /// slot is unregistered. Also handed out (cloned) by
    /// [`PerTaskCounter::output_ring`] so a redirecting event can pin it.
    ring_pages: Arc<GlobalPage>,
    /// IRQ-safe notification the overflow handler pokes; drained by the worker.
    /// Holding this `Arc` keeps `notify_ptr` valid for the registered slot.
    notify: Arc<IrqNotify>,
    /// Readiness set the perf fd's poller waits on; woken (`IoEvents::IN`) by the
    /// worker after each sample lands in the ring.
    poll_ready: Arc<axpoll::PollSet>,
    /// Liveness flag for the worker; cleared on [`free_hw`] to stop it.
    poll_alive: Arc<AtomicBool>,
}

impl core::fmt::Debug for SamplingAnchors {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The `Arc` payloads are not usefully `Debug`; report only presence.
        f.debug_struct("SamplingAnchors").finish_non_exhaustive()
    }
}

/// Construction parameters for a [`PerTaskCounter`].
///
/// Grouped into one struct (rather than a long positional argument list) so the
/// hardware open path ([`super::hw::perf_event_open_hw_per_task`]) builds it
/// once from the decoded `perf_event_attr`. For a counting event `sample_period`
/// is `0`; for a sampling event it is the fixed `-c` period and `sample_type` is
/// `PERF_SAMPLE_IP`.
pub struct PerTaskConfig {
    /// Reserved programmable PMU counter index.
    pub n: usize,
    /// ARM PMUv3 event number.
    pub event: u16,
    /// `attr.exclude_user`.
    pub exclude_user: bool,
    /// `attr.exclude_kernel`.
    pub exclude_kernel: bool,
    /// `attr.read_format`.
    pub read_format: u64,
    /// Userspace-enabled at open (`attr.disabled == 0`).
    pub enabled: bool,
    /// `attr.enable_on_exec`.
    pub enable_on_exec: bool,
    /// Sampling period (`> 0` ⇒ sampling event); `0` ⇒ counting event. In
    /// frequency mode this is the initial estimate the overflow handler adapts.
    pub sample_period: u32,
    /// `attr.sample_type` (only meaningful when `sample_period > 0`).
    pub sample_type: u64,
    /// Frequency mode (`attr.freq`): the overflow handler adapts the period each
    /// slice toward `target_freq` Hz. Fixed `-c` period when false.
    pub freq: bool,
    /// Target sample rate (Hz) for frequency mode; `0` in fixed-period mode.
    pub target_freq: u32,
    /// `attr.comm`: emit `PERF_RECORD_COMM` side-band records (process name).
    pub want_comm: bool,
    /// `attr.mmap2`: emit `PERF_RECORD_MMAP2` side-band records (executable maps).
    pub want_mmap2: bool,
    /// `attr.task`: emit `PERF_RECORD_FORK` / `EXIT` side-band records.
    pub want_task: bool,
    /// `attr.sample_id_all`: append the sample-id trailer to every side-band record.
    pub sample_id_all: bool,
}

impl PerTaskCounter {
    /// Build a per-task counter around an already-reserved programmable slot `n`.
    ///
    /// The HW counter is *not* programmed here; it is configured + enabled lazily
    /// in [`perf_sched_in`] the next time the target task runs (or immediately
    /// from [`on_exec`] when the target is current during `execve`).
    pub fn new(cfg: PerTaskConfig) -> Self {
        PerTaskCounter {
            n: cfg.n,
            event: cfg.event,
            exclude_user: cfg.exclude_user,
            exclude_kernel: cfg.exclude_kernel,
            read_format: cfg.read_format,
            enable_on_exec: cfg.enable_on_exec,
            enabled: AtomicBool::new(cfg.enabled),
            running: AtomicBool::new(false),
            accumulated: AtomicU64::new(0),
            time_enabled_ns: AtomicU64::new(0),
            time_running_ns: AtomicU64::new(0),
            last_in_ns: AtomicU64::new(0),
            enabled_at_ns: AtomicU64::new(0),
            dead: AtomicBool::new(false),
            hw_freed: AtomicBool::new(false),
            is_sampling: cfg.sample_period > 0,
            sample_period: cfg.sample_period,
            sample_type: cfg.sample_type,
            freq: cfg.freq,
            freq_target: cfg.target_freq,
            sample_id: AtomicU64::new(0),
            want_comm: cfg.want_comm,
            want_mmap2: cfg.want_mmap2,
            want_task: cfg.want_task,
            sample_id_all: cfg.sample_id_all,
            ring_vaddr: AtomicUsize::new(0),
            ring_len: AtomicUsize::new(0),
            notify_ptr: AtomicUsize::new(0),
            anchors: SpinNoIrq::new(None),
            redirect_anchor: SpinNoIrq::new(None),
        }
    }

    /// `attr.read_format` for serializing `read(perf_fd)`.
    pub fn read_format(&self) -> u64 {
        self.read_format
    }

    /// Record the unique event id for `PERF_SAMPLE_ID` / `IDENTIFIER`. Called
    /// once at open (before the scheduler hooks run), so a relaxed store suffices.
    pub fn set_sample_id(&self, id: u64) {
        self.sample_id.store(id, Ordering::Relaxed);
    }

    /// Mark userspace-enabled (`ioctl(ENABLE)` / open-enabled). The target's next
    /// [`perf_sched_in`] programs the counter onto HW.
    pub fn set_enabled(&self) {
        if !self.enabled.swap(true, Ordering::AcqRel) {
            self.enabled_at_ns.store(now_ns(), Ordering::Relaxed);
        }
    }

    /// Mark userspace-disabled (`ioctl(DISABLE)`). The next [`perf_sched_out`]
    /// (or an immediate one if the target is running) stops counting; here we
    /// only clear the intent so future slices do not re-program it.
    pub fn set_disabled(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    /// Zero the accumulated value (`ioctl(RESET)`), leaving timing intact.
    /// Mirrors Linux's `PERF_EVENT_IOC_RESET`, which resets the count only.
    pub fn reset(&self) {
        self.accumulated.store(0, Ordering::Release);
    }

    /// Whether this is a sampling event (`sample_period > 0`).
    pub fn is_sampling(&self) -> bool {
        self.is_sampling
    }

    /// Record the ring buffer + notify/poll machinery for a sampling event.
    ///
    /// Called once, in process context, from
    /// [`super::hw::HwPerfEvent::device_mmap`] after the first `mmap(perf_fd)`.
    /// Stores the strong [`SamplingAnchors`] (pinning the ring pages + notify)
    /// and publishes the ring vaddr/len + notify pointer as atomics so the
    /// IRQ-off [`perf_sched_in`] hot path can build a [`SampleSlot`] without a
    /// lock or allocation.
    ///
    /// The publish order matters: the `notify_ptr` and `ring_*` atoms are stored
    /// with `Release` *after* the anchors are installed, so a sched-in that
    /// observes a non-zero `ring_vaddr` is guaranteed the backing `Arc`s are live.
    pub fn set_ring(
        &self,
        ring_pages: Arc<GlobalPage>,
        ring_vaddr: usize,
        ring_len: usize,
        notify: Arc<IrqNotify>,
        poll_ready: Arc<axpoll::PollSet>,
        poll_alive: Arc<AtomicBool>,
    ) {
        let notify_ptr = Arc::as_ptr(&notify) as usize;
        // Install the strong anchors first; the atomics below gate the hot path.
        *self.anchors.lock() = Some(SamplingAnchors {
            ring_pages,
            notify,
            poll_ready,
            poll_alive,
        });
        // Publish geometry + notify; the non-zero `ring_vaddr` is the readiness
        // signal `perf_sched_in` keys on, so store it last.
        self.notify_ptr.store(notify_ptr, Ordering::Release);
        self.ring_len.store(ring_len, Ordering::Release);
        self.ring_vaddr.store(ring_vaddr, Ordering::Release);
    }

    /// Whether a sampling ring has been mmap'd and is therefore armable.
    ///
    /// Read by [`perf_sched_in`] (to decide whether to arm the slice) and by the
    /// fd's `device_mmap` (to reject a second mapping).
    pub fn ring_mapped(&self) -> bool {
        self.ring_vaddr.load(Ordering::Acquire) != 0
    }

    /// Expose this counter's mmap ring for a `PERF_EVENT_IOC_SET_OUTPUT` redirect
    /// (target side). Returns `(ring_vaddr, ring_len, pages)` with a strong clone
    /// of the ring `Arc` so the redirecting event pins the pages. `None` until the
    /// ring is mmap'd. Only an *owned* ring is shared, not a redirected one.
    pub fn output_ring(&self) -> Option<(usize, usize, Arc<dyn Any + Send + Sync>)> {
        let vaddr = self.ring_vaddr.load(Ordering::Acquire);
        if vaddr == 0 {
            return None;
        }
        let len = self.ring_len.load(Ordering::Acquire);
        let guard = self.anchors.lock();
        let anchors = guard.as_ref()?;
        let pages: Arc<dyn Any + Send + Sync> = anchors.ring_pages.clone();
        Some((vaddr, len, pages))
    }

    /// Point this counter's samples at *another* event's ring
    /// (`PERF_EVENT_IOC_SET_OUTPUT`, source side).
    ///
    /// Pins the target ring via `anchor`, then publishes its geometry so
    /// [`perf_sched_in`] arms this counter to write `PERF_RECORD_SAMPLE`s into it.
    /// `notify_ptr` is left `0`: a redirected source has no poll worker of its own
    /// (the target's poller observes the advancing `data_head`), and the overflow
    /// handler skips a null notify. Publishing `ring_vaddr` last makes the
    /// non-zero value the readiness signal `perf_sched_in` keys on.
    pub fn set_redirect_ring(
        &self,
        ring_vaddr: usize,
        ring_len: usize,
        anchor: Arc<dyn Any + Send + Sync>,
    ) {
        *self.redirect_anchor.lock() = Some(anchor);
        self.notify_ptr.store(0, Ordering::Release);
        self.ring_len.store(ring_len, Ordering::Release);
        self.ring_vaddr.store(ring_vaddr, Ordering::Release);
    }

    /// Readiness for `poll(perf_fd)`: `true` when the ring has unread bytes.
    ///
    /// Reads `data_head`/`data_tail` from the header page; used by the perf fd's
    /// [`super::hw::HwPerfEvent::poll`]. Returns `false` before the ring is
    /// mapped or once it is torn down.
    pub fn ring_has_data(&self) -> bool {
        let vaddr = self.ring_vaddr.load(Ordering::Acquire);
        if vaddr == 0 {
            return false;
        }
        // Keep the pages pinned for the duration of the read.
        let guard = self.anchors.lock();
        if guard.is_none() {
            return false;
        }
        let header = vaddr as *const kbpf_basic::linux_bpf::perf_event_mmap_page;
        // SAFETY: the header page is live (an anchor pins it under `guard`) and
        // was initialized by `device_mmap`; plain `u64` fields read as a hint.
        let (head, tail) = unsafe {
            (
                core::ptr::addr_of!((*header).data_head).read_volatile(),
                core::ptr::addr_of!((*header).data_tail).read_volatile(),
            )
        };
        head != tail
    }

    /// Register the perf fd poller's waker on the sampling readiness set.
    ///
    /// Mirrors the M2 `register`: the notify worker wakes this `PollSet` after
    /// each sample. No-op if the ring has not been mmap'd yet (no `PollSet`).
    pub fn register_poll(&self, context: &mut core::task::Context<'_>) {
        let guard = self.anchors.lock();
        if let Some(anchors) = guard.as_ref() {
            // SAFETY: `poll_ready` is a live `PollSet`; registering a waker on it
            // is the documented `axpoll` contract (mirrors the M2 path).
            unsafe {
                anchors
                    .poll_ready
                    .register(context.waker(), axpoll::IoEvents::IN)
            };
        }
    }
}

/// Monotonic time source shared with the system-wide path.
#[inline]
fn now_ns() -> u64 {
    ax_runtime::hal::time::monotonic_time_nanos()
}

/// Attach `ptc` to `thr` and arm the scheduler hooks.
///
/// Called from [`hw::perf_event_open_hw`] in `pid > 0` mode. Bumping
/// [`PERF_TASK_ACTIVE`] *after* the push ensures the hooks, once they start
/// running, always find the counter in the list.
pub fn attach(thr: &Thread, ptc: Arc<PerTaskCounter>) {
    thr.perf_counters.lock().push(ptc);
    PERF_TASK_ACTIVE.fetch_add(1, Ordering::AcqRel);
}

/// Scheduler hook: the given thread is about to start running on this CPU.
///
/// Programs every enabled, not-yet-running, live per-task counter onto HW and
/// starts it. `configure` resets the counter to 0, so the slice delta will equal
/// `counter::read(n)` at the matching [`perf_sched_out`].
///
/// For a *sampling* counter (`is_sampling`) whose ring is mapped, it instead arms
/// the M2 overflow-IRQ path for this slice: `configure`, `preload` to overflow
/// after `sample_period` events, register a [`SampleSlot`] pointing at the ptc's
/// ring + notify, `enable_irq`, then `enable`. So overflows fire `PERF_RECORD_SAMPLE`
/// into the task's ring only while the task runs. (If the ring is not mapped yet,
/// the slice is skipped — `perf` always mmaps before enable, so this is a rare race.)
///
/// Runs with IRQs disabled inside `switch_to`: [`SpinNoIrq`](ax_sync::spin::SpinNoIrq)
/// + atomics + sysreg writes only, no allocation. `sampling::register` nests a
/// further local-IRQ-off section, which is fine.
pub fn perf_sched_in(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let counters = thr.perf_counters.lock();
    if counters.is_empty() {
        return;
    }
    let now = now_ns();
    for ptc in counters.iter() {
        if ptc.dead.load(Ordering::Acquire) {
            continue;
        }
        if !ptc.enabled.load(Ordering::Acquire) {
            continue;
        }
        if ptc.running.load(Ordering::Acquire) {
            continue;
        }
        if ptc.is_sampling {
            // Sampling: only arm if the ring is mmap'd (else skip this slice).
            if !ptc.ring_mapped() {
                continue;
            }
            // Make sure the PMU overflow handler is installed + the PPI enabled.
            sampling::ensure_pmu_irq_registered();
            // configure() programs event + EL filter AND resets the counter to 0.
            ax_cpu::pmu::counter::configure(ptc.n, ptc.event, ptc.exclude_user, ptc.exclude_kernel);
            // Overflow after `sample_period` events.
            ax_cpu::pmu::counter::preload(ptc.n, ptc.sample_period);
            // Publish the slot the overflow handler writes through. The ring +
            // notify pointers were set by `device_mmap`; they are alloc-free
            // reads here.
            sampling::register(
                ptc.n,
                SampleSlot {
                    ring_vaddr: ptc.ring_vaddr.load(Ordering::Acquire),
                    ring_len: ptc.ring_len.load(Ordering::Acquire),
                    period: ptc.sample_period,
                    sample_type: ptc.sample_type,
                    id: ptc.sample_id.load(Ordering::Relaxed),
                    notify: ptc.notify_ptr.load(Ordering::Acquire) as *const (),
                    // Frequency mode adapts the period within each slice; the slot
                    // starts at the initial estimate with no prior timestamp.
                    freq: ptc.freq,
                    target_freq: ptc.freq_target,
                    last_time: 0,
                },
            );
            // Arm the per-counter overflow interrupt, then start counting.
            ax_cpu::pmu::overflow::enable_irq(ptc.n);
            ax_cpu::pmu::counter::enable(ptc.n);
        } else {
            // Counting: configure() programs event + EL filter AND resets to 0.
            ax_cpu::pmu::counter::configure(ptc.n, ptc.event, ptc.exclude_user, ptc.exclude_kernel);
            ax_cpu::pmu::counter::enable(ptc.n);
        }
        ptc.last_in_ns.store(now, Ordering::Release);
        ptc.running.store(true, Ordering::Release);
    }
}

/// Scheduler hook: the given thread is about to stop running on this CPU.
///
/// For a counting counter, reads the current slice delta, folds it into the
/// accumulator, stops the counter, and accrues the slice's wall time.
///
/// For a *sampling* counter, disarms the M2 overflow-IRQ path for this slice:
/// stop the counter (it can no longer overflow), `disable_irq`, then `unregister`
/// the [`SampleSlot`]. After this, an overflow on counter `n` while some *other*
/// task runs cannot fire a sample into this task's ring — that is what attributes
/// samples to the task. (Sampling events carry no read-back value, so no delta is
/// accumulated; only wall time is accrued.)
///
/// Same hot-path constraints as [`perf_sched_in`].
pub fn perf_sched_out(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let counters = thr.perf_counters.lock();
    if counters.is_empty() {
        return;
    }
    let now = now_ns();
    for ptc in counters.iter() {
        if ptc.dead.load(Ordering::Acquire) {
            continue;
        }
        if !ptc.running.load(Ordering::Acquire) {
            continue;
        }
        if ptc.is_sampling {
            // Disarm in the M2 teardown order: stop the counter, mask the IRQ,
            // then clear the slot so a later overflow on `n` (a different task)
            // cannot reach this ring/notify.
            ax_cpu::pmu::counter::disable(ptc.n);
            ax_cpu::pmu::overflow::disable_irq(ptc.n);
            sampling::unregister(ptc.n);
        } else {
            // Slice started at 0 (configure reset it), so delta is the raw read.
            let delta = ax_cpu::pmu::counter::read(ptc.n);
            ptc.accumulated.fetch_add(delta, Ordering::AcqRel);
            ax_cpu::pmu::counter::disable(ptc.n);
        }
        ptc.running.store(false, Ordering::Release);

        let last_in = ptc.last_in_ns.load(Ordering::Acquire);
        let dt = now.saturating_sub(last_in);
        ptc.time_enabled_ns.fetch_add(dt, Ordering::AcqRel);
        ptc.time_running_ns.fetch_add(dt, Ordering::AcqRel);
    }
}

/// Exec hook: the given (current) thread has committed a new image in `execve`.
///
/// Flips any `enable_on_exec` counter to `enabled` and — because the task is the
/// running task right now — programs it onto HW immediately via
/// [`perf_sched_in`]. The `running` flag inside `perf_sched_in` prevents
/// double-programming an already-enabled counter.
pub fn on_exec(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let now = now_ns();
    {
        let counters = thr.perf_counters.lock();
        for ptc in counters.iter() {
            if ptc.dead.load(Ordering::Acquire) {
                continue;
            }
            if ptc.enable_on_exec && !ptc.enabled.swap(true, Ordering::AcqRel) {
                ptc.enabled_at_ns.store(now, Ordering::Release);
            }
        }
    }
    // Program the now-enabled counters onto HW for the current task. Takes the
    // list lock itself, so it is released above first.
    perf_sched_in(thr);
}

/// Build a side-band write target for `ptc` if it has a mapped ring and requested
/// any side-band record (`attr.comm`/`mmap2`/`task`); else `None`.
fn sideband_target(ptc: &PerTaskCounter, pid: u32, tid: u32) -> Option<SidebandTarget> {
    let ring_vaddr = ptc.ring_vaddr.load(Ordering::Acquire);
    if ring_vaddr == 0 || !(ptc.want_comm || ptc.want_mmap2 || ptc.want_task) {
        return None;
    }
    Some(SidebandTarget {
        ring_vaddr,
        ring_len: ptc.ring_len.load(Ordering::Acquire),
        sample_type: ptc.sample_type,
        sample_id_all: ptc.sample_id_all,
        id: ptc.sample_id.load(Ordering::Relaxed),
        pid,
        tid,
    })
}

/// Snapshot the executable file-backed mappings of `thr`'s address space as
/// `MMAP2` records. Collected under the aspace lock and returned owned, so the
/// caller writes the ring (which masks IRQs) without holding that lock.
fn collect_exec_maps(thr: &Thread) -> Vec<Mmap2Info> {
    let aspace = thr.proc_data.aspace();
    let mm = aspace.lock();
    let mut maps = Vec::new();
    for area in mm.areas() {
        let flags = area.flags();
        if !flags.contains(MappingFlags::EXECUTE) {
            continue;
        }
        // Only file-backed areas can be symbolized (perf opens the file). An
        // anonymous executable mapping (JIT) has no file and is skipped.
        let Ok(fi) = area.backend().file_info() else {
            continue;
        };
        let mut prot = 0u32;
        if flags.contains(MappingFlags::READ) {
            prot |= PROT_READ;
        }
        if flags.contains(MappingFlags::WRITE) {
            prot |= PROT_WRITE;
        }
        prot |= PROT_EXEC;
        maps.push(Mmap2Info {
            addr: area.start().as_usize() as u64,
            len: (area.end().as_usize() - area.start().as_usize()) as u64,
            pgoff: fi.offset.unwrap_or(0),
            maj: 0,
            min: 0,
            ino: fi.inode.unwrap_or(0),
            prot,
            flags: if fi.shared { MAP_SHARED } else { MAP_PRIVATE },
            filename: fi.path,
        });
    }
    maps
}

/// Exec side-band hook: emit `PERF_RECORD_COMM` (new process name) and one
/// `PERF_RECORD_MMAP2` per executable mapping (the exec image + the dynamic
/// loader), into every per-task event monitoring this thread that asked for them.
///
/// Called from `do_execve` right after [`on_exec`], in the exec'd task's context
/// (so [`current`] is this task and `thr`'s address space is the new image).
/// `perf record` mmaps the ring before releasing the child, so the ring exists.
pub fn on_exec_sideband(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let pid = thr.proc_data.proc.pid() as u32;
    let tid = thr.tid() as u32;

    /// A target plus which record kinds it wants (so the COMM/MMAP2 loops below
    /// can each skip non-subscribers without re-walking the counter list).
    struct WantTarget {
        target: SidebandTarget,
        comm: bool,
        mmap2: bool,
    }
    // Snapshot targets, then drop the counter lock before any ring write.
    let targets: Vec<WantTarget> = {
        let counters = thr.perf_counters.lock();
        counters
            .iter()
            .filter_map(|ptc| {
                sideband_target(ptc, pid, tid).map(|target| WantTarget {
                    target,
                    comm: ptc.want_comm,
                    mmap2: ptc.want_mmap2,
                })
            })
            .collect()
    };
    if targets.is_empty() {
        return;
    }

    // COMM: the new process name (this hook runs in the exec'd task's context).
    let curr = ax_task::current();
    let name = curr.name();
    for wt in &targets {
        if wt.comm {
            sideband::emit_comm(&wt.target, &name, true);
        }
    }

    // MMAP2: one per executable file-backed mapping of the new image.
    if targets.iter().any(|wt| wt.mmap2) {
        let maps = collect_exec_maps(thr);
        for wt in &targets {
            if wt.mmap2 {
                for m in &maps {
                    sideband::emit_mmap2(&wt.target, m);
                }
            }
        }
    }
}

/// mmap side-band hook: emit a `PERF_RECORD_MMAP2` for a newly-mapped executable
/// file region of the current task (a shared library the dynamic loader just
/// `mmap`ed), into every monitoring per-task event that asked for mmap records.
///
/// Called from `sys_mmap` after a successful executable, file-backed mapping.
pub fn on_mmap_sideband(
    thr: &Thread,
    addr: usize,
    len: usize,
    pgoff: usize,
    prot: u32,
    shared: bool,
    filename: &str,
) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let pid = thr.proc_data.proc.pid() as u32;
    let tid = thr.tid() as u32;
    let targets: Vec<SidebandTarget> = {
        let counters = thr.perf_counters.lock();
        counters
            .iter()
            .filter(|ptc| ptc.want_mmap2)
            .filter_map(|ptc| sideband_target(ptc, pid, tid))
            .collect()
    };
    if targets.is_empty() {
        return;
    }
    let m = Mmap2Info {
        addr: addr as u64,
        len: len as u64,
        pgoff: pgoff as u64,
        maj: 0,
        min: 0,
        ino: 0,
        prot,
        flags: if shared { MAP_SHARED } else { MAP_PRIVATE },
        filename: String::from(filename),
    };
    for t in &targets {
        sideband::emit_mmap2(t, &m);
    }
}

/// Clone side-band hook: emit a `PERF_RECORD_FORK` describing the new child into
/// every per-task event monitoring the **parent** that requested `attr.task`.
///
/// Called from `do_clone` in the parent's (forking task's) context, after the
/// child task is spawned. The record's body describes the child (`child_pid` /
/// `child_tid`) with the parent as `ppid`/`ptid`; its `sample_id_all` trailer is
/// the parent's id (the event's monitored task), so `t.pid`/`t.tid` = parent.
pub fn on_clone_sideband(parent_thr: &Thread, child_pid: u32, child_tid: u32) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let ppid = parent_thr.proc_data.proc.pid();
    let ptid = parent_thr.tid();
    // Snapshot want_task targets, then drop the counter lock before any ring write.
    let targets: Vec<SidebandTarget> = {
        let counters = parent_thr.perf_counters.lock();
        counters
            .iter()
            .filter(|ptc| ptc.want_task)
            .filter_map(|ptc| sideband_target(ptc, ppid, ptid))
            .collect()
    };
    for t in &targets {
        sideband::emit_fork(t, child_pid, ppid, child_tid, ptid);
    }
}

/// Task-exit hook: emit `PERF_RECORD_EXIT` (for `attr.task` events) then free
/// every HW counter the exiting thread still holds.
///
/// The EXIT record must be written *before* [`free_hw`] zeroes the ring geometry,
/// so it is emitted per counter just before that counter is freed; the exiting
/// thread is the subject and its parent (if any) supplies `ppid`/`ptid`.
///
/// `free_hw` is idempotent per counter; safe even if the perf fd is still open
/// (its `Drop` will call `free_hw` again and find it already freed).
pub fn on_task_exit(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let pid = thr.proc_data.proc.pid();
    let tid = thr.tid();
    let (ppid, ptid) = match thr.proc_data.proc.parent() {
        // The parent process's tgid; its main-thread tid equals that tgid.
        Some(p) => {
            let ppid = p.pid();
            (ppid, ppid)
        }
        None => (0, 0),
    };
    let counters = thr.perf_counters.lock();
    for ptc in counters.iter() {
        if ptc.want_task
            && let Some(t) = sideband_target(ptc, pid, tid)
        {
            sideband::emit_exit(&t, pid, ppid, tid, ptid);
        }
        free_hw(ptc);
    }
}

/// Release the HW counter backing `ptc` and tear down its bookkeeping, once.
///
/// Idempotent: the `hw_freed` compare-exchange ensures only the first caller
/// (either [`HwPerfEvent::drop`] on the fd side or [`on_task_exit`] on the task
/// side) does the work. It stops the counter if it was running, returns the
/// slot to the M1 allocator, decrements [`PERF_TASK_ACTIVE`], and marks the
/// counter `dead` so the scheduler hooks skip it forever after.
///
/// For a *sampling* counter that is currently armed, the overflow-IRQ path is
/// torn down in the UAF-safe order before the slot/ring `Arc`s drop: stop the
/// counter, mask the IRQ, then `unregister` the [`SampleSlot`] — so the overflow
/// handler can no longer reach the ring or `notify` pointer. Only after that are
/// the [`SamplingAnchors`] (the `Arc<GlobalPage>` ring + `Arc<IrqNotify>`)
/// dropped and the worker stopped.
pub fn free_hw(ptc: &PerTaskCounter) {
    if ptc
        .hw_freed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // Already freed by the other side; nothing to do.
        return;
    }
    // Mark dead before touching HW so a concurrent hook (single-core: not truly
    // concurrent, but cheap insurance) observes the teardown.
    ptc.dead.store(true, Ordering::Release);
    let was_running = ptc.running.swap(false, Ordering::AcqRel);
    if ptc.is_sampling {
        if was_running {
            // Strict teardown: stop the counter, mask the IRQ, clear the slot.
            // After `unregister` the handler can no longer reference this ring.
            ax_cpu::pmu::counter::disable(ptc.n);
            ax_cpu::pmu::overflow::disable_irq(ptc.n);
            sampling::unregister(ptc.n);
        }
        // Stop the deferred worker and drop the ring/notify anchors. This must
        // run AFTER the slot is unregistered above (the overflow handler keeps
        // the `notify`/ring pointers live only while a slot references them).
        // `Acquire` here pairs with the `Release` in `set_ring`. The ring pages
        // (`Arc<GlobalPage>`) drop here too — but the VMA holds its own strong
        // ref via the mmap retainer, so user memory stays mapped until munmap.
        let anchors = ptc.anchors.lock().take();
        if let Some(anchors) = anchors {
            anchors.poll_alive.store(false, Ordering::Release);
            anchors.notify.notify();
        }
        // Drop a SET_OUTPUT redirect anchor too, if this event was redirected
        // into another's ring (its own `anchors` is then `None`). Safe after the
        // slot is unregistered: the handler can no longer reach the target ring.
        *ptc.redirect_anchor.lock() = None;
        // Zero the published geometry so no later hook can re-arm a stale ring.
        ptc.ring_vaddr.store(0, Ordering::Release);
        ptc.notify_ptr.store(0, Ordering::Release);
    } else if was_running {
        ax_cpu::pmu::counter::disable(ptc.n);
    }
    hw::free_programmable_counter(ptc.n);
    PERF_TASK_ACTIVE.fetch_sub(1, Ordering::AcqRel);
}

/// Read back `(value, time_enabled, time_running)` for `read(perf_fd)`.
///
/// `value` is the accumulated delta plus the live slice if the counter is
/// currently running. For `perf stat -- cmd` the child has already exited by the
/// time the parent reads, so `running == false` and `accumulated` is final.
pub fn read_values(ptc: &PerTaskCounter) -> (u64, u64, u64) {
    let mut value = ptc.accumulated.load(Ordering::Acquire);
    let mut time_enabled = ptc.time_enabled_ns.load(Ordering::Acquire);
    let mut time_running = ptc.time_running_ns.load(Ordering::Acquire);
    if ptc.running.load(Ordering::Acquire) {
        // Live slice: add the in-progress count and elapsed time. This is a
        // cross-task read of HW counter state; on single-core M2 the target is
        // not running concurrently with this reader, so the read is a coherent
        // (if slightly stale) snapshot.
        value += ax_cpu::pmu::counter::read(ptc.n);
        let dt = now_ns().saturating_sub(ptc.last_in_ns.load(Ordering::Acquire));
        time_enabled += dt;
        time_running += dt;
    }
    (value, time_enabled, time_running)
}
