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

use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use ax_alloc::GlobalPage;
use ax_kspin::SpinNoIrq;
use ax_runtime::hal::paging::MappingFlags;
use ax_task::IrqNotify;

use super::{
    sampling::{self, SampleSlot},
    sideband::{self, Mmap2Info, SidebandTarget},
};
use crate::task::{AsThread, Thread};

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

/// [`PerTaskCounter::slot`] sentinel: no hardware counter is held this slice.
///
/// A programmable counter is reserved from the *running* core's per-CPU pool at
/// [`perf_sched_in`] and released at [`perf_sched_out`], so between slices (and
/// before the first run) the counter holds no slot.
const NO_SLOT: usize = usize::MAX;

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
    /// Programmable PMU counter index currently held on the running core, or
    /// [`NO_SLOT`] when this counter holds no hardware counter (not running this
    /// slice). Reserved at [`perf_sched_in`] from the local per-CPU pool,
    /// released at [`perf_sched_out`]. Per-task events never use the dedicated
    /// cycle counter.
    slot: AtomicUsize,
    /// Logical CPU id the counter was last scheduled onto, for the cross-core
    /// [`read_values`] guard (`PMEVCNTRn` is per-PE banked, so the live counter
    /// can only be read on the core the target runs on). `usize::MAX` until the
    /// first [`perf_sched_in`].
    last_cpu: AtomicUsize,
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
    /// The owning task is the running task on its core right now (between
    /// [`perf_sched_in`] and [`perf_sched_out`]), regardless of whether this
    /// counter holds a HW slot. Drives the `time_enabled` accrual: an
    /// over-subscribed counter that is on-CPU but holds no slot still accrues
    /// enabled time (so `perf` scales `value * enabled / running`).
    on_cpu: AtomicBool,
    /// The event holds a HW counter right now (it is `on_cpu` AND was allocated
    /// a slot this slice / rotation window). Cleared when rotated out.
    running: AtomicBool,
    /// Sum of completed-slice deltas (raw event count).
    accumulated: AtomicU64,
    /// Samples the ring dropped because it was full (`PERF_FORMAT_LOST`). Bumped
    /// by the overflow handler through the per-task [`SampleSlot`]'s `lost` pointer
    /// at this field; read back by `read(perf_fd)`.
    lost: AtomicU64,
    /// How many of [`lost`](Self::lost) have been reported in-band as
    /// `PERF_RECORD_LOST` records (the handler's `lost_reported` pointer).
    lost_reported: AtomicU64,
    /// Accumulated enabled time across past windows (ns).
    time_enabled_ns: AtomicU64,
    /// Accumulated running time across past windows (ns). Strictly `<=
    /// time_enabled_ns` once multiplexing rotates this counter off HW.
    time_running_ns: AtomicU64,
    /// Monotonic ns timestamp the owning task last became on-CPU with this event
    /// enabled (set in [`perf_sched_in`]); the base for the `time_enabled` slice.
    last_in_ns: AtomicU64,
    /// Monotonic ns timestamp this counter last started holding a HW slot (set in
    /// [`perf_sched_in`] when armed, or by a rotation admit); `0` when not
    /// holding. The base for the `time_running` slice.
    run_since_ns: AtomicU64,
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
    /// Persistent running count for `PERF_SAMPLE_READ`, advanced by the sampling
    /// `period` on each overflow. The per-slice [`SampleSlot`] is rebuilt every
    /// [`arm_slice`] (its `read_value` starts from here), and the overflow handler
    /// mirrors the advanced value back here through the slot's `read_value_sink`,
    /// so a per-task event's reported count stays monotonic across slices — the
    /// leader row of a group-leader read included. `0` until the first sample.
    sample_read_value: AtomicU64,
    /// `attr.comm`: this event wants `PERF_RECORD_COMM` side-band records.
    want_comm: bool,
    /// `attr.mmap2`: this event wants `PERF_RECORD_MMAP2` side-band records.
    want_mmap2: bool,
    /// `attr.task`: this event wants `PERF_RECORD_FORK` / `EXIT` side-band records.
    want_task: bool,
    /// `attr.sample_id_all`: side-band records carry the sample-id trailer.
    sample_id_all: bool,
    /// `attr.inherit`: clone this event onto `fork`/`clone` children (writing into
    /// the same ring) so `perf record` follows them. Driven by [`on_clone_inherit`].
    inherit: bool,
    /// Which clusters this event may run on (big.LITTLE). Generic events use
    /// [`ClusterMask::ALL`]; an event opened against a cluster's sysfs PMU is
    /// restricted to that cluster — [`perf_sched_in`] skips arming it on a
    /// non-matching core (its `time_enabled` still accrues, so `perf` scales).
    valid_clusters: super::percpu::ClusterMask,

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

    /// Group members this per-task counter LEADS, for group-leader sampling
    /// (`PERF_SAMPLE_READ | PERF_FORMAT_GROUP`). Only a **sampling** leader
    /// populates this: each counting member opened with `group_fd` = this event's
    /// fd is recorded here (weakly, to avoid an `Arc` cycle and so a closed member
    /// is skipped). At each slice-arm the leader snapshots the live members' atomic
    /// pointers into its [`SampleSlot`] so the overflow handler can emit the whole
    /// group without walking this list from hard-IRQ. Written once per member at
    /// open (process context via [`link_group_member`]); read in [`arm_slice`]
    /// (IRQ-off). Empty for a counting leader (whose group read is served in
    /// process context by the file-layer group read) and for a non-leader.
    group_members: SpinNoIrq<Vec<Weak<PerTaskCounter>>>,
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
    /// `attr.inherit`: clone this event onto `fork`/`clone` children.
    pub inherit: bool,
    /// Which clusters this event may run on (from the PMU type it was opened
    /// against). [`super::percpu::ClusterMask::ALL`] for generic events.
    pub valid_clusters: super::percpu::ClusterMask,
}

impl PerTaskCounter {
    /// Build a per-task counter around an already-reserved programmable slot `n`.
    ///
    /// The HW counter is *not* programmed here; it is configured + enabled lazily
    /// in [`perf_sched_in`] the next time the target task runs (or immediately
    /// from [`on_exec`] when the target is current during `execve`).
    pub fn new(cfg: PerTaskConfig) -> Self {
        PerTaskCounter {
            slot: AtomicUsize::new(NO_SLOT),
            last_cpu: AtomicUsize::new(usize::MAX),
            event: cfg.event,
            exclude_user: cfg.exclude_user,
            exclude_kernel: cfg.exclude_kernel,
            read_format: cfg.read_format,
            enable_on_exec: cfg.enable_on_exec,
            enabled: AtomicBool::new(cfg.enabled),
            on_cpu: AtomicBool::new(false),
            running: AtomicBool::new(false),
            accumulated: AtomicU64::new(0),
            time_enabled_ns: AtomicU64::new(0),
            time_running_ns: AtomicU64::new(0),
            last_in_ns: AtomicU64::new(0),
            run_since_ns: AtomicU64::new(0),
            enabled_at_ns: AtomicU64::new(0),
            lost: AtomicU64::new(0),
            lost_reported: AtomicU64::new(0),
            dead: AtomicBool::new(false),
            hw_freed: AtomicBool::new(false),
            is_sampling: cfg.sample_period > 0,
            sample_period: cfg.sample_period,
            sample_type: cfg.sample_type,
            freq: cfg.freq,
            freq_target: cfg.target_freq,
            sample_id: AtomicU64::new(0),
            sample_read_value: AtomicU64::new(0),
            want_comm: cfg.want_comm,
            want_mmap2: cfg.want_mmap2,
            want_task: cfg.want_task,
            sample_id_all: cfg.sample_id_all,
            inherit: cfg.inherit,
            valid_clusters: cfg.valid_clusters,
            ring_vaddr: AtomicUsize::new(0),
            ring_len: AtomicUsize::new(0),
            notify_ptr: AtomicUsize::new(0),
            anchors: SpinNoIrq::new(None),
            redirect_anchor: SpinNoIrq::new(None),
            group_members: SpinNoIrq::new(Vec::new()),
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
    /// Mirrors Linux's `PERF_EVENT_IOC_RESET`, which resets the count only. Also
    /// zeroes the `PERF_SAMPLE_READ` running count so a reset sampling event
    /// restarts its reported value from zero.
    pub fn reset(&self) {
        self.accumulated.store(0, Ordering::Release);
        self.sample_read_value.store(0, Ordering::Release);
    }

    /// Whether this is a sampling event (`sample_period > 0`).
    pub fn is_sampling(&self) -> bool {
        self.is_sampling
    }

    /// Snapshot this (sampling leader) counter's live group members into the fixed
    /// [`SampleSlot`] member table, returning the populated count (`<=
    /// MAX_GROUP_MEMBERS`). Upgrades each member `Weak` — a lock-free atomic op,
    /// safe from the IRQ-off [`arm_slice`] — to capture stable raw pointers to the
    /// member's `accumulated` / `slot` / `last_cpu` / `running` atomics plus its
    /// `sample_id`. Those pointers outlive the temporary `Arc` dropped here: the
    /// member ptc is pinned by its `Thread`'s counter list until task exit, which
    /// outlives this slot's registration (mirrors the notify/lost pointer
    /// discipline). Only entries `[0, n)` are written; the caller pre-fills the
    /// table with [`GroupMember::EMPTY`](sampling::GroupMember::EMPTY).
    fn snapshot_group_members(
        &self,
        out: &mut [sampling::GroupMember; sampling::MAX_GROUP_MEMBERS],
    ) -> u8 {
        let members = self.group_members.lock();
        let mut n = 0usize;
        for weak in members.iter() {
            if n >= sampling::MAX_GROUP_MEMBERS {
                break;
            }
            let Some(m) = weak.upgrade() else {
                continue;
            };
            out[n] = sampling::GroupMember {
                id: m.sample_id.load(Ordering::Relaxed),
                accumulated: &m.accumulated as *const AtomicU64 as *const (),
                slot: &m.slot as *const AtomicUsize as *const (),
                last_cpu: &m.last_cpu as *const AtomicUsize as *const (),
                running: &m.running as *const AtomicBool as *const (),
            };
            n += 1;
        }
        n as u8
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

    /// Expose this counter's ring for an `attr.inherit` child to redirect into.
    ///
    /// Unlike [`output_ring`](Self::output_ring) this also works for a counter
    /// that is *itself* redirected (an inherited child of an inherited child):
    /// it hands back the redirect anchor so all descendants point at the one
    /// root ring. Returns `(ring_vaddr, ring_len, anchor)`, or `None` before the
    /// ring is mapped.
    pub fn inherit_ring(&self) -> Option<(usize, usize, Arc<dyn Any + Send + Sync>)> {
        let vaddr = self.ring_vaddr.load(Ordering::Acquire);
        if vaddr == 0 {
            return None;
        }
        let len = self.ring_len.load(Ordering::Acquire);
        // Owned ring: pin its pages directly.
        if let Some(anchors) = self.anchors.lock().as_ref() {
            let pages: Arc<dyn Any + Send + Sync> = anchors.ring_pages.clone();
            return Some((vaddr, len, pages));
        }
        // Redirected ring (this counter is itself an inherited / SET_OUTPUT
        // source): re-share the same anchor so the grandchild pins the root ring.
        let anchor = self.redirect_anchor.lock().as_ref()?.clone();
        Some((vaddr, len, anchor))
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
/// Called from [`super::hw::perf_event_open_hw`] in `pid > 0` mode. Bumping
/// [`PERF_TASK_ACTIVE`] *after* the push ensures the hooks, once they start
/// running, always find the counter in the list.
pub fn attach(thr: &Thread, ptc: Arc<PerTaskCounter>) {
    thr.perf_counters.lock().push(ptc);
    PERF_TASK_ACTIVE.fetch_add(1, Ordering::AcqRel);
}

/// Record `member` as a group member led by the **sampling** leader `leader`, for
/// group-leader sampling (`PERF_SAMPLE_READ | PERF_FORMAT_GROUP`).
///
/// Called at open (process context) from the file-layer group wiring in
/// [`super::perf_event_open`] when both the leader and the member are per-task
/// hardware counters. A *counting* leader needs no link — its group read is
/// served entirely in process context (the file-layer group read) — so this is a
/// no-op unless `leader` samples. The link is stored weakly; the member ptc stays
/// alive through its own `Thread`'s counter list. Members past
/// [`sampling::MAX_GROUP_MEMBERS`] exceed the co-schedulable PMU width and are
/// dropped from the sampled read (warned), keeping the on-IRQ member table
/// bounded.
pub fn link_group_member(leader: &Arc<PerTaskCounter>, member: &Arc<PerTaskCounter>) {
    if !leader.is_sampling {
        return;
    }
    let mut members = leader.group_members.lock();
    if members.len() >= sampling::MAX_GROUP_MEMBERS {
        warn!(
            "perf group-leader sampling: dropping member beyond {} (PMU width); the sampled group \
             read will omit it",
            sampling::MAX_GROUP_MEMBERS
        );
        return;
    }
    members.push(Arc::downgrade(member));
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
/// Arm `ptc` onto programmable counter `n` on the current core: configure
/// (counting) or configure + preload + register a [`SampleSlot`] (sampling),
/// enable, and mark it running from `now`. IRQ-off, alloc-free. Shared by
/// [`perf_sched_in`] and [`perf_rotate_current`].
///
/// `owner_pid`/`owner_tid` are the monitored thread's real userspace tgid/tid
/// (both callers hold the [`Thread`]); they are stamped into the sampling
/// [`SampleSlot`] so the overflow handler attributes each sample to the
/// monitored task rather than to `current()`, which can differ when the overflow
/// IRQ is serviced after a context switch away from the task.
fn arm_slice(ptc: &PerTaskCounter, n: usize, now: u64, owner_pid: u32, owner_tid: u32) {
    if ptc.is_sampling {
        // Make sure the PMU overflow handler is installed + the PPI enabled.
        sampling::ensure_pmu_irq_registered();
        ax_cpu::pmu::counter::configure(n, ptc.event, ptc.exclude_user, ptc.exclude_kernel);
        ax_cpu::pmu::counter::preload(n, ptc.sample_period);
        // Group-leader sampling: snapshot this leader's live counting members'
        // atomic pointers so the overflow handler can emit the whole group's
        // values. A non-group event (`read_format` without `PERF_FORMAT_GROUP`)
        // skips the lock and leaves `n_members == 0` (single-event read).
        let mut members = [sampling::GroupMember::EMPTY; sampling::MAX_GROUP_MEMBERS];
        let n_members = if ptc.read_format & sampling::READ_FORMAT_GROUP != 0 {
            ptc.snapshot_group_members(&mut members)
        } else {
            0
        };
        sampling::register(
            n,
            SampleSlot {
                ring_vaddr: ptc.ring_vaddr.load(Ordering::Acquire),
                ring_len: ptc.ring_len.load(Ordering::Acquire),
                period: ptc.sample_period,
                sample_type: ptc.sample_type,
                id: ptc.sample_id.load(Ordering::Relaxed),
                notify: ptc.notify_ptr.load(Ordering::Acquire) as *const (),
                freq: ptc.freq,
                target_freq: ptc.freq_target,
                last_time: 0,
                lost: &ptc.lost as *const AtomicU64 as *const (),
                lost_reported: &ptc.lost_reported as *const AtomicU64 as *const (),
                // Per-task event: attribute samples to the monitored thread even
                // if the overflow IRQ lands after a switch away from it.
                owner_ids: Some((owner_pid, owner_tid)),
                read_format: ptc.read_format,
                // Resume the running count from the persisted total (per-task slots
                // are rebuilt each slice) and give the handler the sink to mirror
                // it back into, so the reported value stays monotonic across slices.
                read_value: ptc.sample_read_value.load(Ordering::Relaxed),
                read_value_sink: &ptc.sample_read_value as *const AtomicU64 as *const (),
                members,
                n_members,
            },
        );
        ax_cpu::pmu::overflow::enable_irq(n);
        ax_cpu::pmu::counter::enable(n);
    } else {
        // configure() programs event + EL filter AND resets the counter to 0.
        ax_cpu::pmu::counter::configure(n, ptc.event, ptc.exclude_user, ptc.exclude_kernel);
        ax_cpu::pmu::counter::enable(n);
    }
    ptc.slot.store(n, Ordering::Release);
    ptc.run_since_ns.store(now, Ordering::Release);
    ptc.running.store(true, Ordering::Release);
}

/// Disarm `ptc`'s currently-held slot on the current core: fold the counting
/// delta into `accumulated` (when `accumulate`) or disarm the sampling slot,
/// accrue the `time_running` sub-slice, free the programmable slot, and clear
/// `running`. No-op if no slot is held. IRQ-off, alloc-free. Shared by
/// [`perf_sched_out`], [`perf_rotate_current`], and [`teardown_slice_local`].
fn disarm_slice(ptc: &PerTaskCounter, now: u64, accumulate: bool) {
    let n = ptc.slot.load(Ordering::Acquire);
    if n == NO_SLOT {
        ptc.running.store(false, Ordering::Release);
        return;
    }
    if ptc.is_sampling {
        // M2 teardown order: stop counter, mask IRQ, then clear the slot.
        ax_cpu::pmu::counter::disable(n);
        ax_cpu::pmu::overflow::disable_irq(n);
        sampling::unregister(n);
    } else {
        // Slice/window started at 0 (configure reset it), so delta is the raw read.
        let delta = ax_cpu::pmu::counter::read(n);
        if accumulate {
            ptc.accumulated.fetch_add(delta, Ordering::AcqRel);
        }
        ax_cpu::pmu::counter::disable(n);
    }
    // Accrue the `time_running` sub-slice this counter actually held the slot.
    let run_since = ptc.run_since_ns.swap(0, Ordering::AcqRel);
    if run_since != 0 {
        ptc.time_running_ns
            .fetch_add(now.saturating_sub(run_since), Ordering::AcqRel);
    }
    super::percpu::free_programmable_counter(n);
    ptc.slot.store(NO_SLOT, Ordering::Release);
    ptc.running.store(false, Ordering::Release);
}

pub fn perf_sched_in(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let counters = thr.perf_counters.lock();
    if counters.is_empty() {
        return;
    }
    let now = now_ns();
    let this_cpu = ax_hal::percpu::this_cpu_id();
    for ptc in counters.iter() {
        if ptc.dead.load(Ordering::Acquire) {
            continue;
        }
        if !ptc.enabled.load(Ordering::Acquire) {
            continue;
        }
        // Sampling with an unmapped ring cannot be armed; skip it entirely (not
        // marked on-CPU, so no enabled time accrues while unsampleable). `perf`
        // always mmaps before enable, so this is a rare race.
        if ptc.is_sampling && !ptc.ring_mapped() {
            continue;
        }
        // Mark on-CPU and open the `time_enabled` slice for EVERY enabled counter
        // — even one left degraded below — so an over-subscribed event still
        // accrues enabled time and `perf` can scale it. Open a NEW slice only on
        // the transition to on-CPU: `on_exec` re-enters this hook while a
        // `disabled=0` counter is already armed (`on_cpu && running`), and
        // clobbering `last_in_ns` then would push `time_running > time_enabled`
        // at the next `perf_sched_out`.
        if !ptc.on_cpu.swap(true, Ordering::AcqRel) {
            ptc.last_in_ns.store(now, Ordering::Release);
        }
        ptc.last_cpu.store(this_cpu, Ordering::Release);
        if ptc.running.load(Ordering::Acquire) {
            continue;
        }
        // big.LITTLE: an event opened against a specific cluster's PMU must not
        // arm on a non-matching core. Leave it degraded (on-CPU, so `time_enabled`
        // accrues, but no slot / `time_running`), so `perf` scales — matching
        // Linux's `pmu->filter`.
        if !ptc
            .valid_clusters
            .contains(super::percpu::current_cluster())
        {
            continue;
        }
        // Arm if a programmable counter is free on this core; otherwise leave it
        // degraded (running == false, no slot). The rotation tick
        // ([`perf_rotate_current`]) cycles the slots among the over-subscribed
        // events so each takes a turn on hardware.
        if let Some(n) = super::percpu::alloc_programmable_counter() {
            arm_slice(ptc, n, now, thr.proc_data.proc.pid() as u32, thr.tid());
        }
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
        // Process every counter that was on-CPU this period — including an
        // over-subscribed one that held no slot (so its `time_enabled` accrues),
        // and a `dead` one whose slot a remote fd-close left for us to free.
        if !ptc.on_cpu.load(Ordering::Acquire) {
            continue;
        }
        let dead = ptc.dead.load(Ordering::Acquire);
        // Accrue the whole on-CPU period into `time_enabled` (a `dead` counter
        // being torn down accrues nothing).
        if !dead {
            let last_in = ptc.last_in_ns.load(Ordering::Acquire);
            ptc.time_enabled_ns
                .fetch_add(now.saturating_sub(last_in), Ordering::AcqRel);
        }
        // If it currently holds a slot, fold its delta + `time_running` sub-slice
        // and release the slot to THIS core's pool (where it was reserved).
        if ptc.running.load(Ordering::Acquire) {
            disarm_slice(ptc, now, !dead);
        }
        ptc.on_cpu.store(false, Ordering::Release);
    }
}

/// Per-CPU rotation cursor, advanced once per perf tick to shift the window of
/// over-subscribed events that hold the hardware counters.
#[ax_percpu::def_percpu]
static ROTATE_CURSOR: usize = 0;

/// Whether `ptc` is eligible for counter rotation this tick: on-CPU, enabled,
/// alive, and a *counting* event. Sampling events keep their slot for the slice
/// (`perf record` with more sampling events than counters is out of rotation
/// scope — its overflow IRQ path is not designed to be torn down per tick).
fn rotation_eligible(ptc: &PerTaskCounter) -> bool {
    ptc.on_cpu.load(Ordering::Acquire)
        && ptc.enabled.load(Ordering::Acquire)
        && !ptc.dead.load(Ordering::Acquire)
        && !ptc.is_sampling
        // Skip a counter whose cluster mask excludes this core — it must not be
        // rotated onto hardware here (it accrues `time_enabled` only).
        && ptc.valid_clusters.contains(super::percpu::current_cluster())
}

/// Perf tick (Tier-2 multiplexing): if the currently-running task has more
/// enabled counting events than this core has programmable counters, rotate
/// which `free`-sized subset holds the counters so every event takes a turn on
/// hardware over time. The events not currently holding a counter still accrue
/// `time_enabled` (in [`perf_sched_out`]) but not `time_running`, so userspace
/// scales `value * time_enabled / time_running`.
///
/// Runs in timer-IRQ context on this core (alloc-free, no sleeping locks),
/// invoked via the [`ax_task::set_perf_tick`] hook from the periodic timer. The
/// rotation moves one event in / one out per tick (the window shifts by one),
/// mirroring Linux's `rotate_ctx`.
pub fn perf_rotate_current() {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let curr = ax_task::current();
    let Some(thr) = curr.try_as_thread() else {
        return;
    };
    let counters = thr.perf_counters.lock();
    if counters.is_empty() {
        return;
    }
    // Over-subscribed? Count the eligible (rotatable) counters and how many
    // already hold a slot.
    let mut n_eligible = 0usize;
    let mut held_eligible = 0usize;
    for ptc in counters.iter() {
        if rotation_eligible(ptc) {
            n_eligible += 1;
            if ptc.running.load(Ordering::Acquire) {
                held_eligible += 1;
            }
        }
    }
    // Slots available to THIS task = the ones it already holds + the free pool.
    // Sizing the window against the raw `PMCR.N` would over-count when a sys_cpu
    // or sampling event permanently holds a slot on this core, starving one
    // rotatable event forever.
    let free = held_eligible + super::percpu::free_programmable_count();
    if free == 0 {
        return;
    }
    if n_eligible <= free {
        return; // every eligible event already fits on hardware — no rotation.
    }
    let now = now_ns();
    // Advance the per-CPU cursor; the holding window is the `free` eligible events
    // at ranks `[cursor, cursor + free)` (mod `n_eligible`).
    let cursor = ROTATE_CURSOR.with_current(|c| {
        *c = c.wrapping_add(1);
        *c
    }) % n_eligible;

    // Pass 1 — evict counters that hold a slot but fell out of the window. This
    // frees slots first, so the admits in pass 2 can allocate them.
    let mut rank = 0usize;
    for ptc in counters.iter() {
        if !rotation_eligible(ptc) {
            continue;
        }
        let in_window = (rank + n_eligible - cursor) % n_eligible < free;
        rank += 1;
        if !in_window && ptc.running.load(Ordering::Acquire) {
            disarm_slice(ptc, now, true);
        }
    }
    // Pass 2 — admit window counters that are not yet holding a slot.
    let mut rank = 0usize;
    for ptc in counters.iter() {
        if !rotation_eligible(ptc) {
            continue;
        }
        let in_window = (rank + n_eligible - cursor) % n_eligible < free;
        rank += 1;
        if in_window
            && !ptc.running.load(Ordering::Acquire)
            && let Some(n) = super::percpu::alloc_programmable_counter()
        {
            arm_slice(ptc, n, now, thr.proc_data.proc.pid() as u32, thr.tid());
        }
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

/// Clone-inherit hook (`attr.inherit`): for each counter on the parent with
/// `inherit` set, create a matching counter on the freshly-cloned `child_thr` so
/// `perf record` follows it. The child counter writes into the **same ring** as
/// the parent event (the child has no fd / ring of its own): it is set up exactly
/// like a `PERF_EVENT_IOC_SET_OUTPUT` redirect, sharing the parent's `sample_id`
/// so all samples aggregate under one event. Inheritance is transitive — the
/// child's counter is itself `inherit`, so its own children inherit in turn (all
/// pointing at the one root ring via [`PerTaskCounter::inherit_ring`]).
///
/// Called from `do_clone` in the parent's context, *before* the child is
/// scheduled. The inherited counter reserves no HW slot here; it allocates one
/// per slice from its running core's per-CPU pool at `perf_sched_in`, like any
/// per-task counter. A sampling event whose ring is not mapped yet cannot be
/// followed (skipped).
pub fn on_clone_inherit(parent_thr: &Thread, child_thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    /// Everything needed to rebuild a child counter, snapshotted under the parent
    /// lock so the (allocating) child construction happens lock-free.
    struct InheritSpec {
        cfg: PerTaskConfig,
        sample_id: u64,
        ring: Option<(usize, usize, Arc<dyn Any + Send + Sync>)>,
        is_sampling: bool,
    }
    let specs: Vec<InheritSpec> = {
        let counters = parent_thr.perf_counters.lock();
        counters
            .iter()
            .filter(|p| p.inherit && !p.dead.load(Ordering::Acquire))
            .map(|p| InheritSpec {
                cfg: PerTaskConfig {
                    event: p.event,
                    exclude_user: p.exclude_user,
                    exclude_kernel: p.exclude_kernel,
                    read_format: p.read_format,
                    // Follow the parent's current enable state; the child runs the
                    // monitored workload from birth, so it does not wait on exec.
                    enabled: p.enabled.load(Ordering::Acquire),
                    enable_on_exec: false,
                    sample_period: p.sample_period,
                    sample_type: p.sample_type,
                    freq: p.freq,
                    target_freq: p.freq_target,
                    want_comm: p.want_comm,
                    want_mmap2: p.want_mmap2,
                    want_task: p.want_task,
                    sample_id_all: p.sample_id_all,
                    inherit: true,
                    valid_clusters: p.valid_clusters,
                },
                sample_id: p.sample_id.load(Ordering::Relaxed),
                ring: p.inherit_ring(),
                is_sampling: p.is_sampling,
            })
            .collect()
    };
    for spec in specs {
        // A sampling event with no ring yet has nowhere to write the child's
        // samples; skip (perf maps the ring before enabling, so this is rare).
        if spec.is_sampling && spec.ring.is_none() {
            continue;
        }
        // No slot is reserved here: the inherited child allocates a programmable
        // counter from its running core's per-CPU pool at its first
        // `perf_sched_in`, like any per-task counter. If that core's pool is
        // momentarily full the child simply isn't counted that slice (S4
        // rotation lets over-subscribed events take turns).
        let child = Arc::new(PerTaskCounter::new(spec.cfg));
        // Share the parent event's id so inherited samples aggregate under it.
        child.set_sample_id(spec.sample_id);
        // Redirect the child's output into the (root) parent ring it inherited.
        if let Some((vaddr, len, anchor)) = spec.ring {
            child.set_redirect_ring(vaddr, len, anchor);
        }
        attach(child_thr, child);
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
    // Snapshot the counter list, then DROP the lock before `free_hw`: the lock is
    // `SpinNoIrq` (IRQs off while held), and `free_hw` may issue a synchronous
    // cross-core IPI (remote-running teardown) and drop `Arc<GlobalPage>` (page
    // dealloc) — neither is safe under an IRQ-off lock that `perf_sched_in/out`
    // also take. `on_task_exit` runs in process context, so the clone is fine.
    let counters: Vec<Arc<PerTaskCounter>> = thr.perf_counters.lock().iter().cloned().collect();
    for ptc in &counters {
        if ptc.want_task
            && let Some(t) = sideband_target(ptc, pid, tid)
        {
            sideband::emit_exit(&t, pid, ppid, tid, ptid);
        }
        free_hw(ptc);
    }
}

/// Tear down the live HW slice of `ptc` on the *current* core, accounting the
/// final on-CPU slice exactly like [`perf_sched_out`] so it is not lost and stays
/// balanced: accrue the slice's `time_enabled` (matching the `time_running` that
/// [`disarm_slice`] folds, so `time_running <= time_enabled` holds), fold the
/// final counting delta, (for sampling) tear down the overflow-IRQ slot, and
/// release the programmable slot to this core's pool. `on_cpu` is cleared so the
/// owning core's later `perf_sched_out` does not double-count this slice.
/// Idempotent. MUST run on the core that holds the slice (the owning core),
/// directly or via [`teardown_slice_thunk`] over an IPI.
fn teardown_slice_local(ptc: &PerTaskCounter) {
    let now = now_ns();
    // Close the `time_enabled` slice for the final on-CPU period (the matching
    // `time_running` is closed by `disarm_slice` below).
    if ptc.on_cpu.swap(false, Ordering::AcqRel) {
        let last_in = ptc.last_in_ns.load(Ordering::Acquire);
        ptc.time_enabled_ns
            .fetch_add(now.saturating_sub(last_in), Ordering::AcqRel);
    }
    if ptc.running.load(Ordering::Acquire) {
        // Fold the final partial slice (counting) before the counter goes away,
        // so `perf stat -- cmd` does not lose the exit slice's count.
        disarm_slice(ptc, now, true);
    }
}

/// IPI thunk wrapping [`teardown_slice_local`] for the remote-fd-close case.
///
/// # Safety
/// `arg` must be a `*const PerTaskCounter` kept alive for the duration of the
/// call — guaranteed because [`free_hw`] blocks on `run_on_cpu_sync_raw` until
/// this returns.
unsafe fn teardown_slice_thunk(arg: *mut ()) {
    let ptc = unsafe { &*(arg as *const PerTaskCounter) };
    teardown_slice_local(ptc);
}

/// Release the HW counter backing `ptc` and tear down its bookkeeping, once.
///
/// Idempotent: the `hw_freed` compare-exchange ensures only the first caller
/// (either [`HwPerfEvent::drop`] on the fd side or [`on_task_exit`] on the task
/// side) does the work. It marks the counter `dead`, releases the live slice's
/// programmable slot back to the owning core's per-CPU pool, drops any sampling
/// anchors, and decrements [`PERF_TASK_ACTIVE`].
///
/// **SMP teardown safety.** The programmable slot was reserved on the core the
/// target last ran on (`last_cpu`). The slice must be torn down *on that core*,
/// never on the (possibly different) core that closed the fd: doing the
/// `disable(n)` / `unregister(n)` / slot-free on the wrong core would stomp
/// another task's counter `n` and corrupt the wrong pool. So when the target is
/// mid-slice on a remote core, [`free_hw`] issues a synchronous IPI to that core
/// (mirroring Linux `__perf_event_disable` via `smp_call_function_single`);
/// otherwise it tears down locally. For a sampling counter the anchors
/// (`Arc<GlobalPage>` ring + `Arc<IrqNotify>`) are dropped only *after* the slot
/// is unregistered, so the overflow handler can no longer reach them.
pub fn free_hw(ptc: &PerTaskCounter) {
    if ptc
        .hw_freed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // Already freed by the other side; nothing to do.
        return;
    }
    // Mark dead before touching HW so the scheduler hooks skip it forever after.
    ptc.dead.store(true, Ordering::Release);

    let this_cpu = ax_hal::percpu::this_cpu_id();
    let owner = ptc.last_cpu.load(Ordering::Acquire);
    if ptc.running.load(Ordering::Acquire) && owner != usize::MAX && owner != this_cpu {
        // Target is mid-slice on a remote core: tear the slice down ON that core.
        let arg = ptc as *const PerTaskCounter as *mut ();
        if ax_ipi::wait_until_cpu_ready(owner) {
            // SAFETY: `ptc` outlives the synchronous call (`run_on_cpu_sync_raw`
            // blocks until the thunk returns), and `teardown_slice_thunk` only
            // touches per-CPU PMU state on `owner`.
            let _ = unsafe { ax_ipi::run_on_cpu_sync_raw(owner, teardown_slice_thunk, arg) };
        } else {
            // Owner not ready (should not happen for a running target); fall back
            // to a local teardown attempt rather than leaking the slice.
            teardown_slice_local(ptc);
        }
    } else {
        // Owning core (or not running): tear down locally.
        teardown_slice_local(ptc);
    }

    if ptc.is_sampling {
        // Drop the ring/notify anchors and stop the worker, AFTER the slot is
        // unregistered above (the overflow handler holds the `notify`/ring
        // pointers live only while a slot references them). The ring pages
        // (`Arc<GlobalPage>`) drop here too — but the VMA holds its own strong
        // ref via the mmap retainer, so user memory stays mapped until munmap.
        let anchors = ptc.anchors.lock().take();
        if let Some(anchors) = anchors {
            anchors.poll_alive.store(false, Ordering::Release);
            anchors.notify.notify();
        }
        // Drop a SET_OUTPUT redirect anchor too, if this event was redirected
        // into another's ring (its own `anchors` is then `None`).
        *ptc.redirect_anchor.lock() = None;
        // Zero the published geometry so no later hook can re-arm a stale ring.
        ptc.ring_vaddr.store(0, Ordering::Release);
        ptc.notify_ptr.store(0, Ordering::Release);
    }
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
    // Live slice: add the in-progress count ONLY when the target is running on
    // THIS core. `PMEVCNTRn_EL0` is per-PE banked, so reading it for a target
    // running on another core would return the reader core's unrelated counter.
    // When the target runs elsewhere (or is not running), return accumulated-only
    // — monotonic, paired with `perf_sched_out`'s Release `fetch_add`, lagging by
    // at most one in-flight slice. (On a single core only a self-monitoring task
    // satisfies this; a separate monitor reads accumulated, as before.)
    if ptc.running.load(Ordering::Acquire)
        && ptc.last_cpu.load(Ordering::Acquire) == ax_hal::percpu::this_cpu_id()
    {
        let n = ptc.slot.load(Ordering::Acquire);
        if n != NO_SLOT {
            value += ax_cpu::pmu::counter::read(n);
            let now = now_ns();
            // `time_enabled` accrues over the whole on-CPU slice (`last_in_ns`);
            // `time_running` only since the slot was actually held (`run_since_ns`)
            // — for a rotation-admitted slice the latter is later, so basing both
            // on `last_in_ns` would over-report running (running > enabled).
            time_enabled += now.saturating_sub(ptc.last_in_ns.load(Ordering::Acquire));
            let run_since = ptc.run_since_ns.load(Ordering::Acquire);
            if run_since != 0 {
                time_running += now.saturating_sub(run_since);
            }
        }
    }
    (value, time_enabled, time_running)
}

/// Lost-sample count for this per-task event (`PERF_FORMAT_LOST`), bumped by the
/// overflow handler when the ring is full. Paired with the handler's `Relaxed`
/// `fetch_add`; a monotonic best-effort total is all `perf record` needs.
pub fn read_lost(ptc: &PerTaskCounter) -> u64 {
    ptc.lost.load(Ordering::Acquire)
}
