use super::*;

/// A hardware counter bound to one specific task.
///
/// Interior-mutable and allocation-free so the scheduler hooks can drive it with
/// IRQs disabled. A non-sampling `CPU_CYCLES` event prefers the architectural
/// cycle counter, while all other events use programmable PMU slots. That is the
/// same counter-selection rule as Linux `armv8pmu_get_event_idx()`.
///
/// State machine (per slice):
///
/// * `enabled` — userspace wants this event counting (set at open if
///   `!disabled`, by `enable_on_exec` on exec, or by `ioctl(ENABLE)`).
/// * `run_state` — the generation-bearing owner CPU and optional sampling
///   registration for the hardware-programmed slice.
///
/// Configuring a slice resets its selected counter to 0, so its sched-out read is
/// the slice delta; [`PerTaskCounter::accumulated`] sums those deltas.
#[derive(Debug)]
pub struct PerTaskCounter {
    /// Generation-bearing scheduler identity of the task context.
    scheduler_id: ax_runtime::task::ThreadId,
    /// Physical counter reservation used while this task is scheduled.
    pub(super) counter: Counter,
    /// ARM PMUv3 event number. It is programmed only for a programmable
    /// counter; a dedicated cycle-counter reservation carries the same semantic
    /// event so an inherited child can fall back to a programmable slot.
    event: u16,
    /// `attr.exclude_user`: do not count EL0 (`PMEVTYPERn_EL0.U`).
    pub(super) exclude_user: bool,
    /// `attr.exclude_kernel`: do not count EL1 (`PMEVTYPERn_EL0.P`).
    pub(super) exclude_kernel: bool,
    /// `attr.read_format`, controlling which fields `read(perf_fd)` emits.
    read_format: u64,
    /// `attr.enable_on_exec`: start counting only when the attached task
    /// `execve`s a new image (consumed by [`on_exec`]).
    pub(super) enable_on_exec: bool,
    /// Optional Linux task-event CPU constraint (`cpu >= 0`).
    pub(super) cpu_filter: Option<PerfCpuId>,

    /// Userspace wants this event counting (see the struct-level state machine).
    pub(super) enabled: AtomicBool,
    /// Sole owner of schedule-in, schedule-out, remote stop, and close state.
    pub(super) run_state: SpinNoIrq<PmuRunState>,
    /// Sum of completed-slice deltas (raw event count).
    pub(super) accumulated: AtomicU64,
    /// Accumulated enabled time across past windows (ns).
    pub(super) time_enabled_ns: AtomicU64,
    /// Accumulated running time across past windows (ns). Equal to
    /// `time_enabled_ns` with no multiplexing.
    pub(super) time_running_ns: AtomicU64,
    /// Monotonic ns timestamp of the last [`perf_sched_in`] (live slice start).
    pub(super) last_in_ns: AtomicU64,
    /// Monotonic ns timestamp at which the event last became `enabled`.
    /// Unused for the no-multiplexing timing math but kept for parity with the
    /// system-wide path and future multiplexing accounting.
    pub(super) enabled_at_ns: AtomicU64,
    // --- Per-task sampling (`perf record -- cmd`) ---
    /// This event samples (`sample_period > 0`): the scheduler hooks arm/disarm
    /// the overflow-IRQ path each slice instead of plain counting.
    pub(super) is_sampling: bool,
    /// Sampling period (events between overflows); `0` for counting events. The
    /// counter is `preload`ed to overflow after this many events each slice. In
    /// frequency mode this is the per-slice initial estimate the handler adapts.
    pub(super) sample_period: u32,
    /// Validated scalar `attr.sample_type`.
    pub(super) sample_type: u64,
    /// Frequency mode (`attr.freq`): the overflow handler re-derives the period
    /// after each sample to converge on `freq_target` Hz. Fixed period when false.
    pub(super) freq: bool,
    /// Target sample rate (Hz) for frequency mode; `0` in fixed-period mode.
    pub(super) freq_target: u32,
    /// Unique event id emitted in `PERF_SAMPLE_ID` / `IDENTIFIER` records (set
    /// once via [`set_sample_id`](Self::set_sample_id) from the `PerfEvent`
    /// wrapper, before any scheduler hook runs); `0` until then.
    pub(super) sample_id: AtomicU64,
    /// `attr.comm`: this event wants `PERF_RECORD_COMM` side-band records.
    pub(super) want_comm: bool,
    /// `attr.mmap2`: this event wants `PERF_RECORD_MMAP2` side-band records.
    pub(super) want_mmap2: bool,
    /// `attr.task`: this event wants `PERF_RECORD_FORK` / `EXIT` side-band records.
    pub(super) want_task: bool,
    /// `attr.sample_id_all`: side-band records carry the sample-id trailer.
    pub(super) sample_id_all: bool,
    /// `attr.inherit`: clone this event onto `fork`/`clone` children (writing into
    /// the same ring) so `perf record` follows them. Driven by [`on_clone_inherit`].
    inherit: bool,
    /// Weak fd-owned family identity. The family owns members strongly, so a
    /// weak back-reference avoids a root/member cycle.
    family: SpinNoIrq<Option<FamilyBinding>>,
    /// Ensures the reserved PMU slot and global active count are reclaimed once
    /// when fd close races task exit.
    pub(super) resources: PmuResourceRelease,
    /// VMA-owned direct-read metadata for a counting event.
    rdpmc: RdpmcMapping,

    /// Coherent own-ring and redirect ownership.
    ///
    /// The own ring is weakly retained so `munmap` permits a later mmap; a
    /// redirect is strongly retained while this event can publish into it.
    /// Scheduler/sideband readers clone one complete effective output.
    pub(super) output: SpinNoIrq<PerfOutputRoute>,
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
pub(super) struct PerTaskConfig {
    /// Generation-bearing scheduler identity of the target task.
    pub(super) scheduler_id: ax_runtime::task::ThreadId,
    /// Reserved physical PMU counter.
    pub(super) counter: Counter,
    /// ARM PMUv3 event number.
    pub(super) event: u16,
    /// `attr.exclude_user`.
    pub(super) exclude_user: bool,
    /// `attr.exclude_kernel`.
    pub(super) exclude_kernel: bool,
    /// `attr.read_format`.
    pub(super) read_format: u64,
    /// Userspace-enabled at open (`attr.disabled == 0`).
    pub(super) enabled: bool,
    /// `attr.enable_on_exec`.
    pub(super) enable_on_exec: bool,
    /// Optional CPU on which this task event is eligible to run.
    pub(super) cpu_filter: Option<PerfCpuId>,
    /// Sampling period (`> 0` ⇒ sampling event); `0` ⇒ counting event. In
    /// frequency mode this is the initial estimate the overflow handler adapts.
    pub(super) sample_period: u32,
    /// `attr.sample_type` (only meaningful when `sample_period > 0`).
    pub(super) sample_type: u64,
    /// Frequency mode (`attr.freq`): the overflow handler adapts the period each
    /// slice toward `target_freq` Hz. Fixed `-c` period when false.
    pub(super) freq: bool,
    /// Target sample rate (Hz) for frequency mode; `0` in fixed-period mode.
    pub(super) target_freq: u32,
    /// `attr.comm`: emit `PERF_RECORD_COMM` side-band records (process name).
    pub(super) want_comm: bool,
    /// `attr.mmap2`: emit `PERF_RECORD_MMAP2` side-band records (executable maps).
    pub(super) want_mmap2: bool,
    /// `attr.task`: emit `PERF_RECORD_FORK` / `EXIT` side-band records.
    pub(super) want_task: bool,
    /// `attr.sample_id_all`: append the sample-id trailer to every side-band record.
    pub(super) sample_id_all: bool,
    /// `attr.inherit`: clone this event onto `fork`/`clone` children.
    pub(super) inherit: bool,
}

impl PerTaskCounter {
    /// Build a per-task counter around an already-reserved physical counter.
    ///
    /// The HW counter is *not* programmed here; it is configured + enabled lazily
    /// in [`perf_sched_in`] the next time the target task runs (or immediately
    /// from [`on_exec`] when the target is current during `execve`).
    pub(super) fn new(cfg: PerTaskConfig) -> Self {
        PerTaskCounter {
            scheduler_id: cfg.scheduler_id,
            counter: cfg.counter,
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
            rdpmc: RdpmcMapping::new(),
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

    pub(super) fn inherited_config(
        &self,
        scheduler_id: ax_runtime::task::ThreadId,
        counter: Counter,
    ) -> PerTaskConfig {
        PerTaskConfig {
            scheduler_id,
            counter,
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

    pub(super) fn programmed_event(&self) -> Option<u16> {
        self.counter.programmable_index().map(|_| self.event)
    }

    pub(super) fn programmable_index(&self) -> usize {
        self.counter
            .programmable_index()
            .expect("sampling events are validated onto programmable counters")
    }

    /// Joins event publication with the target CPU's scheduler order.
    ///
    /// The fixed worker is deliberately used even for the local CPU. If the
    /// target was already running when this event was attached or enabled, the
    /// worker wake makes it cross sched-out/sched-in; if it was not running,
    /// its first future sched-in observes the published counter directly.
    pub(super) fn synchronize_context(&self) -> AxResult<()> {
        let handle = match ax_runtime::task::thread_handle(self.scheduler_id) {
            Ok(handle) => handle,
            // Linux treats a tombstoned perf task context as already detached:
            // no owner CPU remains to synchronize, and fd-side aggregate
            // control remains a successful no-op.
            Err(ax_runtime::task::TaskError::StaleThreadId) => return Ok(()),
            Err(_) => return Err(AxError::BadState),
        };
        if handle.state() == ax_runtime::task::ThreadState::Exited {
            return Ok(());
        }
        let Some(cpu) = handle.scheduler_fence_cpu() else {
            return Ok(());
        };
        cpu_worker::synchronize_task_context(PerfCpuId::new(cpu.as_u32() as usize))
    }

    pub(super) fn rdpmc_snapshot(&self) -> RdpmcSnapshot {
        RdpmcSnapshot {
            offset: self.accumulated.load(Ordering::Acquire),
            time_enabled: self.time_enabled_ns.load(Ordering::Acquire),
            time_running: self.time_running_ns.load(Ordering::Acquire),
        }
    }

    pub(super) fn publish_rdpmc_active(&self) {
        if !self.is_sampling {
            self.rdpmc.publish_active(self.rdpmc_snapshot());
        }
    }

    pub(super) fn publish_rdpmc_inactive(&self) {
        if !self.is_sampling {
            self.rdpmc.publish_inactive(self.rdpmc_snapshot());
        }
    }

    /// Creates the one VMA-owned direct-read page for this counting event.
    pub(super) fn device_mmap_rdpmc(
        &self,
        len: usize,
    ) -> AxResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        if self.is_sampling {
            return Err(AxError::InvalidInput);
        }
        let page = self
            .rdpmc
            .install(len, self.counter, self.rdpmc_snapshot())?;
        // Close the publication-versus-sched-out race: whichever side runs
        // second republishes the completed accumulator after the weak page
        // reference is visible.
        self.publish_rdpmc_inactive();
        if let Err(error) = self.synchronize_context() {
            self.rdpmc.withdraw(&page);
            return Err(error);
        }
        Ok(mapping_result(page))
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

    pub(super) fn is_family_root(&self) -> bool {
        self.family
            .lock()
            .as_ref()
            .is_some_and(|binding| binding.root)
    }

    pub(super) fn resources_released(&self) -> bool {
        self.resources.is_released()
    }

    pub(super) fn publish_scheduler_registration(&self) -> bool {
        self.resources.publish()
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
    pub(super) fn sample_output(&self) -> Option<SampleOutput> {
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
