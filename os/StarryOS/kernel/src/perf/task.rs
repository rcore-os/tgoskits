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
//! Task events acquire counters from the CPU on which the task actually runs;
//! migration therefore releases the old CPU's slot before a later sched-in
//! allocates from the new CPU. The local timer rotates flexible events when a
//! task has more enabled events than available programmable counters.

use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering, fence},
};

use ax_alloc::GlobalPage;
use ax_runtime::hal::paging::MappingFlags;
use kbpf_basic::linux_bpf::perf_event_mmap_page;

use super::{
    sampling::{
        self, LossState, RingEndpoint, SampleReadEntry, SampleReadValue, SampleSlot,
        MAX_SAMPLE_READ_EVENTS,
    },
    sideband::{self, Mmap2Info, SidebandTarget},
};
use crate::{
    sync::IrqMutex,
    task::{
        AsThread, PidIdentity, PidIdentityId, PidNamespaceId, TgidNumber, Thread, TidNumber,
    },
};

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

/// [`PerTaskCounter::slot`] sentinel: no programmable counter is held.
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
    /// Programmable PMU counter index on [`Self::last_cpu`], or [`NO_SLOT`].
    /// Slots are acquired and released within one scheduling/multiplexing slice.
    slot: AtomicUsize,
    /// Last CPU on which this event entered the task context.
    last_cpu: AtomicUsize,
    /// Optional CPU constraint from `perf_event_open(pid, cpu, ...)`.
    cpu_filter: Option<usize>,
    /// Generation-stable target thread identity used to reject cross-context
    /// backend group links even if namespace-visible numeric TIDs are reused.
    owner_identity: PidIdentityId,
    /// Generic/raw/cache request resolved against each scheduling CPU.
    event: super::hw::PmuEventSpec,
    /// `attr.exclude_user`: do not count EL0 (`PMEVTYPERn_EL0.U`).
    exclude_user: bool,
    /// `attr.exclude_kernel`: do not count EL1 (`PMEVTYPERn_EL0.P`).
    exclude_kernel: bool,
    /// `attr.read_format`, controlling which fields `read(perf_fd)` emits.
    read_format: u64,
    /// `attr.enable_on_exec`: start counting only when the attached task
    /// `execve`s a new image (consumed by [`on_exec`]).
    enable_on_exec: bool,
    /// `attr.pinned`: a scheduling failure moves the event to Linux's ERROR
    /// state until userspace disables and enables it again.
    pinned: bool,

    /// Userspace wants this event counting (see the struct-level state machine).
    enabled: AtomicBool,
    /// The target task is currently executing in an eligible CPU context.
    on_cpu: AtomicBool,
    /// The event is programmed onto HW right now (target task is running).
    running: AtomicBool,
    /// Sum of completed-slice deltas (raw event count).
    accumulated: AtomicU64,
    /// Accumulated enabled time across past windows (ns).
    time_enabled_ns: AtomicU64,
    /// Accumulated running time across past windows (ns). Equal to
    /// `time_enabled_ns` with no multiplexing.
    time_running_ns: AtomicU64,
    /// Monotonic ns timestamp of the current hardware-running sub-slice.
    run_since_ns: AtomicU64,
    /// Monotonic ns timestamp at which the event last became `enabled`.
    /// Unused for the no-multiplexing timing math but kept for parity with the
    /// system-wide path and future multiplexing accounting.
    enabled_at_ns: AtomicU64,
    /// Pinned scheduling failure. Linux exposes this as EOF from `read()`.
    scheduling_error: AtomicBool,
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
    /// `attr.inherit`: clone this event onto `fork`/`clone` children (writing into
    /// the same ring) so `perf record` follows them. Driven by [`on_clone_inherit`].
    inherit: bool,
    /// PID namespace view captured when this event was opened.
    observer: PidNamespaceId,
    /// Stable owner ids captured for task sampling attribution.
    owner_ids: Option<(TgidNumber, TidNumber)>,

    // --- Linux event-group topology ---
    /// Weak ownership in both directions prevents fd/task lifetime cycles.
    group_leader: IrqMutex<Option<Weak<PerTaskCounter>>>,
    /// In insertion order, which is also Linux's leader-first READ order.
    group_members: IrqMutex<Vec<Weak<PerTaskCounter>>>,

    // --- Per-task counting mmap (`rdpmc`) ---
    /// Weak event-side reference to the VMA-owned metadata page. Scheduler
    /// hooks upgrade it only for a bounded publication; after `munmap`, a stale
    /// weak reference no longer blocks a replacement mapping. The IRQ-safe lock
    /// also serializes scheduler and fd-teardown writers.
    rdpmc_page: IrqMutex<Option<Weak<GlobalPage>>>,

    /// Raw pointer to the active [`RingEndpoint`] for the scheduler/IRQ hot
    /// path. Its owning Arc lives in `anchors` or `redirect_endpoint`, and can
    /// only be exchanged after the current hardware slice is disarmed.
    endpoint_ptr: AtomicUsize,
    /// Per-source loss accounting; retained for the entire counter lifetime.
    loss: Arc<LossState>,

    /// Strong anchors keeping the ring pages + notify alive, plus the deferred
    /// poll machinery. Set in process context by [`set_ring`](Self::set_ring),
    /// read in process context (`poll`/`register`/`free_hw`); never touched by
    /// the IRQ handler (which reaches the ring/notify through the registered
    /// [`SampleSlot`]'s raw pointers). Behind a [`IrqMutex`] so the hot-path
    /// hooks (which only read the atomics above) never block on it.
    anchors: IrqMutex<Option<SamplingAnchors>>,

    /// `PERF_EVENT_IOC_SET_OUTPUT` redirect anchor: when this event's samples are
    /// redirected into *another* event's ring, this pins that ring's pages while
    /// we may write into them. `ring_vaddr`/`ring_len` then point at the target
    /// ring and `notify_ptr` stays `0` (the target's poller re-checks
    /// `data_head`; the overflow handler guards the null notify). Set by
    /// [`set_redirect_ring`](Self::set_redirect_ring) instead of [`set_ring`](Self::set_ring).
    redirect_endpoint: IrqMutex<Option<Arc<RingEndpoint>>>,
}

/// Strong references that keep a per-task sampling event's ring + notify alive,
/// plus the `axpoll` readiness machinery the perf fd polls.
///
/// Mirrors the M2 `hw::SamplingState`/`RingState`, but lives on the
/// [`PerTaskCounter`] (the task side) rather than the `HwPerfEvent` (the fd
/// side), because the slot the IRQ handler uses is built from the task side in
/// [`perf_sched_in`]. Set once by [`PerTaskCounter::set_ring`].
struct SamplingAnchors {
    /// Stable ring ownership and writer serialization shared with redirecting
    /// sources.
    endpoint: Arc<RingEndpoint>,
    /// IRQ-safe notification the overflow handler pokes; drained by the worker.
    /// Holding this `Arc` keeps `notify_ptr` valid for the registered slot.
    notify: Arc<ax_task::IrqNotify>,
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
    /// Optional CPU constraint from `perf_event_open`.
    pub cpu_filter: Option<usize>,
    /// Generation-stable target thread identity.
    pub owner_identity: PidIdentityId,
    /// Generic/raw/cache event request.
    pub event: super::hw::PmuEventSpec,
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
    /// `attr.pinned`.
    pub pinned: bool,
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
    /// PID namespace view captured when the root event was opened.
    pub observer: PidNamespaceId,
    /// Target task identity in that namespace.
    pub owner_ids: Option<(TgidNumber, TidNumber)>,
}

impl PerTaskCounter {
    /// Build a per-task counter with no hardware slot initially assigned.
    ///
    /// The HW counter is *not* programmed here; it is configured + enabled lazily
    /// in [`perf_sched_in`] the next time the target task runs (or immediately
    /// from [`on_exec`] when the target is current during `execve`).
    pub fn new(cfg: PerTaskConfig) -> Self {
        PerTaskCounter {
            slot: AtomicUsize::new(NO_SLOT),
            last_cpu: AtomicUsize::new(usize::MAX),
            cpu_filter: cfg.cpu_filter,
            owner_identity: cfg.owner_identity,
            event: cfg.event,
            exclude_user: cfg.exclude_user,
            exclude_kernel: cfg.exclude_kernel,
            read_format: cfg.read_format,
            enable_on_exec: cfg.enable_on_exec,
            pinned: cfg.pinned,
            enabled: AtomicBool::new(cfg.enabled),
            on_cpu: AtomicBool::new(false),
            running: AtomicBool::new(false),
            accumulated: AtomicU64::new(0),
            time_enabled_ns: AtomicU64::new(0),
            time_running_ns: AtomicU64::new(0),
            run_since_ns: AtomicU64::new(0),
            enabled_at_ns: AtomicU64::new(if cfg.enabled { now_ns() } else { 0 }),
            scheduling_error: AtomicBool::new(false),
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
            inherit: cfg.inherit,
            observer: cfg.observer,
            owner_ids: cfg.owner_ids,
            group_leader: IrqMutex::new(None),
            group_members: IrqMutex::new(Vec::new()),
            rdpmc_page: IrqMutex::new(None),
            endpoint_ptr: AtomicUsize::new(0),
            loss: Arc::new(LossState::new()),
            anchors: IrqMutex::new(None),
            redirect_endpoint: IrqMutex::new(None),
        }
    }

    /// `attr.read_format` for serializing `read(perf_fd)`.
    pub fn read_format(&self) -> u64 {
        self.read_format
    }

    /// Total samples this source dropped because its output ring was full.
    pub fn lost_samples(&self) -> u64 {
        self.loss.total()
    }

    /// Whether this pinned event entered Linux's scheduling ERROR state.
    pub fn scheduling_error(&self) -> bool {
        self.scheduling_error.load(Ordering::Acquire)
    }

    /// Record the unique event id for `PERF_SAMPLE_ID` / `IDENTIFIER`. Called
    /// once at open (before the scheduler hooks run), so a relaxed store suffices.
    pub fn set_sample_id(&self, id: u64) {
        self.sample_id.store(id, Ordering::Relaxed);
    }

    /// Build the type-erased, IRQ-safe source descriptor stored in a sampling
    /// leader's fixed-capacity read table. The task's `perf_counters` list owns
    /// this counter until task teardown; teardown unregisters the SampleSlot
    /// synchronously before the list or ring anchors can be released.
    fn sample_read_entry(&self) -> SampleReadEntry {
        SampleReadEntry::new(
            core::ptr::from_ref(self).cast(),
            per_task_sample_read_irq,
            self.sample_id.load(Ordering::Relaxed),
        )
    }

    /// Link a hardware member to a hardware leader after the file layer has
    /// validated the public context/inherit rules.
    pub fn link_group(
        leader: &Arc<PerTaskCounter>,
        member: &Arc<PerTaskCounter>,
    ) -> crate::StarryResult<()> {
        if leader.owner_identity != member.owner_identity
            || leader.cpu_filter != member.cpu_filter
            || leader.dead.load(Ordering::Acquire)
            || member.dead.load(Ordering::Acquire)
        {
            return Err(crate::StarryError::InvalidInput);
        }
        let mut members = leader.group_members.lock();
        members.retain(|entry| {
            entry
                .upgrade()
                .is_some_and(|event| !event.dead.load(Ordering::Acquire))
        });
        if members.len() + 1 >= MAX_SAMPLE_READ_EVENTS {
            return Err(crate::StarryError::InvalidInput);
        }
        *member.group_leader.lock() = Some(Arc::downgrade(leader));
        members.push(Arc::downgrade(member));
        Ok(())
    }

    /// Mark userspace-enabled (`ioctl(ENABLE)` / open-enabled). The target's next
    /// [`perf_sched_in`] programs the counter onto HW.
    pub fn set_enabled(&self) -> crate::StarryResult<()> {
        if !self.enabled.swap(true, Ordering::AcqRel) {
            self.enabled_at_ns.store(now_ns(), Ordering::Relaxed);
        }
        self.scheduling_error.store(false, Ordering::Release);
        schedule_if_on_cpu(self)
    }

    /// Mark userspace-disabled (`ioctl(DISABLE)`). The next [`perf_sched_out`]
    /// (or an immediate one if the target is running) stops counting; here we
    /// only clear the intent so future slices do not re-program it.
    pub fn set_disabled(&self) -> crate::StarryResult<()> {
        if self.enabled.swap(false, Ordering::AcqRel) {
            let enabled_at = self.enabled_at_ns.swap(0, Ordering::AcqRel);
            if enabled_at != 0 {
                self.time_enabled_ns
                    .fetch_add(now_ns().saturating_sub(enabled_at), Ordering::AcqRel);
            }
        }
        disarm_on_owner(self)
    }

    /// Zero the accumulated value (`ioctl(RESET)`), leaving timing intact.
    /// Mirrors Linux's `PERF_EVENT_IOC_RESET`, which resets the count only.
    pub fn reset(&self) -> crate::StarryResult<()> {
        self.accumulated.store(0, Ordering::Release);
        reset_on_owner(self)
    }

    /// Whether a counting event already owns an mmap metadata page.
    pub fn rdpmc_page_mapped(&self) -> bool {
        self.rdpmc_page
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some()
    }

    /// Install and publish a counting event's mmap metadata page.
    ///
    /// The strong page anchor is installed before the address becomes visible
    /// to scheduler hooks. Returns `false` if this event already has a page.
    pub fn install_rdpmc_page(&self, pages: Arc<GlobalPage>) -> bool {
        let mut slot = self.rdpmc_page.lock();
        if slot.as_ref().and_then(Weak::upgrade).is_some() {
            return false;
        }
        self.write_rdpmc_snapshot(&pages, self.running.load(Ordering::Acquire));
        *slot = Some(Arc::downgrade(&pages));
        true
    }

    /// Publish one Linux `perf_event_mmap_page` snapshot.
    ///
    /// Active snapshots expose the reserved 1-based hardware index and put the
    /// completed-slice total in `offset`; inactive snapshots expose `index=0`
    /// and retain the full count in `offset`. The odd/even `lock` sequence lets
    /// userspace retry rather than combining fields from different slices.
    fn publish_rdpmc_page(&self, active: bool) {
        let page = self.rdpmc_page.lock();
        let Some(page) = page.as_ref().and_then(Weak::upgrade) else {
            return;
        };
        self.write_rdpmc_snapshot(&page, active);
    }

    fn write_rdpmc_snapshot(&self, page: &GlobalPage, active: bool) {
        let header = page.start_vaddr().as_usize() as *mut perf_event_mmap_page;
        let slot = self.slot.load(Ordering::Acquire);
        let index = if active && slot != NO_SLOT {
            slot as u32 + 1
        } else {
            0
        };
        let offset = self.accumulated.load(Ordering::Acquire) as i64;
        let time_enabled = self.time_enabled_ns.load(Ordering::Acquire);
        let time_running = self.time_running_ns.load(Ordering::Acquire);

        // SAFETY: `page` pins a zeroed, page-sized allocation and `lock` is a
        // naturally aligned u32 in `perf_event_mmap_page`. The `rdpmc_page`
        // lock serializes writers. Atomic odd/even publication plus volatile
        // metadata stores implement the userspace seqlock contract.
        unsafe {
            let sequence = AtomicU32::from_ptr(core::ptr::addr_of_mut!((*header).lock));
            let odd = sequence.load(Ordering::Relaxed).wrapping_add(1) | 1;
            sequence.store(odd, Ordering::SeqCst);
            core::ptr::addr_of_mut!((*header).index).write_volatile(index);
            core::ptr::addr_of_mut!((*header).offset).write_volatile(offset);
            core::ptr::addr_of_mut!((*header).time_enabled).write_volatile(time_enabled);
            core::ptr::addr_of_mut!((*header).time_running).write_volatile(time_running);
            fence(Ordering::Release);
            sequence.store(odd.wrapping_add(1), Ordering::Release);
        }
    }

    /// Stop scheduler updates after publishing the final inactive snapshot.
    fn release_rdpmc_page(&self) {
        *self.rdpmc_page.lock() = None;
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
        endpoint: Arc<RingEndpoint>,
        poll_ready: Arc<axpoll::PollSet>,
        poll_alive: Arc<AtomicBool>,
    ) -> crate::StarryResult<()> {
        let notify = endpoint.notify();
        *self.anchors.lock() = Some(SamplingAnchors {
            endpoint: endpoint.clone(),
            notify,
            poll_ready,
            poll_alive,
        });
        self.endpoint_ptr
            .store(Arc::as_ptr(&endpoint) as usize, Ordering::Release);
        schedule_if_on_cpu(self)
    }

    /// Whether a sampling ring has been mmap'd and is therefore armable.
    ///
    /// Read by [`perf_sched_in`] (to decide whether to arm the slice) and by the
    /// fd's `device_mmap` (to reject a second mapping).
    pub fn ring_mapped(&self) -> bool {
        self.endpoint_ptr.load(Ordering::Acquire) != 0
    }

    /// Expose this counter's mmap ring for a `PERF_EVENT_IOC_SET_OUTPUT` redirect
    /// (target side). Returns `(ring_vaddr, ring_len, pages)` with a strong clone
    /// of the ring `Arc` so the redirecting event pins the pages. `None` until the
    /// ring is mmap'd. Only an *owned* ring is shared, not a redirected one.
    pub fn output_ring(&self) -> Option<(usize, usize, Arc<dyn Any + Send + Sync>)> {
        let guard = self.anchors.lock();
        let anchors = guard.as_ref()?;
        let anchor: Arc<dyn Any + Send + Sync> = anchors.endpoint.clone();
        Some((
            anchors.endpoint.ring_vaddr(),
            anchors.endpoint.ring_len(),
            anchor,
        ))
    }

    /// Expose this counter's ring for an `attr.inherit` child to redirect into.
    ///
    /// Unlike [`output_ring`](Self::output_ring) this also works for a counter
    /// that is *itself* redirected (an inherited child of an inherited child):
    /// it hands back the redirect anchor so all descendants point at the one
    /// root ring. Returns `(ring_vaddr, ring_len, anchor)`, or `None` before the
    /// ring is mapped.
    pub fn inherit_ring(&self) -> Option<Arc<RingEndpoint>> {
        if let Some(anchors) = self.anchors.lock().as_ref() {
            return Some(anchors.endpoint.clone());
        }
        self.redirect_endpoint.lock().as_ref().cloned()
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
        endpoint: Arc<RingEndpoint>,
    ) -> crate::StarryResult<()> {
        disarm_on_owner(self)?;
        *self.redirect_endpoint.lock() = Some(endpoint.clone());
        self.endpoint_ptr
            .store(Arc::as_ptr(&endpoint) as usize, Ordering::Release);
        schedule_if_on_cpu(self)
    }

    /// Detaches `PERF_EVENT_IOC_SET_OUTPUT` and restores the event's own ring.
    pub fn detach_redirect_ring(&self) -> crate::StarryResult<()> {
        disarm_on_owner(self)?;
        *self.redirect_endpoint.lock() = None;
        let endpoint = self
            .anchors
            .lock()
            .as_ref()
            .map(|anchors| Arc::as_ptr(&anchors.endpoint) as usize)
            .unwrap_or(0);
        self.endpoint_ptr.store(endpoint, Ordering::Release);
        schedule_if_on_cpu(self)
    }

    /// Readiness for `poll(perf_fd)`: `true` when the ring has unread bytes.
    ///
    /// Reads `data_head`/`data_tail` from the header page; used by the perf fd's
    /// [`super::hw::HwPerfEvent::poll`]. Returns `false` before the ring is
    /// mapped or once it is torn down.
    pub fn ring_has_data(&self) -> bool {
        self.active_endpoint()
            .is_some_and(|endpoint| endpoint.has_data())
    }

    fn active_endpoint(&self) -> Option<Arc<RingEndpoint>> {
        if let Some(endpoint) = self.redirect_endpoint.lock().as_ref() {
            return Some(endpoint.clone());
        }
        self.anchors
            .lock()
            .as_ref()
            .map(|anchors| anchors.endpoint.clone())
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

/// Per-CPU timer hook used by the PMU multiplexing scheduler.
///
/// The callback is registered once on every CPU during perf initialization, so
/// this IRQ-context path only performs bounded locking, atomics, and PMU sysreg
/// operations.
pub fn perf_timer_tick() {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let current = ax_task::current();
    if let Some(thread) = current.try_as_thread() {
        perf_rotate_current(thread);
    }
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

fn eligible_on_current_cpu(ptc: &PerTaskCounter) -> bool {
    if ptc.dead.load(Ordering::Acquire)
        || !ptc.enabled.load(Ordering::Acquire)
        || ptc.scheduling_error.load(Ordering::Acquire)
    {
        return false;
    }
    let cpu = ax_hal::percpu::this_cpu_id();
    if ptc.cpu_filter.is_some_and(|filter| filter != cpu) {
        return false;
    }
    if ptc.is_sampling && !ptc.ring_mapped() {
        return false;
    }
    super::percpu::current_info().is_some_and(|info| ptc.event.resolve(info).is_ok())
}

fn live_group_leader(ptc: &PerTaskCounter) -> Option<Arc<PerTaskCounter>> {
    ptc.group_leader
        .lock()
        .as_ref()
        .and_then(Weak::upgrade)
        .filter(|leader| !leader.dead.load(Ordering::Acquire))
}

fn is_live_group_member(ptc: &PerTaskCounter) -> bool {
    live_group_leader(ptc).is_some()
}

fn effective_pinned(ptc: &PerTaskCounter) -> bool {
    ptc.pinned || live_group_leader(ptc).is_some_and(|leader| leader.pinned)
}

/// Build a leader-first read table while still in scheduler context. Weak
/// upgrades are bounded atomic reference-count operations and the returned raw
/// descriptors remain backed by the owning task's `perf_counters` list.
fn sample_read_entries(
    leader: &PerTaskCounter,
) -> ([SampleReadEntry; MAX_SAMPLE_READ_EVENTS], u8) {
    let mut entries = [SampleReadEntry::EMPTY; MAX_SAMPLE_READ_EVENTS];
    entries[0] = leader.sample_read_entry();
    let mut len = 1usize;
    let members = leader.group_members.lock();
    for member in members.iter().filter_map(Weak::upgrade) {
        if member.dead.load(Ordering::Acquire) || len == MAX_SAMPLE_READ_EVENTS {
            continue;
        }
        entries[len] = member.sample_read_entry();
        len += 1;
    }
    (entries, len as u8)
}

/// Read one task event from the PMU overflow handler without allocation,
/// sleeping locks, migration, or a cross-CPU call.
///
/// # Safety
///
/// `context` must be a live `PerTaskCounter` on the current CPU. The registered
/// SampleSlot lifetime and task counter list establish that requirement.
unsafe fn per_task_sample_read_irq(
    context: *const (),
    _source_slot: usize,
    now: u64,
    period: u32,
    account_source: bool,
) -> SampleReadValue {
    // SAFETY: guaranteed by the callback contract above.
    let ptc = unsafe { &*context.cast::<PerTaskCounter>() };
    if account_source {
        ptc.accumulated.fetch_add(period as u64, Ordering::AcqRel);
    }

    let mut value = ptc.accumulated.load(Ordering::Acquire);
    if !ptc.is_sampling && ptc.running.load(Ordering::Acquire) {
        let slot = ptc.slot.load(Ordering::Acquire);
        if slot != NO_SLOT
            && ptc.last_cpu.load(Ordering::Acquire) == ax_hal::percpu::this_cpu_id()
        {
            value = value.saturating_add(ax_cpu::pmu::counter::read(slot));
        }
    }

    let mut time_enabled = ptc.time_enabled_ns.load(Ordering::Acquire);
    let enabled_at = ptc.enabled_at_ns.load(Ordering::Acquire);
    if ptc.enabled.load(Ordering::Acquire) && enabled_at != 0 {
        time_enabled = time_enabled.saturating_add(now.saturating_sub(enabled_at));
    }
    let mut time_running = ptc.time_running_ns.load(Ordering::Acquire);
    let run_since = ptc.run_since_ns.load(Ordering::Acquire);
    if ptc.running.load(Ordering::Acquire) && run_since != 0 {
        time_running = time_running.saturating_add(now.saturating_sub(run_since));
    }
    SampleReadValue {
        value,
        time_enabled,
        time_running,
        lost: ptc.loss.total(),
    }
}

/// Arms one event on a slot owned by the executing CPU.
fn arm_slice(ptc: &PerTaskCounter, slot: usize, now: u64) {
    let Some(info) = super::percpu::current_info() else {
        super::percpu::free_programmable(slot);
        return;
    };
    let Ok(event) = ptc.event.resolve(info) else {
        super::percpu::free_programmable(slot);
        return;
    };
    if ptc.is_sampling {
        let (read_entries, read_len) = sample_read_entries(ptc);
        ax_cpu::pmu::counter::configure(
            slot,
            event,
            ptc.exclude_user,
            ptc.exclude_kernel,
        );
        ax_cpu::pmu::counter::preload(slot, ptc.sample_period);
        sampling::register(
            slot,
            SampleSlot {
                endpoint: ptc.endpoint_ptr.load(Ordering::Acquire) as *const RingEndpoint,
                loss: Arc::as_ptr(&ptc.loss),
                period: ptc.sample_period,
                sample_type: ptc.sample_type,
                id: ptc.sample_id.load(Ordering::Relaxed),
                read_format: ptc.read_format,
                read_entries,
                read_len,
                observer: ptc.observer,
                owner_ids: ptc.owner_ids,
                freq: ptc.freq,
                target_freq: ptc.freq_target,
                last_time: 0,
            },
        );
        ax_cpu::pmu::overflow::enable_irq(slot);
    } else {
        ax_cpu::pmu::counter::configure(
            slot,
            event,
            ptc.exclude_user,
            ptc.exclude_kernel,
        );
    }
    ptc.slot.store(slot, Ordering::Release);
    ptc.run_since_ns.store(now, Ordering::Release);
    ptc.running.store(true, Ordering::Release);
    ax_cpu::pmu::counter::enable(slot);
    if !ptc.is_sampling {
        ptc.publish_rdpmc_page(true);
    }
}

/// Disarms an event on the CPU that owns its current slot.
fn disarm_slice(ptc: &PerTaskCounter, now: u64, accumulate: bool) {
    let slot = ptc.slot.load(Ordering::Acquire);
    if slot == NO_SLOT {
        ptc.running.store(false, Ordering::Release);
        return;
    }
    ax_cpu::pmu::counter::disable(slot);
    if ptc.is_sampling {
        ax_cpu::pmu::overflow::disable_irq(slot);
        sampling::unregister(slot);
    } else if accumulate {
        ptc.accumulated
            .fetch_add(ax_cpu::pmu::counter::read(slot), Ordering::AcqRel);
    }
    let run_since = ptc.run_since_ns.swap(0, Ordering::AcqRel);
    if accumulate && run_since != 0 {
        ptc.time_running_ns
            .fetch_add(now.saturating_sub(run_since), Ordering::AcqRel);
    }
    super::percpu::free_programmable(slot);
    ptc.slot.store(NO_SLOT, Ordering::Release);
    ptc.running.store(false, Ordering::Release);
    if !ptc.is_sampling {
        ptc.publish_rdpmc_page(false);
    }
}

fn schedule_if_on_cpu(ptc: &PerTaskCounter) -> crate::StarryResult<()> {
    let group_leader = live_group_leader(ptc);
    let root = group_leader.as_deref().unwrap_or(ptc);
    if !root.on_cpu.load(Ordering::Acquire)
        || (ptc.running.load(Ordering::Acquire) && group_leader.is_none())
    {
        return Ok(());
    }
    let owner = root.last_cpu.load(Ordering::Acquire);
    if owner == usize::MAX {
        return Ok(());
    }
    // SAFETY: the closure performs only bounded atomic, PMU, and IRQ-registry
    // operations and keeps `ptc` borrowed until the synchronous IPI returns.
    unsafe {
        super::percpu::run_on_cpu_sync(owner, || {
            if root.last_cpu.load(Ordering::Acquire) != ax_hal::percpu::this_cpu_id()
                || !root.on_cpu.load(Ordering::Acquire)
                || !eligible_on_current_cpu(root)
            {
                return;
            }
            if !root.group_members.lock().is_empty() {
                schedule_group(root, now_ns());
            } else if let Some(slot) = super::percpu::alloc_programmable() {
                arm_slice(root, slot, now_ns());
            } else if root.pinned {
                root.scheduling_error.store(true, Ordering::Release);
            }
        })
    }
}

/// Collect enabled members and program the complete hardware group as one PMU
/// transaction. No event is armed unless every slot has first been reserved.
fn schedule_group(leader: &PerTaskCounter, now: u64) -> bool {
    let mut members_snapshot: [Option<Arc<PerTaskCounter>>; MAX_SAMPLE_READ_EVENTS] =
        core::array::from_fn(|_| None);
    let mut member_len = 0usize;
    {
        let members = leader.group_members.lock();
        for member in members.iter().filter_map(Weak::upgrade) {
            if member.dead.load(Ordering::Acquire) || !member.enabled.load(Ordering::Acquire) {
                continue;
            }
            if member_len + 1 == MAX_SAMPLE_READ_EVENTS || !eligible_on_current_cpu(&member) {
                if leader.pinned {
                    leader.scheduling_error.store(true, Ordering::Release);
                }
                return false;
            }
            members_snapshot[member_len] = Some(member);
            member_len += 1;
        }
    }

    // A member may have been enabled while the leader was already running.
    // Remove the old (smaller) placement before attempting the new transaction.
    if leader.running.load(Ordering::Acquire) {
        disarm_slice(leader, now, true);
    }
    for member in members_snapshot[..member_len]
        .iter()
        .filter_map(Option::as_deref)
    {
        if member.running.load(Ordering::Acquire) {
            disarm_slice(member, now, true);
        }
    }

    let mut slots = [NO_SLOT; MAX_SAMPLE_READ_EVENTS];
    for index in 0..=member_len {
        let Some(slot) = super::percpu::alloc_programmable() else {
            for reserved in slots[..index].iter().copied() {
                super::percpu::free_programmable(reserved);
            }
            if leader.pinned {
                leader.scheduling_error.store(true, Ordering::Release);
            }
            return false;
        };
        slots[index] = slot;
    }

    // Counting siblings must be live before the sampling leader is armed: the
    // leader's SampleSlot captures their complete leader-first read snapshot.
    for index in (0..member_len).rev() {
        arm_slice(
            members_snapshot[index].as_deref().unwrap(),
            slots[index + 1],
            now,
        );
    }
    arm_slice(leader, slots[0], now);
    true
}

fn disarm_on_owner(ptc: &PerTaskCounter) -> crate::StarryResult<()> {
    if !ptc.running.load(Ordering::Acquire) {
        return Ok(());
    }
    let owner = ptc.last_cpu.load(Ordering::Acquire);
    if owner == usize::MAX {
        return Ok(());
    }
    // SAFETY: see `schedule_if_on_cpu`; disarming is allocation-free and the
    // borrowed event remains live through the synchronous call.
    unsafe {
        super::percpu::run_on_cpu_sync(owner, || {
            if ptc.last_cpu.load(Ordering::Acquire) == ax_hal::percpu::this_cpu_id()
                && ptc.running.load(Ordering::Acquire)
            {
                disarm_slice(ptc, now_ns(), true);
            }
        })
    }
}

fn reset_on_owner(ptc: &PerTaskCounter) -> crate::StarryResult<()> {
    if !ptc.running.load(Ordering::Acquire) {
        if !ptc.is_sampling {
            ptc.publish_rdpmc_page(false);
        }
        return Ok(());
    }
    let owner = ptc.last_cpu.load(Ordering::Acquire);
    if owner == usize::MAX {
        return Ok(());
    }
    // SAFETY: the owner-side operation is a bounded PMU register write.
    unsafe {
        super::percpu::run_on_cpu_sync(owner, || {
            let slot = ptc.slot.load(Ordering::Acquire);
            if ptc.last_cpu.load(Ordering::Acquire) != ax_hal::percpu::this_cpu_id()
                || slot == NO_SLOT
            {
                return;
            }
            if ptc.is_sampling {
                ax_cpu::pmu::counter::preload(slot, ptc.sample_period);
            } else {
                ax_cpu::pmu::counter::reset(slot);
                ptc.publish_rdpmc_page(true);
            }
        })
    }
}

fn schedule_pinned(counters: &[Arc<PerTaskCounter>], now: u64) {
    for ptc in counters {
        if !ptc.pinned
            || is_live_group_member(ptc)
            || !eligible_on_current_cpu(ptc)
            || ptc.running.load(Ordering::Acquire)
        {
            continue;
        }
        if !ptc.group_members.lock().is_empty() {
            schedule_group(ptc, now);
            continue;
        }
        if let Some(slot) = super::percpu::alloc_programmable() {
            arm_slice(ptc, slot, now);
        } else {
            ptc.scheduling_error.store(true, Ordering::Release);
        }
    }
}

fn schedule_flexible(counters: &[Arc<PerTaskCounter>], start: usize, now: u64) {
    for offset in 0..counters.len() {
        let ptc = &counters[(start + offset) % counters.len()];
        if ptc.pinned
            || is_live_group_member(ptc)
            || !eligible_on_current_cpu(ptc)
            || ptc.running.load(Ordering::Acquire)
        {
            continue;
        }
        if !ptc.group_members.lock().is_empty() {
            schedule_group(ptc, now);
            continue;
        }
        let Some(slot) = super::percpu::alloc_programmable() else {
            break;
        };
        arm_slice(ptc, slot, now);
    }
}

/// Scheduler hook: marks the task on this CPU, schedules pinned events first,
/// then fills remaining PMU slots with flexible events.
pub fn perf_sched_in(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let counters = thr.perf_counters.lock();
    if counters.is_empty() {
        return;
    }
    let now = now_ns();
    let cpu = ax_hal::percpu::this_cpu_id();
    for ptc in counters.iter() {
        if eligible_on_current_cpu(ptc) {
            ptc.on_cpu.store(true, Ordering::Release);
            ptc.last_cpu.store(cpu, Ordering::Release);
        }
    }
    schedule_pinned(&counters, now);
    schedule_flexible(&counters, 0, now);
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
        if ptc.running.load(Ordering::Acquire) {
            disarm_slice(ptc, now, !ptc.dead.load(Ordering::Acquire));
        }
        ptc.on_cpu.store(false, Ordering::Release);
    }
}

/// Timer-IRQ multiplexing step for the currently running task.
fn perf_rotate_current(thr: &Thread) {
    let counters = thr.perf_counters.lock();
    if counters.len() < 2 {
        return;
    }
    let mut eligible = 0usize;
    for ptc in counters.iter() {
        if !effective_pinned(ptc)
            && !is_live_group_member(ptc)
            && eligible_on_current_cpu(ptc)
        {
            eligible += 1;
        }
    }
    if eligible < 2 {
        return;
    }
    let now = now_ns();
    for ptc in counters.iter() {
        if !effective_pinned(ptc) && ptc.running.load(Ordering::Acquire) {
            disarm_slice(ptc, now, true);
        }
    }
    let start = super::percpu::next_rotation_start(counters.len());
    schedule_flexible(&counters, start, now);
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
                ptc.scheduling_error.store(false, Ordering::Release);
            }
        }
    }
    // Program the now-enabled counters onto HW for the current task. Takes the
    // list lock itself, so it is released above first.
    perf_sched_in(thr);
}

/// Build a side-band write target for `ptc` if it has a mapped ring and requested
/// any side-band record (`attr.comm`/`mmap2`/`task`); else `None`.
fn visible_tgid(ptc: &PerTaskCounter, identity: &PidIdentity) -> Option<TgidNumber> {
    identity
        .visible_number_in(ptc.observer)
        .map(TgidNumber::from)
}

fn visible_tid(ptc: &PerTaskCounter, identity: &PidIdentity) -> Option<TidNumber> {
    identity
        .visible_number_in(ptc.observer)
        .map(TidNumber::from)
}

fn sideband_target(ptc: &PerTaskCounter, thread: &Thread) -> Option<SidebandTarget> {
    if !(ptc.want_comm || ptc.want_mmap2 || ptc.want_task) {
        return None;
    }
    let endpoint = ptc.active_endpoint()?;
    let pid = visible_tgid(ptc, &thread.proc_data.identity())?;
    let tid = visible_tid(ptc, &thread.pid_identity())?;
    Some(SidebandTarget {
        endpoint,
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
                sideband_target(ptc, thr).map(|target| WantTarget {
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
    let targets: Vec<SidebandTarget> = {
        let counters = thr.perf_counters.lock();
        counters
            .iter()
            .filter(|ptc| ptc.want_mmap2)
            .filter_map(|ptc| sideband_target(ptc, thr))
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
pub fn on_clone_sideband(
    parent_thr: &Thread,
    child_process: &PidIdentity,
    child_thread: &PidIdentity,
) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    // Snapshot want_task targets, then drop the counter lock before any ring write.
    let targets: Vec<(SidebandTarget, TgidNumber, TidNumber, TgidNumber, TidNumber)> = {
        let counters = parent_thr.perf_counters.lock();
        counters
            .iter()
            .filter(|ptc| ptc.want_task)
            .filter_map(|ptc| {
                Some((
                    sideband_target(ptc, parent_thr)?,
                    visible_tgid(ptc, child_process)?,
                    visible_tid(ptc, child_thread)?,
                    visible_tgid(ptc, &parent_thr.proc_data.identity())?,
                    visible_tid(ptc, &parent_thr.pid_identity())?,
                ))
            })
            .collect()
    };
    for (target, child_pid, child_tid, parent_pid, parent_tid) in &targets {
        sideband::emit_fork(target, *child_pid, *parent_pid, *child_tid, *parent_tid);
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
/// scheduled. Each inherited counter takes its own programmable HW slot; if the
/// slots are exhausted the inheritance for that event is skipped (the child is
/// simply not monitored — we do not time-multiplex), and likewise a sampling
/// event whose ring is not mapped yet cannot be followed.
pub fn on_clone_inherit(parent_thr: &Thread, child_thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    /// Everything needed to rebuild a child counter, snapshotted under the parent
    /// lock so the (allocating) child construction happens lock-free.
    struct InheritSpec {
        cfg: PerTaskConfig,
        sample_id: u64,
        ring: Option<Arc<RingEndpoint>>,
        is_sampling: bool,
    }
    let specs: Vec<InheritSpec> = {
        let counters = parent_thr.perf_counters.lock();
        counters
            .iter()
            .filter(|p| p.inherit && !p.dead.load(Ordering::Acquire))
            .map(|p| InheritSpec {
                cfg: PerTaskConfig {
                    cpu_filter: p.cpu_filter,
                    owner_identity: child_thr.pid_identity().id(),
                    event: p.event,
                    exclude_user: p.exclude_user,
                    exclude_kernel: p.exclude_kernel,
                    read_format: p.read_format,
                    // Follow the parent's current enable state; the child runs the
                    // monitored workload from birth, so it does not wait on exec.
                    enabled: p.enabled.load(Ordering::Acquire),
                    enable_on_exec: false,
                    pinned: p.pinned,
                    sample_period: p.sample_period,
                    sample_type: p.sample_type,
                    freq: p.freq,
                    target_freq: p.freq_target,
                    want_comm: p.want_comm,
                    want_mmap2: p.want_mmap2,
                    want_task: p.want_task,
                    sample_id_all: p.sample_id_all,
                    inherit: true,
                    observer: p.observer,
                    owner_ids: visible_tgid(p, &child_thr.proc_data.identity())
                        .zip(visible_tid(p, &child_thr.pid_identity())),
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
        let child = Arc::new(PerTaskCounter::new(spec.cfg));
        // Share the parent event's id so inherited samples aggregate under it.
        child.set_sample_id(spec.sample_id);
        // Redirect the child's output into the (root) parent ring it inherited.
        if let Some(endpoint) = spec.ring
            && child.set_redirect_ring(endpoint).is_err()
        {
            continue;
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
    let counters = thr.perf_counters.lock().clone();
    for ptc in &counters {
        if ptc.want_task
            && let Some(target) = sideband_target(ptc, thr)
        {
            let pid = target.pid;
            let tid = target.tid;
            let parent = thr.proc_data.proc.parent().and_then(|parent| {
                let number = parent.identity().visible_number_in(ptc.observer)?;
                Some((TgidNumber::from(number), TidNumber::from(number)))
            });
            sideband::emit_exit(
                &target,
                pid,
                parent.map(|(pid, _)| pid),
                tid,
                parent.map(|(_, tid)| tid),
            );
        }
        free_hw(ptc);
    }
}

/// Release the live per-CPU slice and tear down this event once.
///
/// Idempotent: the `hw_freed` compare-exchange ensures only the first caller
/// (either [`HwPerfEvent::drop`] on the fd side or [`on_task_exit`] on the task
/// side) does the work. A live slice is synchronously stopped on the CPU that
/// owns the banked PMU registers before its ring/notify anchors may be dropped.
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
    // Mark dead before touching HW so scheduler/timer paths stop re-arming it.
    ptc.dead.store(true, Ordering::Release);
    if ptc.enabled.swap(false, Ordering::AcqRel) {
        let enabled_at = ptc.enabled_at_ns.swap(0, Ordering::AcqRel);
        if enabled_at != 0 {
            ptc.time_enabled_ns
                .fetch_add(now_ns().saturating_sub(enabled_at), Ordering::AcqRel);
        }
    }
    if let Err(error) = disarm_on_owner(ptc) {
        // Keep the event and its strong anchors reachable from the task list if
        // owner-CPU teardown failed. Dropping them here could leave the PMU IRQ
        // registry with dangling ring/notify pointers.
        warn!("perf: failed to stop task event on its owner CPU: {error}");
        ptc.hw_freed.store(false, Ordering::Release);
        return;
    }
    ptc.on_cpu.store(false, Ordering::Release);
    if ptc.is_sampling {
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
        // Release a redirect only after unregistering the slot, then withdraw
        // the raw endpoint pointer so no later hook can re-arm stale ownership.
        *ptc.redirect_endpoint.lock() = None;
        ptc.endpoint_ptr.store(0, Ordering::Release);
    } else {
        ptc.publish_rdpmc_page(false);
        ptc.release_rdpmc_page();
    }
    PERF_TASK_ACTIVE.fetch_sub(1, Ordering::AcqRel);
}

/// Read back `(value, time_enabled, time_running)` for `read(perf_fd)`.
///
/// `value` is the accumulated delta plus the live slice if the counter is
/// currently running. For `perf stat -- cmd` the child has already exited by the
/// time the parent reads, so `running == false` and `accumulated` is final.
pub fn read_values(ptc: &PerTaskCounter) -> crate::StarryResult<(u64, u64, u64)> {
    let mut value = ptc.accumulated.load(Ordering::Acquire);
    let mut time_enabled = ptc.time_enabled_ns.load(Ordering::Acquire);
    let mut time_running = ptc.time_running_ns.load(Ordering::Acquire);
    let now = now_ns();
    let enabled_at = ptc.enabled_at_ns.load(Ordering::Acquire);
    if ptc.enabled.load(Ordering::Acquire) && enabled_at != 0 {
        time_enabled += now.saturating_sub(enabled_at);
    }
    if ptc.running.load(Ordering::Acquire) {
        let owner = ptc.last_cpu.load(Ordering::Acquire);
        if owner != usize::MAX {
            // SAFETY: the closure only reads owner-local PMU/atomic state and
            // returns before `ptc` can be released.
            let (live_value, live_running) = unsafe {
                super::percpu::run_on_cpu_sync(owner, || {
                    let slot = ptc.slot.load(Ordering::Acquire);
                    if !ptc.running.load(Ordering::Acquire)
                        || ptc.last_cpu.load(Ordering::Acquire)
                            != ax_hal::percpu::this_cpu_id()
                        || slot == NO_SLOT
                    {
                        return (0, 0);
                    }
                    let value = if ptc.is_sampling {
                        0
                    } else {
                        ax_cpu::pmu::counter::read(slot)
                    };
                    let running = now_ns()
                        .saturating_sub(ptc.run_since_ns.load(Ordering::Acquire));
                    (value, running)
                })
            }?;
            value += live_value;
            time_running += live_running;
        }
    }
    Ok((value, time_enabled, time_running))
}
