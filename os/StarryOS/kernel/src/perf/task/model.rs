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
    scheduler_id: ax_runtime::task::thread::ThreadId,
    /// Physical counter reservation used while this task is scheduled.
    pub(super) counter: Counter,
    /// Programmable events acquire a physical slot only for each running slice.
    pub(super) flexible: bool,
    /// Keeps deferred scheduler ticks published while this logical event may
    /// need multiplex rotation.
    _scheduler_tick_lease: Option<crate::task::PerfSchedulerTickLease>,
    /// ARM PMUv3 event number. It is programmed only for a programmable
    /// counter; a dedicated cycle-counter reservation carries the same semantic
    /// event so an inherited child can fall back to a programmable slot.
    pub(super) event: u16,
    /// `attr.exclude_user`: do not count EL0 (`PMEVTYPERn_EL0.U`).
    pub(super) exclude_user: bool,
    /// `attr.exclude_kernel`: do not count EL1 (`PMEVTYPERn_EL0.P`).
    pub(super) exclude_kernel: bool,
    /// `attr.read_format`, controlling which fields `read(perf_fd)` emits.
    pub(super) read_format: u64,
    /// `attr.enable_on_exec`: start counting only when the attached task
    /// `execve`s a new image (consumed by [`on_exec`]).
    pub(super) enable_on_exec: AtomicBool,
    /// Optional Linux task-event CPU constraint (`cpu >= 0`).
    pub(super) cpu_filter: Option<PerfCpuId>,
    /// PMU cluster selected by a cluster-specific sysfs event source.
    pub(super) required_cluster: Option<crate::perf::event_map::ClusterId>,

    /// Userspace wants this event counting (see the struct-level state machine).
    pub(super) enabled: AtomicBool,
    /// Sole owner of schedule-in, schedule-out, remote stop, and close state.
    pub(super) run_state: IrqMutex<PmuRunState>,
    /// Sum of completed-slice deltas (raw event count).
    pub(super) accumulated: AtomicU64,
    /// Greatest raw value published through `PERF_SAMPLE_READ`.
    ///
    /// A live PMU read and the completed-slice accumulator are observed through
    /// separate ownership transitions.  Keep the IRQ-visible result monotonic
    /// across a multiplex boundary, as Linux perf event counts never move
    /// backwards between samples unless userspace explicitly resets the event.
    pub(super) sample_read_floor: AtomicU64,
    /// Owner-CPU state extending the current finite-width hardware slice.
    pub(super) counting_extender: Arc<IrqMutex<super::super::counting::CounterExtender>>,
    /// Raw sampling deltas for this slice; reloads do not change its total.
    pub(super) sampling_count: Arc<sampling::SamplingCount>,
    /// Accumulated enabled time across past windows (ns).
    pub(super) time_enabled_ns: AtomicU64,
    /// Accumulated running time across past windows (ns). Equal to
    /// `time_enabled_ns` with no multiplexing.
    pub(super) time_running_ns: AtomicU64,
    /// Monotonic ns timestamp of the last [`perf_sched_in`] (live slice start).
    pub(super) last_in_ns: AtomicU64,
    /// Monotonic ns timestamp at which the enabled event's current task-context
    /// slice started. This advances `time_enabled` even when a flexible event
    /// has no physical slot, matching Linux's INACTIVE event state.
    pub(super) context_in_ns: AtomicU64,
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
    pub(super) sample_user_lr: bool,
    /// Frequency mode (`attr.freq`): the overflow handler re-derives the period
    /// after each sample to converge on `freq_target` Hz. Fixed period when false.
    pub(super) freq: bool,
    /// Target sample rate (Hz) for frequency mode; `0` in fixed-period mode.
    pub(super) freq_target: u32,
    /// Unique event id emitted in `PERF_SAMPLE_ID` / `IDENTIFIER` records (set
    /// once via [`set_sample_id`](Self::set_sample_id) from the `PerfEvent`
    /// wrapper, before any scheduler hook runs); `0` until then.
    pub(super) sample_id: AtomicU64,
    /// Concrete event identity; inherited streams differ from the primary ID.
    pub(super) stream_id: AtomicU64,
    /// Samples dropped by this source because its selected ring was full.
    loss: Arc<super::super::sampling::LossState>,
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
    /// PID namespace view captured when the root event was opened.
    pub(super) observer: PidNamespaceId,
    /// Target task identity in the event's captured PID namespace.
    pub(super) owner_ids: Option<(TgidNumber, TidNumber)>,
    group_leader: IrqMutex<Option<Weak<PerTaskCounter>>>,
    group_members: IrqMutex<Vec<Weak<PerTaskCounter>>>,
    /// Weak fd-owned family identity. The family owns members strongly, so a
    /// weak back-reference avoids a root/member cycle.
    family: IrqMutex<Option<FamilyBinding>>,
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
    pub(super) output: IrqMutex<PerfOutputRoute>,
    /// An inherited redirect targets the root event's poll worker, unlike an
    /// explicit `SET_OUTPUT` redirect whose wake ownership belongs to the target
    /// event.
    inherited_output_wake: AtomicBool,
    /// Strong notification and deferred poll machinery.
    anchors: IrqMutex<Option<SamplingAnchors>>,
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
    poll_ready: Arc<axpoll_set::PollSet>,
    /// Liveness flag for the worker; cleared on family/fd close.
    poll_alive: Arc<AtomicBool>,
}

impl SamplingAnchors {
    pub(crate) fn new(
        notify: Arc<IrqNotify>,
        poll_ready: Arc<axpoll_set::PollSet>,
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
pub(in crate::perf) struct PerTaskConfig {
    /// Inherited output charges losses to the root event, as on Linux.
    pub(in crate::perf) loss: Arc<sampling::LossState>,
    /// Generation-bearing scheduler identity of the target task.
    pub(in crate::perf) scheduler_id: ax_runtime::task::thread::ThreadId,
    /// Reserved physical PMU counter.
    pub(in crate::perf) counter: Counter,
    pub(in crate::perf) flexible: bool,
    pub(in crate::perf) scheduler_tick_lease: Option<crate::task::PerfSchedulerTickLease>,
    /// ARM PMUv3 event number.
    pub(in crate::perf) event: u16,
    /// `attr.exclude_user`.
    pub(in crate::perf) exclude_user: bool,
    /// `attr.exclude_kernel`.
    pub(in crate::perf) exclude_kernel: bool,
    /// `attr.read_format`.
    pub(in crate::perf) read_format: u64,
    /// Userspace-enabled at open (`attr.disabled == 0`).
    pub(in crate::perf) enabled: bool,
    /// `attr.enable_on_exec`.
    pub(in crate::perf) enable_on_exec: bool,
    /// Optional CPU on which this task event is eligible to run.
    pub(in crate::perf) cpu_filter: Option<PerfCpuId>,
    /// PMU cluster selected by a cluster-specific sysfs event source.
    pub(in crate::perf) required_cluster: Option<crate::perf::event_map::ClusterId>,
    /// Sampling period (`> 0` ⇒ sampling event); `0` ⇒ counting event. In
    /// frequency mode this is the initial estimate the overflow handler adapts.
    pub(in crate::perf) sample_period: u32,
    /// `attr.sample_type` (only meaningful when `sample_period > 0`).
    pub(in crate::perf) sample_type: u64,
    /// Capture the saved user LR for PERF_SAMPLE_REGS_USER.
    pub(in crate::perf) sample_user_lr: bool,
    /// Frequency mode (`attr.freq`): the overflow handler adapts the period each
    /// slice toward `target_freq` Hz. Fixed `-c` period when false.
    pub(in crate::perf) freq: bool,
    /// Target sample rate (Hz) for frequency mode; `0` in fixed-period mode.
    pub(in crate::perf) target_freq: u32,
    /// `attr.comm`: emit `PERF_RECORD_COMM` side-band records (process name).
    pub(in crate::perf) want_comm: bool,
    /// `attr.mmap2`: emit `PERF_RECORD_MMAP2` side-band records (executable maps).
    pub(in crate::perf) want_mmap2: bool,
    /// `attr.task`: emit `PERF_RECORD_FORK` / `EXIT` side-band records.
    pub(in crate::perf) want_task: bool,
    /// `attr.sample_id_all`: append the sample-id trailer to every side-band record.
    pub(in crate::perf) sample_id_all: bool,
    /// `attr.inherit`: clone this event onto `fork`/`clone` children.
    pub(in crate::perf) inherit: bool,
    /// PID namespace view captured when the root event was opened.
    pub(in crate::perf) observer: PidNamespaceId,
    pub(in crate::perf) owner_ids: Option<(TgidNumber, TidNumber)>,
}

impl PerTaskCounter {
    /// Build a per-task counter around an already-reserved physical counter.
    ///
    /// The HW counter is *not* programmed here; it is configured + enabled lazily
    /// in [`perf_sched_in`] the next time the target task runs (or immediately
    /// from [`on_exec`] when the target is current during `execve`).
    pub(in crate::perf) fn new(cfg: PerTaskConfig) -> Self {
        PerTaskCounter {
            scheduler_id: cfg.scheduler_id,
            counter: cfg.counter,
            flexible: cfg.flexible,
            _scheduler_tick_lease: cfg.scheduler_tick_lease,
            event: cfg.event,
            exclude_user: cfg.exclude_user,
            exclude_kernel: cfg.exclude_kernel,
            read_format: cfg.read_format,
            enable_on_exec: AtomicBool::new(cfg.enable_on_exec),
            cpu_filter: cfg.cpu_filter,
            required_cluster: cfg.required_cluster,
            enabled: AtomicBool::new(cfg.enabled),
            run_state: IrqMutex::new(PmuRunState::new()),
            accumulated: AtomicU64::new(0),
            sampling_count: Arc::new(sampling::SamplingCount::new()),
            sample_read_floor: AtomicU64::new(0),
            counting_extender: Arc::new(IrqMutex::new(
                super::super::counting::CounterExtender::new(),
            )),
            time_enabled_ns: AtomicU64::new(0),
            time_running_ns: AtomicU64::new(0),
            last_in_ns: AtomicU64::new(0),
            context_in_ns: AtomicU64::new(0),
            is_sampling: cfg.sample_period > 0,
            sample_period: cfg.sample_period,
            sample_type: cfg.sample_type,
            sample_user_lr: cfg.sample_user_lr,
            freq: cfg.freq,
            freq_target: cfg.target_freq,
            sample_id: AtomicU64::new(0),
            stream_id: AtomicU64::new(0),
            loss: cfg.loss,
            want_comm: cfg.want_comm,
            want_mmap2: cfg.want_mmap2,
            want_task: cfg.want_task,
            sample_id_all: cfg.sample_id_all,
            inherit: cfg.inherit,
            observer: cfg.observer,
            owner_ids: cfg.owner_ids,
            group_leader: IrqMutex::new(None),
            group_members: IrqMutex::new(Vec::new()),
            family: IrqMutex::new(None),
            resources: PmuResourceRelease::new(),
            rdpmc: RdpmcMapping::new(),
            output: IrqMutex::new(PerfOutputRoute::new()),
            inherited_output_wake: AtomicBool::new(false),
            anchors: IrqMutex::new(None),
        }
    }

    /// `attr.read_format` for serializing `read(perf_fd)`.
    pub fn read_format(&self) -> u64 {
        self.read_format
    }

    pub(in crate::perf) fn is_flexible(&self) -> bool {
        self.flexible
    }

    /// PID namespace used to expose this event's task identity to userspace.
    pub(in crate::perf) fn observer(&self) -> PidNamespaceId {
        self.observer
    }

    /// Record the unique event id for `PERF_SAMPLE_ID` / `IDENTIFIER`. Called
    /// once at open (before the scheduler hooks run), so a relaxed store suffices.
    pub fn set_sample_id(&self, id: u64) {
        self.sample_id.store(id, Ordering::Relaxed);
        self.stream_id.store(id, Ordering::Relaxed);
    }

    /// Initializes an inherited event before it is published to the scheduler.
    pub(in crate::perf) fn set_inherited_sample_id(&self, primary_id: u64) {
        self.sample_id.store(primary_id, Ordering::Relaxed);
        self.stream_id
            .store(super::super::allocate_event_id(), Ordering::Relaxed);
    }

    pub(in crate::perf) fn inherited_config(
        &self,
        scheduler_id: ax_runtime::task::thread::ThreadId,
        counter: Counter,
        scheduler_tick_lease: Option<crate::task::PerfSchedulerTickLease>,
        owner_ids: Option<(TgidNumber, TidNumber)>,
    ) -> PerTaskConfig {
        PerTaskConfig {
            loss: Arc::clone(&self.loss),
            scheduler_id,
            counter,
            // Every inherited copy obtains its own per-CPU reservation. The
            // parent's fixed cycle/programmable reservation cannot be shared.
            flexible: true,
            scheduler_tick_lease,
            event: self.event,
            exclude_user: self.exclude_user,
            exclude_kernel: self.exclude_kernel,
            read_format: self.read_format,
            // Registration under the family relation lock publishes the current
            // root-fd control intent before the child becomes schedulable.
            enabled: false,
            enable_on_exec: self.enable_on_exec.load(Ordering::Acquire),
            cpu_filter: self.cpu_filter,
            required_cluster: self.required_cluster,
            sample_period: self.sample_period,
            sample_type: self.sample_type,
            sample_user_lr: self.sample_user_lr,
            freq: self.freq,
            target_freq: self.freq_target,
            want_comm: self.want_comm,
            want_mmap2: self.want_mmap2,
            want_task: self.want_task,
            sample_id_all: self.sample_id_all,
            inherit: true,
            observer: self.observer,
            owner_ids,
        }
    }

    pub(super) fn programmed_event(&self, counter: Counter) -> Option<u16> {
        counter.programmable_index().map(|_| self.event)
    }

    pub(super) fn reset_counting_slice(&self, counter: Counter) {
        self.counting_extender.lock().reset();
        if let Some(index) = counter.programmable_index() {
            crate::perf::hw_owner::on_pmu(|pmu| pmu.clear_overflow(1u64 << index));
        }
    }

    pub(super) fn read_counting_slice(&self, counter: Counter) -> u64 {
        let mut extender = self.counting_extender.lock();
        if let Some(index) = counter.programmable_index() {
            let bit = 1 << index;
            if (crate::perf::hw_owner::on_pmu(|pmu| pmu.overflow_status()) as u32) & bit != 0 {
                crate::perf::hw_owner::on_pmu(|pmu| pmu.clear_overflow(u64::from(bit)));
                extender.record_overflow();
            }
        }
        let (_, width) = counter.mmap_metadata();
        extender.value(counter.read(), width)
    }

    /// Joins event publication with the target CPU's scheduler order.
    ///
    /// The fixed worker is deliberately used even for the local CPU. If the
    /// target was already running when this event was attached or enabled, the
    /// worker wake makes it cross sched-out/sched-in; if it was not running,
    /// its first future sched-in observes the published counter directly.
    pub(in crate::perf) fn synchronize_context(&self) -> crate::StarryResult<()> {
        let handle = match ax_runtime::task::thread::ThreadHandle::lookup(self.scheduler_id) {
            Ok(handle) => handle,
            // Linux treats a tombstoned perf task context as already detached:
            // no owner CPU remains to synchronize, and fd-side aggregate
            // control remains a successful no-op.
            Err(ax_runtime::task::thread::TaskError::StaleThreadId) => return Ok(()),
            Err(_) => return Err(crate::StarryError::BadState),
        };
        if handle.state() == ax_runtime::task::thread::ThreadState::Exited {
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
    pub(in crate::perf) fn device_mmap_rdpmc(
        &self,
        len: usize,
    ) -> crate::StarryResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        if self.is_sampling {
            return Err(crate::StarryError::InvalidInput);
        }
        if self.flexible {
            return Err(crate::StarryError::Unsupported);
        }
        let page = self.rdpmc.install(len, self.rdpmc_snapshot())?;
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
        self.enabled.store(true, Ordering::Release);
    }

    pub(super) fn begin_enabled_context(&self, now: u64) {
        let _ = self
            .context_in_ns
            .compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire);
    }

    pub(super) fn finish_enabled_context(&self, now: u64) {
        let since = self.context_in_ns.swap(0, Ordering::AcqRel);
        if since != 0 {
            self.time_enabled_ns
                .fetch_add(now.saturating_sub(since), Ordering::AcqRel);
        }
    }

    pub(super) fn live_enabled_time(&self, now: u64) -> u64 {
        let since = self.context_in_ns.load(Ordering::Acquire);
        if since == 0 {
            0
        } else {
            now.saturating_sub(since)
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

    pub(in crate::perf) fn resources_released(&self) -> bool {
        self.resources.is_released()
    }

    pub(in crate::perf) fn publish_scheduler_registration(&self) -> bool {
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

    pub(in crate::perf) fn wants_comm(&self) -> bool {
        self.want_comm
    }

    pub(in crate::perf) fn wants_mmap2(&self) -> bool {
        self.want_mmap2
    }

    pub(in crate::perf) fn wants_task(&self) -> bool {
        self.want_task
    }

    pub(in crate::perf) fn inheritable(&self) -> bool {
        self.inherit && !self.run_state.lock().is_stopping()
    }

    pub(in crate::perf) fn enabled_for_inheritance(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub(in crate::perf) fn sample_id(&self) -> u64 {
        self.sample_id.load(Ordering::Relaxed)
    }

    pub(in crate::perf) fn lost_samples(&self) -> u64 {
        self.loss.total()
    }

    fn sample_read_entry(self: &Arc<Self>) -> SampleReadEntry {
        SampleReadEntry::owned(Arc::clone(self), per_task_sample_read_irq, self.sample_id())
    }

    pub(in crate::perf) fn link_group(
        leader: &Arc<Self>,
        member: &Arc<Self>,
    ) -> crate::StarryResult<()> {
        if leader.scheduler_id != member.scheduler_id
            || leader.cpu_filter != member.cpu_filter
            || leader.required_cluster != member.required_cluster
        {
            return Err(crate::StarryError::InvalidInput);
        }
        let mut members = leader.group_members.lock();
        members.retain(|member| member.strong_count() != 0);
        if members.len() + 1 >= MAX_SAMPLE_READ_EVENTS {
            return Err(crate::StarryError::InvalidInput);
        }
        *member.group_leader.lock() = Some(Arc::downgrade(leader));
        members.push(Arc::downgrade(member));
        Ok(())
    }

    pub(in crate::perf) fn live_group_leader(&self) -> Option<Arc<Self>> {
        self.group_leader
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
            .filter(|leader| !leader.resources_released())
    }

    pub(super) fn sample_read_entries(
        self: &Arc<Self>,
    ) -> ([SampleReadEntry; MAX_SAMPLE_READ_EVENTS], u8) {
        let mut entries = [const { SampleReadEntry::EMPTY }; MAX_SAMPLE_READ_EVENTS];
        if self.read_format & super::super::PERF_FORMAT_GROUP == 0 {
            entries[0] = self.sample_read_entry();
            return (entries, 1);
        }
        let leader = self.live_group_leader();
        let leader = leader.as_ref().unwrap_or(self);
        entries[0] = leader.sample_read_entry();
        let mut len = 1;
        for member in leader.group_members.lock().iter().filter_map(Weak::upgrade) {
            if member.resources_released() {
                continue;
            }
            if len == MAX_SAMPLE_READ_EVENTS {
                break;
            }
            entries[len] = member.sample_read_entry();
            len += 1;
        }
        (entries, len as u8)
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

    /// Expose the effective ring for a `PERF_EVENT_IOC_SET_OUTPUT` redirect
    /// target, following an existing redirect chain.
    pub(crate) fn output_ring(&self) -> Option<PerfRingOutput> {
        self.output.lock().effective_output()
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
        Some(SampleOutput::new(
            Some(ring),
            notify,
            Arc::clone(&self.loss),
        ))
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
    pub unsafe fn register_poll_shared(&self, sink: &mut dyn axpoll::SharedRegistrationSink) {
        let guard = self.anchors.lock();
        if let Some(anchors) = guard.as_ref() {
            unsafe { sink.register_shared(&anchors.poll_ready, axpoll::IoEvents::IN) };
        }
    }

    pub unsafe fn register_poll_exclusive(&self, sink: &mut dyn axpoll::ExclusiveRegistrationSink) {
        let guard = self.anchors.lock();
        if let Some(anchors) = guard.as_ref() {
            unsafe { sink.register_exclusive(&anchors.poll_ready, axpoll::IoEvents::IN) };
        }
    }
}

unsafe fn per_task_sample_read_irq(
    context: *const (),
    _source_slot: usize,
    now: u64,
) -> SampleReadValue {
    // SAFETY: task context ownership keeps the counter alive until its sampling
    // registration has been synchronously removed.
    let counter = unsafe { &*context.cast::<PerTaskCounter>() };
    // Retain the generation lock through the physical read, not merely while
    // copying the lease: its slot and sampling baseline must describe the
    // same scheduling generation throughout the snapshot.
    let run_state = counter.run_state.lock();
    let mut value = counter.accumulated.load(Ordering::Acquire);
    let running = run_state.running();
    if let Some(lease) = running
        && lease.owner().as_usize() == ax_hal::percpu::this_cpu_id()
    {
        let physical = lease.counter();
        let live = if counter.is_sampling {
            counter
                .sampling_count
                .update(physical.programmable_index().expect("sampling slot"))
        } else {
            counter.read_counting_slice(physical)
        };
        value = value.saturating_add(live);
    }
    let time_enabled = counter
        .time_enabled_ns
        .load(Ordering::Acquire)
        .saturating_add(counter.live_enabled_time(now));
    let mut time_running = counter.time_running_ns.load(Ordering::Acquire);
    if running.is_some() {
        let elapsed = now.saturating_sub(counter.last_in_ns.load(Ordering::Acquire));
        time_running = time_running.saturating_add(elapsed);
    }
    let previous = counter.sample_read_floor.fetch_max(value, Ordering::AcqRel);
    value = value.max(previous);
    SampleReadValue {
        value,
        time_enabled,
        time_running,
        lost: counter.loss.total(),
    }
}
