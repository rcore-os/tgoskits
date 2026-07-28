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
//! There is no counter multiplexing (so `time_running == time_enabled`).
//! Generation-bearing owner leases follow task migration across CPUs, and an
//! optional CPU filter limits eligibility. Sampling supports fixed-period
//! (`-c <period>`) and frequency mode (`-F`, `sample_freq`); inherited child
//! events share the root output through the same owned redirect boundary.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use ax_runtime::hal::paging::MappingFlags;

use super::{
    cpu_worker, hw,
    output::{PerfOutputRoute, PerfRingOutput},
    sampling::{self, SampleOutput, SampleSlot, SampleSlotConfig},
    sampling_lifecycle::{PmuCloseAction, PmuRunLease, PmuRunState, PmuStopClaim},
    sideband::{self, Mmap2Info, SidebandTarget},
    target::PerfCpuId,
};
use crate::task::{Thread, future::IrqNotify};

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

    /// Coherent own-ring and redirect ownership.
    ///
    /// The own ring is weakly retained so `munmap` permits a later mmap; a
    /// redirect is strongly retained while this event can publish into it.
    /// Scheduler/sideband readers clone one complete effective output.
    output: SpinNoIrq<PerfOutputRoute>,
    /// Strong notification and deferred poll machinery.
    anchors: SpinNoIrq<Option<SamplingAnchors>>,
}

/// Strong references for one per-task sampling event's notification worker.
///
/// Mirrors the system-wide sampling notification state, but lives on the
/// [`PerTaskCounter`] (the task side) rather than the `HwPerfEvent` (the fd
/// side), because the slot the IRQ handler uses is built from the task side in
/// [`perf_sched_in`]. Set once by [`PerTaskCounter::set_ring`].
struct SamplingAnchors {
    /// IRQ-safe notification the overflow handler pokes; drained by the worker.
    /// Registered slots clone this `Arc`; no IRQ path borrows its address.
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
            output: SpinNoIrq::new(PerfOutputRoute::new()),
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

    /// Mark userspace-enabled (`ioctl(ENABLE)` / open-enabled). The target's next
    /// [`perf_sched_in`] programs the counter onto HW.
    pub fn set_enabled(&self) {
        if !self.enabled.swap(true, Ordering::AcqRel) {
            self.enabled_at_ns.store(now_ns(), Ordering::Relaxed);
        }
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
    /// and publishes the ring geometry after the anchors are installed.
    pub fn set_ring(
        &self,
        output: &PerfRingOutput,
        notify: Arc<IrqNotify>,
        poll_ready: Arc<axpoll::PollSet>,
        poll_alive: Arc<AtomicBool>,
    ) {
        *self.anchors.lock() = Some(SamplingAnchors {
            notify,
            poll_ready,
            poll_alive,
        });
        self.output.lock().publish_owned(output);
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

    /// Expose this counter's ring for an `attr.inherit` child to redirect into.
    ///
    /// Unlike [`output_ring`](Self::output_ring) this also works for a counter
    /// that is *itself* redirected (an inherited child of an inherited child):
    /// it hands back the redirect anchor so all descendants point at the one
    /// root ring.
    pub(crate) fn inherit_ring(&self) -> Option<PerfRingOutput> {
        self.output.lock().effective().map(|(output, _)| output)
    }

    /// Point this counter's samples at *another* event's ring
    /// (`PERF_EVENT_IOC_SET_OUTPUT`, source side).
    ///
    /// Retains the target output, then publishes it so [`perf_sched_in`] arms
    /// this counter to write `PERF_RECORD_SAMPLE`s into it.
    /// A redirected source has no poll worker of its own; the target's poller
    /// observes the advancing `data_head`.
    pub(crate) fn set_redirect_ring(&self, output: PerfRingOutput) {
        self.output.lock().redirect(output);
    }

    /// Detaches an explicit redirect.
    pub(crate) fn detach_redirect(&self) {
        self.output.lock().detach();
    }

    /// Builds one owned IRQ registry output from the currently published ring.
    fn sample_output(&self) -> Option<SampleOutput> {
        let (ring, redirected) = self.output.lock().effective()?;
        let notify = if redirected {
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
    thr.perf_counters().lock().push(ptc);
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
///   further local-IRQ-off section, which is fine.
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
    match ptc.run_state.lock().claim_requested_stop(lease) {
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
    match ptc.run_state.lock().begin_disable() {
        PmuCloseAction::AlreadyClosed | PmuCloseAction::Complete => Ok(()),
        PmuCloseAction::Stop(lease) => cpu_worker::stop_task_counter(Arc::clone(ptc), lease),
    }
}

/// Changes one task event's output after fencing its owner-CPU slot.
///
/// Registered sampling slots own immutable output snapshots. Fencing the live
/// generation before publishing a new route prevents an ioctl from returning
/// while hard IRQs can still write the previous destination indefinitely.
pub(crate) fn redirect_output(ptc: &Arc<PerTaskCounter>, output: PerfRingOutput) -> AxResult<()> {
    replace_output(ptc, Some(output))
}

/// Detaches one task event's redirect after fencing its owner-CPU slot.
pub(crate) fn detach_output(ptc: &Arc<PerTaskCounter>) -> AxResult<()> {
    replace_output(ptc, None)
}

fn replace_output(ptc: &Arc<PerTaskCounter>, redirect: Option<PerfRingOutput>) -> AxResult<()> {
    let restore_enabled = ptc.enabled.load(Ordering::Acquire);
    if let Err(error) = disable_counter(ptc) {
        if restore_enabled {
            ptc.enabled.store(true, Ordering::Release);
        }
        return Err(error);
    }
    match redirect {
        Some(output) => ptc.set_redirect_ring(output),
        None => ptc.detach_redirect(),
    }
    if restore_enabled && !ptc.run_state.lock().is_stopping() {
        ptc.set_enabled();
    }
    Ok(())
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
fn sideband_target(ptc: &PerTaskCounter, pid: u32, tid: u32) -> Option<SidebandTarget> {
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
    let pid = thr.proc_data.proc.pid();
    let tid = thr.tid();

    /// A target plus which record kinds it wants (so the COMM/MMAP2 loops below
    /// can each skip non-subscribers without re-walking the counter list).
    struct WantTarget {
        target: SidebandTarget,
        comm: bool,
        mmap2: bool,
    }
    // Snapshot targets, then drop the counter lock before any ring write.
    let targets: Vec<WantTarget> = {
        let counters = thr.perf_counters().lock();
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
    let curr = crate::task::current_user_task();
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
    let pid = thr.proc_data.proc.pid();
    let tid = thr.tid();
    let targets: Vec<SidebandTarget> = {
        let counters = thr.perf_counters().lock();
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
        let counters = parent_thr.perf_counters().lock();
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
        ring: Option<PerfRingOutput>,
        is_sampling: bool,
    }
    let specs: Vec<InheritSpec> = {
        let counters = parent_thr.perf_counters().lock();
        counters
            .iter()
            .filter(|p| p.inherit && !p.run_state.lock().is_stopping())
            .map(|p| InheritSpec {
                cfg: PerTaskConfig {
                    n: 0, // assigned after the slot is reserved below
                    event: p.event,
                    exclude_user: p.exclude_user,
                    exclude_kernel: p.exclude_kernel,
                    read_format: p.read_format,
                    // Follow the parent's current enable state; the child runs the
                    // monitored workload from birth, so it does not wait on exec.
                    enabled: p.enabled.load(Ordering::Acquire),
                    enable_on_exec: false,
                    cpu_filter: p.cpu_filter,
                    sample_period: p.sample_period,
                    sample_type: p.sample_type,
                    freq: p.freq,
                    target_freq: p.freq_target,
                    want_comm: p.want_comm,
                    want_mmap2: p.want_mmap2,
                    want_task: p.want_task,
                    sample_id_all: p.sample_id_all,
                    inherit: true,
                },
                sample_id: p.sample_id.load(Ordering::Relaxed),
                ring: p.inherit_ring(),
                is_sampling: p.is_sampling,
            })
            .collect()
    };
    for mut spec in specs {
        // A sampling event with no ring yet has nowhere to write the child's
        // samples; skip (perf maps the ring before enabling, so this is rare).
        if spec.is_sampling && spec.ring.is_none() {
            continue;
        }
        let Some(n) = hw::alloc_programmable_counter() else {
            warn!(
                "perf: attr.inherit skipped for child tid {} (no free PMU counter)",
                child_thr.tid()
            );
            continue;
        };
        spec.cfg.n = n;
        let child = Arc::new(PerTaskCounter::new(spec.cfg));
        // Share the parent event's id so inherited samples aggregate under it.
        child.set_sample_id(spec.sample_id);
        // Redirect the child's output into the (root) parent ring it inherited.
        if let Some(output) = spec.ring {
            child.set_redirect_ring(output);
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
    let counters = thr.perf_counters().lock().clone();
    for ptc in &counters {
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
/// Idempotence and in-flight owner CPU identity are both held by
/// [`PmuRunState`]. Either the fd side or task-exit side may win; a concurrent
/// loser observes `AlreadyClosed`.
///
/// For a *sampling* counter that is currently armed, the overflow-IRQ path is
/// torn down in the UAF-safe order before the slot/ring `Arc`s drop: stop the
/// counter, mask the IRQ, then `unregister` the [`SampleSlot`] — so the overflow
/// handler can no longer reach the ring or `notify` pointer. Only after that are
/// the [`SamplingAnchors`] dropped and the worker stopped.
pub fn free_hw(ptc: &Arc<PerTaskCounter>) {
    let close_action = ptc.run_state.lock().begin_close();
    match close_action {
        PmuCloseAction::AlreadyClosed => return,
        PmuCloseAction::Complete => {}
        PmuCloseAction::Stop(lease) => {
            if let Err(error) = cpu_worker::stop_task_counter(Arc::clone(ptc), lease) {
                // Keep the close request published and retain every
                // anchor/counter. A
                // later fd/task release retries the exact generation; leaking
                // is safer than authorizing IRQ-visible reclamation.
                warn!(
                    "perf: failed to stop counter {} on CPU {}: {error}",
                    ptc.n,
                    lease.owner().as_usize()
                );
                return;
            }
        }
    }

    if ptc.is_sampling {
        // Stop the deferred worker and drop the ring/notify anchors only after
        // owner-CPU generation removal completed. The VMA retains the ring
        // output independently, so user memory stays mapped until munmap.
        let anchors = ptc.anchors.lock().take();
        if let Some(anchors) = anchors {
            anchors.poll_alive.store(false, Ordering::Release);
            anchors.notify.notify();
        }
        // Remove the weak own-ring reference and any redirect as one value.
        // The generation-checked slot is already gone, so no hard-IRQ reader
        // can race this final task-context publication.
        ptc.output.lock().clear();
    }
    hw::free_programmable_counter(ptc.n);
    PERF_TASK_ACTIVE.fetch_sub(1, Ordering::AcqRel);
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
