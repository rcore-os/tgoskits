//! Per-task hardware-PMU `perf` counting (`perf stat -- cmd`).
//!
//! Where [`super::hw`] can bind a system-wide event to an explicit CPU context,
//! this module counts a *specific task*: the counter is
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
//! A task-bound event opened with a nonzero sampling period and a supported
//! scalar `sample_type` behaves like an [M2 sampling
//! event](super::sampling) *while the attached task is running*, and fires no
//! samples while it is not — so the samples are attributed to the task.
//!
//! This reuses the M2 IRQ backend wholesale. The mechanism is:
//!
//! * `mmap(perf_fd)` allocates the ring (in [`super::hw::HwPerfEvent::device_mmap`])
//!   and publishes the ring plus page/notify anchors through the fd-owned
//!   [`PerfInheritanceFamily`]. Existing and future descendants receive the same
//!   output.
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
//! There is no counter multiplexing (so `time_running == time_enabled`).
//! Generation-bearing owner leases follow task migration across CPUs, and an
//! optional CPU filter limits eligibility. Sampling supports fixed-period
//! (`-c <period>`) and frequency mode (`-F`, `sample_freq`); inherited child
//! events share the root output through the same owned redirect boundary.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;

pub use super::inheritance::on_clone_inherit;
pub(crate) use super::task_sideband::{on_clone_sideband, on_exec_sideband, on_mmap_sideband};
use super::{
    cpu_worker, hw,
    inheritance::{PerfInheritanceFamily, PerfInheritanceFamilyWeak},
    output::{PerfOutputRoute, PerfRingOutput},
    resource_lifecycle::PmuResourceRelease,
    sampling::{self, SampleOutput, SampleSlot, SampleSlotConfig},
    sampling_lifecycle::{PmuCloseAction, PmuRunLease, PmuRunState, PmuStopClaim},
    sideband::{self, SidebandTarget},
    target::PerfCpuId,
};
use crate::task::{Thread, future::IrqNotify};

/// Number of per-task counters currently attached anywhere in the system.
///
/// Incremented by [`attach`] and decremented by [`free_hw`] (when the HW counter
/// is released). The scheduler hooks early-return while this is `0`, so an
/// idle perf subsystem costs one relaxed atomic load per context switch.
pub(super) static PERF_TASK_ACTIVE: AtomicUsize = AtomicUsize::new(0);

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
/// * `run_state` — the generation-bearing owner CPU and optional sampling
///   registration for the hardware-programmed slice.
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
    /// Optional Linux task-event CPU constraint (`cpu >= 0`).
    cpu_filter: Option<PerfCpuId>,

    /// Userspace wants this event counting (see the struct-level state machine).
    enabled: AtomicBool,
    /// Sole owner of schedule-in, schedule-out, remote stop, and close state.
    run_state: SpinNoIrq<PmuRunState>,
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
    // --- Per-task sampling (`perf record -- cmd`) ---
    /// This event samples (`sample_period > 0`): the scheduler hooks arm/disarm
    /// the overflow-IRQ path each slice instead of plain counting.
    is_sampling: bool,
    /// Sampling period (events between overflows); `0` for counting events. The
    /// counter is `preload`ed to overflow after this many events each slice. In
    /// frequency mode this is the per-slice initial estimate the handler adapts.
    sample_period: u32,
    /// Validated scalar `attr.sample_type`.
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
    /// Weak fd-owned family identity. The family owns members strongly, so a
    /// weak back-reference avoids a root/member cycle.
    family: SpinNoIrq<Option<FamilyBinding>>,
    /// Ensures the reserved PMU slot and global active count are reclaimed once
    /// when fd close races task exit.
    resources: PmuResourceRelease,

    /// Coherent own-ring and redirect ownership.
    ///
    /// The own ring is weakly retained so `munmap` permits a later mmap; a
    /// redirect is strongly retained while this event can publish into it.
    /// Scheduler/sideband readers clone one complete effective output.
    output: SpinNoIrq<PerfOutputRoute>,
    /// An inherited redirect targets the root event's poll worker, unlike an
    /// explicit `SET_OUTPUT` redirect whose wake ownership belongs to the target
    /// event.
    inherited_output_wake: AtomicBool,
    /// Strong notification and deferred poll machinery.
    anchors: SpinNoIrq<Option<SamplingAnchors>>,
}

#[derive(Clone, Debug)]
struct FamilyBinding {
    family: PerfInheritanceFamilyWeak,
    root: bool,
}

/// Strong references for one per-task sampling event's notification worker.
///
/// Mirrors the system-wide sampling notification state, but lives on the
/// [`PerTaskCounter`] (the task side) rather than the `HwPerfEvent` (the fd
/// side), because the slot the IRQ handler uses is built from the task side in
/// [`perf_sched_in`]. Published by [`PerfInheritanceFamily`] when the root fd is
/// mapped.
#[derive(Clone)]
pub(crate) struct SamplingAnchors {
    /// IRQ-safe notification the overflow handler pokes; drained by the worker.
    /// Registered slots clone this `Arc`; no IRQ path borrows its address.
    notify: Arc<IrqNotify>,
    /// Readiness set the perf fd's poller waits on; woken (`IoEvents::IN`) by the
    /// worker after each sample lands in the ring.
    poll_ready: Arc<axpoll::PollSet>,
    /// Liveness flag for the worker; cleared on family/fd close.
    poll_alive: Arc<AtomicBool>,
}

impl SamplingAnchors {
    pub(crate) fn new(
        notify: Arc<IrqNotify>,
        poll_ready: Arc<axpoll::PollSet>,
        poll_alive: Arc<AtomicBool>,
    ) -> Self {
        Self {
            notify,
            poll_ready,
            poll_alive,
        }
    }

    pub(crate) fn stop(&self) {
        self.poll_alive.store(false, Ordering::Release);
        self.notify.notify();
    }
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
    /// Optional CPU on which this task event is eligible to run.
    pub cpu_filter: Option<PerfCpuId>,
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
            cpu_filter: cfg.cpu_filter,
            enabled: AtomicBool::new(cfg.enabled),
            run_state: SpinNoIrq::new(PmuRunState::new()),
            accumulated: AtomicU64::new(0),
            time_enabled_ns: AtomicU64::new(0),
            time_running_ns: AtomicU64::new(0),
            last_in_ns: AtomicU64::new(0),
            enabled_at_ns: AtomicU64::new(0),
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
            family: SpinNoIrq::new(None),
            resources: PmuResourceRelease::new(),
            output: SpinNoIrq::new(PerfOutputRoute::new()),
            inherited_output_wake: AtomicBool::new(false),
            anchors: SpinNoIrq::new(None),
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

    pub(super) fn inherited_config(&self, n: usize) -> PerTaskConfig {
        PerTaskConfig {
            n,
            event: self.event,
            exclude_user: self.exclude_user,
            exclude_kernel: self.exclude_kernel,
            read_format: self.read_format,
            // Registration under the family relation lock publishes the current
            // root-fd control intent before the child becomes schedulable.
            enabled: false,
            enable_on_exec: false,
            cpu_filter: self.cpu_filter,
            sample_period: self.sample_period,
            sample_type: self.sample_type,
            freq: self.freq,
            target_freq: self.freq_target,
            want_comm: self.want_comm,
            want_mmap2: self.want_mmap2,
            want_task: self.want_task,
            sample_id_all: self.sample_id_all,
            inherit: true,
        }
    }

    /// Mark userspace-enabled (`ioctl(ENABLE)` / open-enabled). The target's next
    /// [`perf_sched_in`] programs the counter onto HW.
    pub fn set_enabled(&self) {
        if !self.enabled.swap(true, Ordering::AcqRel) {
            self.enabled_at_ns.store(now_ns(), Ordering::Relaxed);
        }
    }

    pub(crate) fn set_enabled_state(&self, enabled: bool) {
        if enabled {
            self.set_enabled();
        } else {
            self.enabled.store(false, Ordering::Release);
        }
    }

    pub(crate) fn bind_family(&self, family: PerfInheritanceFamilyWeak, root: bool) {
        let old = self.family.lock().replace(FamilyBinding { family, root });
        assert!(old.is_none(), "a task perf counter joined two families");
    }

    pub(crate) fn family(&self) -> Option<Arc<PerfInheritanceFamily>> {
        self.family.lock().as_ref()?.family.upgrade()
    }

    fn is_family_root(&self) -> bool {
        self.family
            .lock()
            .as_ref()
            .is_some_and(|binding| binding.root)
    }

    fn resources_released(&self) -> bool {
        self.resources.is_released()
    }

    pub(crate) fn retired_values(&self) -> (u64, u64, u64) {
        debug_assert!(
            self.resources_released(),
            "only a quiescent task event may be folded into family totals"
        );
        (
            self.accumulated.load(Ordering::Acquire),
            self.time_enabled_ns.load(Ordering::Acquire),
            self.time_running_ns.load(Ordering::Acquire),
        )
    }

    /// Whether this is a sampling event (`sample_period > 0`).
    pub fn is_sampling(&self) -> bool {
        self.is_sampling
    }

    pub(super) fn wants_comm(&self) -> bool {
        self.want_comm
    }

    pub(super) fn wants_mmap2(&self) -> bool {
        self.want_mmap2
    }

    pub(super) fn wants_task(&self) -> bool {
        self.want_task
    }

    pub(super) fn inheritable(&self) -> bool {
        self.inherit && !self.run_state.lock().is_stopping()
    }

    pub(super) fn sample_id(&self) -> u64 {
        self.sample_id.load(Ordering::Relaxed)
    }

    /// Record the ring buffer + notify/poll machinery for a sampling event.
    ///
    /// Called once, in process context, from
    /// [`super::hw::HwPerfEvent::device_mmap`] after the first `mmap(perf_fd)`.
    /// Stores the strong [`SamplingAnchors`] (pinning the ring pages + notify)
    /// and publishes the ring geometry after the anchors are installed.
    pub(crate) fn install_root_output(&self, output: &PerfRingOutput, anchors: SamplingAnchors) {
        *self.anchors.lock() = Some(anchors);
        self.inherited_output_wake.store(false, Ordering::Release);
        self.output.lock().publish_owned(output);
    }

    pub(crate) fn install_family_output(
        &self,
        output: PerfRingOutput,
        anchors: Option<SamplingAnchors>,
    ) {
        self.inherited_output_wake
            .store(anchors.is_some(), Ordering::Release);
        *self.anchors.lock() = anchors;
        self.output.lock().redirect(output);
    }

    pub(crate) fn clear_family_output(&self) {
        self.inherited_output_wake.store(false, Ordering::Release);
        self.anchors.lock().take();
        self.output.lock().clear();
    }

    /// Whether a sampling ring has been mmap'd and is therefore armable.
    ///
    /// Read by [`perf_sched_in`] (to decide whether to arm the slice) and by the
    /// fd's `device_mmap` (to reject a second mapping).
    pub fn ring_mapped(&self) -> bool {
        self.output.lock().owned().is_some()
    }

    /// Expose this counter's mmap ring for a `PERF_EVENT_IOC_SET_OUTPUT` redirect
    /// (target side). Only the event's own mmap ring may be shared.
    pub(crate) fn output_ring(&self) -> Option<PerfRingOutput> {
        self.output.lock().owned()
    }

    /// Point this counter's samples at *another* event's ring
    /// (`PERF_EVENT_IOC_SET_OUTPUT`, source side).
    ///
    /// Retains the target output, then publishes it so [`perf_sched_in`] arms
    /// this counter to write `PERF_RECORD_SAMPLE`s into it.
    /// A redirected source has no poll worker of its own; the target's poller
    /// observes the advancing `data_head`.
    pub(crate) fn set_redirect_ring(&self, output: PerfRingOutput) {
        self.inherited_output_wake.store(false, Ordering::Release);
        self.output.lock().redirect(output);
    }

    /// Detaches an explicit redirect.
    pub(crate) fn detach_redirect(&self) {
        self.inherited_output_wake.store(false, Ordering::Release);
        self.output.lock().detach();
    }

    /// Builds one owned IRQ registry output from the currently published ring.
    fn sample_output(&self) -> Option<SampleOutput> {
        let (ring, redirected) = self.output.lock().effective()?;
        let notify = if redirected && !self.inherited_output_wake.load(Ordering::Acquire) {
            None
        } else {
            self.anchors
                .lock()
                .as_ref()
                .map(|anchors| Arc::clone(&anchors.notify))
        };
        Some(SampleOutput::new(Some(ring), notify))
    }

    /// Readiness for `poll(perf_fd)`: `true` when the ring has unread bytes.
    ///
    /// Reads `data_head`/`data_tail` from the header page; used by the perf fd's
    /// [`super::hw::HwPerfEvent::poll`]. Returns `false` before the ring is
    /// mapped or once it is torn down.
    pub fn ring_has_data(&self) -> bool {
        let Some(ring) = self.output.lock().owned() else {
            return false;
        };
        let header = ring.ring_vaddr() as *const kbpf_basic::linux_bpf::perf_event_mmap_page;
        // SAFETY: the output snapshot pins the initialized header page and
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
/// Called from [`hw::perf_event_open_hw`] for a task target. Bumping
/// [`PERF_TASK_ACTIVE`] *after* the push ensures the hooks, once they start
/// running, always find the counter in the list.
pub fn attach(thr: &Thread, ptc: Arc<PerTaskCounter>) {
    let mut counters = thr.perf_counters().lock();
    // Closed events remain family-owned for aggregate reads, but need not stay
    // in a live task's scheduler list. Reclaim them in task context before the
    // bounded list accepts a new hardware-backed member.
    counters.retain(|counter| !counter.resources_released());
    counters
        .push(ptc)
        .expect("task perf list cannot exceed the architectural PMU slot limit");
    drop(counters);
    PERF_TASK_ACTIVE.fetch_add(1, Ordering::AcqRel);
}

/// Withdraws a counter whose family publication failed before its thread became
/// schedulable.
pub(super) fn detach_unpublished(thr: &Thread, ptc: &Arc<PerTaskCounter>) {
    let mut counters = thr.perf_counters().lock();
    let index = counters
        .iter()
        .position(|counter| Arc::ptr_eq(counter, ptc))
        .expect("an unpublished perf counter must retain its local reservation");
    counters.swap_remove(index);
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
/// Runs with IRQs disabled inside `switch_to` and uses only
/// [`SpinNoIrq`](ax_sync::spin::SpinNoIrq), atomics, and sysreg writes; it does
/// not allocate. `sampling::register` nests a further local-IRQ-off section.
pub fn perf_sched_in(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let counters = thr.perf_counters().lock();
    if counters.is_empty() {
        return;
    }
    let now = now_ns();
    let current_cpu = PerfCpuId::new(ax_hal::percpu::this_cpu_id());
    for ptc in counters.iter() {
        if !ptc.enabled.load(Ordering::Acquire) {
            continue;
        }
        if ptc.cpu_filter.is_some_and(|cpu| cpu != current_cpu) {
            continue;
        }
        let sample_output = if ptc.is_sampling {
            let Some(output) = ptc.sample_output() else {
                continue;
            };
            Some(output)
        } else {
            None
        };
        let mut run_state = ptc.run_state.lock();
        let Some(ticket) = run_state.begin_arm(current_cpu) else {
            continue;
        };
        if let Some(output) = sample_output {
            if let Err(error) = sampling::enable_local_pmu_irq() {
                run_state.cancel_arm(ticket);
                warn!(
                    "perf: failed to enable the PMU IRQ on CPU {}: {error:?}",
                    current_cpu.as_usize()
                );
                continue;
            }
            // configure() programs event + EL filter AND resets the counter to 0.
            ax_cpu::pmu::counter::configure(ptc.n, ptc.event, ptc.exclude_user, ptc.exclude_kernel);
            // Overflow after `sample_period` events.
            ax_cpu::pmu::counter::preload(ptc.n, ptc.sample_period);
            let registration = match sampling::register(
                ptc.n,
                SampleSlot::new(
                    output,
                    SampleSlotConfig {
                        period: ptc.sample_period,
                        sample_type: ptc.sample_type,
                        id: ptc.sample_id.load(Ordering::Relaxed),
                        // Frequency mode adapts the period within each slice; the
                        // slot starts at the initial estimate with no timestamp.
                        freq: ptc.freq,
                        target_freq: ptc.freq_target,
                        last_time: 0,
                    },
                ),
            ) {
                Ok(registration) => registration,
                Err(error) => {
                    run_state.cancel_arm(ticket);
                    warn!(
                        "perf: failed to register counter {} on CPU {}: {error:?}",
                        ptc.n,
                        current_cpu.as_usize()
                    );
                    continue;
                }
            };
            run_state.publish_registration(ticket, registration);
            // Arm the per-counter overflow interrupt, then start counting.
            ax_cpu::pmu::overflow::enable_irq(ptc.n);
            ax_cpu::pmu::counter::enable(ptc.n);
        } else {
            // Counting: configure() programs event + EL filter AND resets to 0.
            ax_cpu::pmu::counter::configure(ptc.n, ptc.event, ptc.exclude_user, ptc.exclude_kernel);
            ax_cpu::pmu::counter::enable(ptc.n);
        }
        ptc.last_in_ns.store(now, Ordering::Release);
        run_state.finish_arm(ticket);
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
    let counters = thr.perf_counters().lock();
    if counters.is_empty() {
        return;
    }
    for ptc in counters.iter() {
        let Some(lease) = ptc.run_state.lock().claim_schedule_out() else {
            continue;
        };
        stop_hardware_on_owner(ptc, lease)
            .unwrap_or_else(|error| panic!("scheduler PMU stop failed: {error}"));
        ptc.run_state.lock().finish_owner_stop(lease);
    }
}

/// Stops one exact PMU generation on its owner CPU.
///
/// The sampling order is mask → stop → clear pending overflow → generation
/// unregister. Local IRQ exclusion in the registry removal is the grace period
/// before its owned ring/notification references can be released.
fn stop_hardware_on_owner(ptc: &PerTaskCounter, lease: PmuRunLease) -> AxResult<()> {
    if lease.owner().as_usize() != ax_hal::percpu::this_cpu_id() {
        return Err(AxError::BadState);
    }
    if let Some(registration) = lease.registration() {
        if registration.counter() != ptc.n {
            return Err(AxError::BadState);
        }
        ax_cpu::pmu::overflow::disable_irq(ptc.n);
        ax_cpu::pmu::counter::disable(ptc.n);
        ax_cpu::pmu::overflow::clear(1 << ptc.n);
        sampling::unregister(registration).map_err(|_| AxError::BadState)?;
    } else {
        let delta = ax_cpu::pmu::counter::read(ptc.n);
        ptc.accumulated.fetch_add(delta, Ordering::AcqRel);
        ax_cpu::pmu::counter::disable(ptc.n);
    }

    let dt = now_ns().saturating_sub(ptc.last_in_ns.load(Ordering::Acquire));
    ptc.time_enabled_ns.fetch_add(dt, Ordering::AcqRel);
    ptc.time_running_ns.fetch_add(dt, Ordering::AcqRel);
    Ok(())
}

/// Completes one disable/close request on the CPU that owns `lease`.
///
/// The scheduler switch-out path may have won the same generation before the
/// affine worker gets CPU time. Generation state makes that case a successful
/// fence instead of a duplicate hardware unregister.
pub(crate) fn stop_requested_on_owner(ptc: &PerTaskCounter, lease: PmuRunLease) -> AxResult<()> {
    // The run-state guard must end before the hardware transaction and before
    // the completion path takes it again. A lock expression used directly as a
    // `match` scrutinee lives through the whole match and self-deadlocks in the
    // `Claimed` arm.
    let claim = ptc.run_state.lock().claim_requested_stop(lease);
    match claim {
        PmuStopClaim::Claimed(claimed) => {
            if let Err(error) = stop_hardware_on_owner(ptc, claimed) {
                ptc.run_state.lock().abort_owner_stop(claimed);
                return Err(error);
            }
            ptc.run_state.lock().finish_owner_stop(claimed);
            Ok(())
        }
        PmuStopClaim::AlreadyComplete => Ok(()),
        PmuStopClaim::InProgress => Err(AxError::ResourceBusy),
        PmuStopClaim::Stale => Err(AxError::BadState),
    }
}

/// Applies userspace disable intent and fences any live owner-CPU generation.
pub(crate) fn disable_counter(ptc: &Arc<PerTaskCounter>) -> AxResult<()> {
    ptc.enabled.store(false, Ordering::Release);
    // Never retain the non-sleeping run-state lock across an owner-CPU worker
    // rendezvous. See `stop_requested_on_owner` for the temporary-lifetime trap.
    let action = ptc.run_state.lock().begin_disable();
    match action {
        PmuCloseAction::AlreadyClosed | PmuCloseAction::Complete => Ok(()),
        PmuCloseAction::Stop(lease) => cpu_worker::stop_task_counter(Arc::clone(ptc), lease),
    }
}

/// Resets a task-bound count in the active owner CPU's scheduling order.
pub(crate) fn reset_counter(ptc: &Arc<PerTaskCounter>) -> AxResult<()> {
    let owner = ptc.run_state.lock().running().map(PmuRunLease::owner);
    if let Some(owner) = owner {
        cpu_worker::reset_task_counter(Arc::clone(ptc), owner)
    } else {
        ptc.accumulated.store(0, Ordering::Release);
        Ok(())
    }
}

/// Performs the hardware part of reset on a pinned owner CPU.
pub(crate) fn reset_task_on_owner(ptc: &PerTaskCounter) -> AxResult<()> {
    let run_state = ptc.run_state.lock();
    ptc.accumulated.store(0, Ordering::Release);
    if let Some(lease) = run_state.running() {
        if lease.owner().as_usize() != ax_hal::percpu::this_cpu_id() {
            // The event migrated before this worker ran. The accumulated reset
            // still linearizes before the new slice; do not touch remote PMU.
            return Ok(());
        }
        if ptc.is_sampling {
            ax_cpu::pmu::counter::preload(ptc.n, ptc.sample_period);
        } else {
            ax_cpu::pmu::counter::reset(ptc.n);
        }
    }
    Ok(())
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
        let counters = thr.perf_counters().lock();
        for ptc in counters.iter() {
            if ptc.run_state.lock().is_stopping() {
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
pub(super) fn sideband_target(ptc: &PerTaskCounter, pid: u32, tid: u32) -> Option<SidebandTarget> {
    if !(ptc.want_comm || ptc.want_mmap2 || ptc.want_task) {
        return None;
    }
    let ring = ptc.output.lock().effective()?.0;
    Some(SidebandTarget {
        ring,
        sample_type: ptc.sample_type,
        sample_id_all: ptc.sample_id_all,
        id: ptc.sample_id.load(Ordering::Relaxed),
        pid,
        tid,
    })
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
    let counters = thr.perf_counters().lock().clone();
    for ptc in &counters {
        if ptc.want_task
            && let Some(t) = sideband_target(ptc, pid, tid)
        {
            sideband::emit_exit(&t, pid, ppid, tid, ptid);
        }
        if let Err(error) = free_hw(ptc) {
            warn!(
                "perf: task-exit failed to quiesce counter {} on tid {}: {error}",
                ptc.n, tid
            );
        }
    }
}

/// Release the HW counter backing `ptc` and tear down its bookkeeping, once.
///
/// Idempotence and in-flight owner CPU identity are both held by
/// [`PmuRunState`] plus [`PmuResourceRelease`]. Either the fd side or task-exit
/// side may win the hardware stop; exactly one caller reclaims the reservation.
///
/// For a *sampling* counter that is currently armed, the overflow-IRQ path is
/// torn down in the UAF-safe order before the slot/ring `Arc`s drop: stop the
/// counter, mask the IRQ, then `unregister` the [`SampleSlot`] — so the overflow
/// handler can no longer reach the ring or notification anchor. An inherited
/// member then drops its redirect; the root output remains fd-owned until the
/// complete family has quiesced.
pub(crate) fn free_hw(ptc: &Arc<PerTaskCounter>) -> AxResult<()> {
    if ptc.resources_released() {
        return Ok(());
    }
    let close_action = ptc.run_state.lock().begin_close();
    let stop_result = match close_action {
        PmuCloseAction::AlreadyClosed | PmuCloseAction::Complete => Ok(()),
        PmuCloseAction::Stop(lease) => cpu_worker::stop_task_counter(Arc::clone(ptc), lease),
    };
    stop_result?;

    if !ptc.resources.claim() {
        return Ok(());
    }
    // An inherited task has no fd-owned output lifetime of its own. Its EXIT
    // side-band record was emitted before this fence, so its strong redirect
    // can now be dropped. Withdraw the family relation before returning the PMU
    // slot so a concurrent clone cannot reserve the hardware and then observe a
    // stale full-family snapshot.
    if !ptc.is_family_root() {
        if let Some(family) = ptc.family() {
            family.retire_child(ptc);
        }
        ptc.clear_family_output();
    }
    hw::free_programmable_counter(ptc.n);
    PERF_TASK_ACTIVE.fetch_sub(1, Ordering::AcqRel);
    Ok(())
}

/// Read back `(value, time_enabled, time_running)` for `read(perf_fd)`.
///
/// `value` is the accumulated delta plus the live slice if the counter is
/// currently running. For `perf stat -- cmd` the child has already exited by the
/// time the parent reads, so `running == false` and `accumulated` is final.
pub(crate) fn read_counter(ptc: &Arc<PerTaskCounter>) -> AxResult<(u64, u64, u64)> {
    let owner = ptc.run_state.lock().running().map(PmuRunLease::owner);
    if let Some(owner) = owner {
        cpu_worker::read_task_counter(Arc::clone(ptc), owner)
    } else {
        read_task_on_owner(ptc)
    }
}

/// Reads a task-bound event from a pinned owner worker or a detached state.
pub(crate) fn read_task_on_owner(ptc: &PerTaskCounter) -> AxResult<(u64, u64, u64)> {
    let mut value = ptc.accumulated.load(Ordering::Acquire);
    let mut time_enabled = ptc.time_enabled_ns.load(Ordering::Acquire);
    let mut time_running = ptc.time_running_ns.load(Ordering::Acquire);
    let run_state = ptc.run_state.lock();
    if let Some(lease) = run_state.running()
        && lease.owner().as_usize() == ax_hal::percpu::this_cpu_id()
    {
        // Live slice: add the in-progress count and elapsed time. This is a
        // local owner-CPU snapshot; remote reads are routed through the CPU
        // worker in the complete PMU ownership path.
        value += ax_cpu::pmu::counter::read(ptc.n);
        let dt = now_ns().saturating_sub(ptc.last_in_ns.load(Ordering::Acquire));
        time_enabled += dt;
        time_running += dt;
    }
    Ok((value, time_enabled, time_running))
}
