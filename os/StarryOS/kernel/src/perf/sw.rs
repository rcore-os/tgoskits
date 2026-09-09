//! Linux-compatible counting for the five core `PERF_TYPE_SOFTWARE` events.
//!
//! Task events are attached to one generation-stable thread identity and are
//! driven from scheduler and page-fault hooks. CPU-wide events live in a
//! separate registry and are charged only by hooks executing on their target
//! CPU. Inherited task bindings keep slice-local scheduling state while sharing
//! the aggregate count owned by the original event.

use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use ax_lazyinit::LazyInit;
use axpoll::{IoEvents, Pollable};
use kbpf_basic::linux_bpf::{perf_event_attr, perf_sw_ids};

use super::{PerfEventOps, PerfReadValues, access::AuthorizedPerfTarget};
use crate::{
    StarryError, StarryResult,
    sync::IrqMutex,
    task::{PidIdentityId, Thread},
};

/// Number of live software events. Hot-path hooks return after one atomic load
/// when no task or CPU software event exists.
static PERF_SW_ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Sentinel used before a task has run while software accounting is active.
pub(crate) const CPU_UNSET: u32 = u32::MAX;

static SYSTEM_COUNTERS: LazyInit<IrqMutex<Vec<Arc<SwSystemCounter>>>> = LazyInit::new();

#[inline]
fn now_ns() -> u64 {
    ax_runtime::hal::time::monotonic_time_nanos()
}

/// The software events implemented by this backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SwId {
    CpuClock,
    TaskClock,
    PageFaults,
    ContextSwitches,
    CpuMigrations,
}

impl SwId {
    fn from_raw(id: perf_sw_ids) -> Option<Self> {
        Some(match id {
            perf_sw_ids::PERF_COUNT_SW_CPU_CLOCK => Self::CpuClock,
            perf_sw_ids::PERF_COUNT_SW_TASK_CLOCK => Self::TaskClock,
            perf_sw_ids::PERF_COUNT_SW_PAGE_FAULTS => Self::PageFaults,
            perf_sw_ids::PERF_COUNT_SW_CONTEXT_SWITCHES => Self::ContextSwitches,
            perf_sw_ids::PERF_COUNT_SW_CPU_MIGRATIONS => Self::CpuMigrations,
            _ => return None,
        })
    }

    const fn is_clock(self) -> bool {
        matches!(self, Self::CpuClock | Self::TaskClock)
    }
}

/// Returns whether the software id has real counter semantics here. Other
/// software ids, notably `PERF_COUNT_SW_DUMMY`, remain on the BPF/tracking path.
pub fn is_counting_sw(id: perf_sw_ids) -> bool {
    SwId::from_raw(id).is_some()
}

/// Linux's non-counting tracking event used to carry side-band records.
pub fn is_tracking_dummy(id: perf_sw_ids) -> bool {
    id == perf_sw_ids::PERF_COUNT_SW_DUMMY
}

/// Aggregate state owned by one perf event and shared with inherited task
/// bindings. Scheduling-window state deliberately stays in each binding.
#[derive(Debug)]
struct SwEventState {
    kind: SwId,
    read_format: u64,
    inherit: bool,
    dead: AtomicBool,
    count: AtomicU64,
    runtime_ns: AtomicU64,
    time_enabled_ns: AtomicU64,
    reset_epoch: AtomicU64,
}

impl SwEventState {
    fn new(kind: SwId, attr: &perf_event_attr) -> Self {
        Self {
            kind,
            read_format: attr.read_format,
            inherit: attr.inherit() != 0,
            dead: AtomicBool::new(false),
            count: AtomicU64::new(0),
            runtime_ns: AtomicU64::new(0),
            time_enabled_ns: AtomicU64::new(0),
            reset_epoch: AtomicU64::new(1),
        }
    }

    fn reset(&self) -> u64 {
        let epoch = self.reset_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.count.store(0, Ordering::Release);
        self.runtime_ns.store(0, Ordering::Release);
        self.time_enabled_ns.store(0, Ordering::Release);
        epoch
    }
}

/// Slice-local state for an event attached to one task. An inherited child gets
/// a new instance so two tasks never race over `run_since_ns` or CPU history.
#[derive(Debug)]
pub struct SwPerTaskCounter {
    state: Arc<SwEventState>,
    owner: PidIdentityId,
    cpu_filter: Option<usize>,
    enabled: AtomicBool,
    enable_on_exec: AtomicBool,
    retired: AtomicBool,
    enabled_since_ns: AtomicU64,
    run_since_ns: AtomicU64,
    epoch: AtomicU64,
    /// A sibling keeps only weak ownership of its leader. Closing the leader
    /// therefore makes the sibling standalone instead of creating a cycle.
    group_leader: IrqMutex<Option<Weak<SwPerTaskCounter>>>,
    /// The leader owns no sibling; scheduler-visible task bindings retain them.
    group_members: IrqMutex<Vec<Weak<SwPerTaskCounter>>>,
}

impl SwPerTaskCounter {
    fn new(
        state: Arc<SwEventState>,
        owner: PidIdentityId,
        cpu_filter: Option<usize>,
        enabled: bool,
        enable_on_exec: bool,
    ) -> Self {
        let now = now_ns();
        Self {
            epoch: AtomicU64::new(state.reset_epoch.load(Ordering::Acquire)),
            state,
            owner,
            cpu_filter,
            enabled: AtomicBool::new(enabled),
            enable_on_exec: AtomicBool::new(enable_on_exec),
            retired: AtomicBool::new(false),
            enabled_since_ns: AtomicU64::new(if enabled { now } else { 0 }),
            run_since_ns: AtomicU64::new(0),
            group_leader: IrqMutex::new(None),
            group_members: IrqMutex::new(Vec::new()),
        }
    }

    fn clone_for(&self, child: &Thread) -> Arc<Self> {
        Arc::new(Self::new(
            self.state.clone(),
            child.pid_identity().id(),
            self.cpu_filter,
            self.enabled.load(Ordering::Acquire),
            self.enable_on_exec.load(Ordering::Acquire),
        ))
    }

    fn accepts_cpu(&self, cpu: usize) -> bool {
        self.cpu_filter.is_none_or(|filter| filter == cpu)
    }

    fn live_group_leader(&self) -> Option<Arc<Self>> {
        self.group_leader
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
            .filter(|leader| !leader.state.dead.load(Ordering::Acquire))
    }

    fn is_effectively_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
            && self
                .live_group_leader()
                .is_none_or(|leader| leader.enabled.load(Ordering::Acquire))
    }

    fn live_group_members(&self) -> Vec<Arc<Self>> {
        let mut members = self.group_members.lock();
        let live = members
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|member| !member.state.dead.load(Ordering::Acquire))
            .collect::<Vec<_>>();
        members.retain(|member| {
            member
                .upgrade()
                .is_some_and(|member| !member.state.dead.load(Ordering::Acquire))
        });
        live
    }

    fn synchronize_epoch(&self, now: u64) {
        let epoch = self.state.reset_epoch.load(Ordering::Acquire);
        if self.epoch.swap(epoch, Ordering::AcqRel) != epoch {
            self.run_since_ns.store(0, Ordering::Release);
            if self.is_effectively_enabled() {
                self.enabled_since_ns.store(now, Ordering::Release);
            } else {
                self.enabled_since_ns.store(0, Ordering::Release);
            }
        }
    }

    fn start_slice(&self, now: u64, cpu: usize) {
        self.synchronize_epoch(now);
        if !self.retired.load(Ordering::Acquire)
            && !self.state.dead.load(Ordering::Acquire)
            && self.is_effectively_enabled()
            && self.accepts_cpu(cpu)
        {
            let _ = self
                .run_since_ns
                .compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire);
        }
    }

    fn close_slice(&self, now: u64) {
        let since = self.run_since_ns.swap(0, Ordering::AcqRel);
        if since != 0
            && self.epoch.load(Ordering::Acquire) == self.state.reset_epoch.load(Ordering::Acquire)
        {
            self.state
                .runtime_ns
                .fetch_add(now.saturating_sub(since), Ordering::AcqRel);
        }
    }

    fn enable_at(&self, now: u64) -> bool {
        if !self.enabled.swap(true, Ordering::AcqRel) {
            self.synchronize_epoch(now);
            if self.is_effectively_enabled() {
                self.enabled_since_ns.store(now, Ordering::Release);
                self.arm_if_current(now);
            }
            true
        } else {
            false
        }
    }

    fn disable_at(&self, now: u64) -> bool {
        if self.enabled.swap(false, Ordering::AcqRel) {
            self.close_slice(now);
            self.close_enabled_window(now);
            true
        } else {
            false
        }
    }

    fn close_enabled_window(&self, now: u64) {
        let since = self.enabled_since_ns.swap(0, Ordering::AcqRel);
        if since != 0 {
            self.state
                .time_enabled_ns
                .fetch_add(now.saturating_sub(since), Ordering::AcqRel);
        }
    }

    fn pause_for_group(&self, now: u64) {
        if self.enabled.load(Ordering::Acquire) {
            self.close_slice(now);
            self.close_enabled_window(now);
        }
    }

    fn resume_for_group(&self, now: u64) {
        if !self.retired.load(Ordering::Acquire)
            && !self.state.dead.load(Ordering::Acquire)
            && self.is_effectively_enabled()
        {
            self.synchronize_epoch(now);
            let _ =
                self.enabled_since_ns
                    .compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire);
            self.arm_if_current(now);
        }
    }

    fn set_enabled(&self) {
        let now = now_ns();
        if self.enable_at(now) && self.live_group_leader().is_none() {
            for member in self.live_group_members() {
                member.resume_for_group(now);
            }
        }
    }

    fn set_disabled(&self) {
        let now = now_ns();
        let is_group_root = self.live_group_leader().is_none();
        if self.disable_at(now) && is_group_root {
            for member in self.live_group_members() {
                member.pause_for_group(now);
            }
        }
    }

    fn arm_if_current(&self, now: u64) {
        let _guard = crate::sync::PreemptGuard::new();
        let Ok(Some(current)) = crate::task::try_current_user_task() else {
            return;
        };
        let thread = current.as_thread();
        if thread.pid_identity().id() == self.owner {
            self.start_slice(now, ax_hal::percpu::this_cpu_id());
        }
    }

    fn reset(&self) {
        let now = now_ns();
        let epoch = self.state.reset();
        self.epoch.store(epoch, Ordering::Release);
        self.run_since_ns.store(0, Ordering::Release);
        if self.is_effectively_enabled() {
            self.enabled_since_ns.store(now, Ordering::Release);
            self.arm_if_current(now);
        } else {
            self.enabled_since_ns.store(0, Ordering::Release);
        }
    }

    fn snapshot(&self) -> PerfReadValues {
        let now = now_ns();
        let enabled = self.is_effectively_enabled();
        let enabled_since = self.enabled_since_ns.load(Ordering::Acquire);
        let live_enabled = if enabled && enabled_since != 0 {
            now.saturating_sub(enabled_since)
        } else {
            0
        };
        let run_since = self.run_since_ns.load(Ordering::Acquire);
        let live_runtime = if enabled && run_since != 0 {
            now.saturating_sub(run_since)
        } else {
            0
        };
        let runtime = self.state.runtime_ns.load(Ordering::Acquire) + live_runtime;
        PerfReadValues {
            value: if self.state.kind.is_clock() {
                runtime
            } else {
                self.state.count.load(Ordering::Acquire)
            },
            time_enabled: self.state.time_enabled_ns.load(Ordering::Acquire) + live_enabled,
            time_running: runtime,
            lost: 0,
            read_format: self.state.read_format,
        }
    }

    fn retire(&self) {
        if !self.retired.swap(true, Ordering::AcqRel) {
            self.set_disabled();
        }
    }

    fn link_group(leader: &Arc<Self>, member: &Arc<Self>) -> StarryResult<()> {
        Self::link_group_binding(leader, member, true)
    }

    fn link_inherited_group(leader: &Arc<Self>, member: &Arc<Self>) -> StarryResult<()> {
        Self::link_group_binding(leader, member, false)
    }

    fn link_group_binding(
        leader: &Arc<Self>,
        member: &Arc<Self>,
        reset_new_event: bool,
    ) -> StarryResult<()> {
        if leader.owner != member.owner
            || leader.cpu_filter != member.cpu_filter
            || leader.state.dead.load(Ordering::Acquire)
            || member.state.dead.load(Ordering::Acquire)
        {
            return Err(StarryError::InvalidInput);
        }

        let now = now_ns();
        member.run_since_ns.store(0, Ordering::Release);
        member.enabled_since_ns.store(0, Ordering::Release);
        if reset_new_event {
            let epoch = member.state.reset();
            member.epoch.store(epoch, Ordering::Release);
        }
        *member.group_leader.lock() = Some(Arc::downgrade(leader));
        let mut members = leader.group_members.lock();
        members.retain(|entry| {
            entry
                .upgrade()
                .is_some_and(|event| !event.state.dead.load(Ordering::Acquire))
        });
        members.push(Arc::downgrade(member));
        drop(members);
        if leader.enabled.load(Ordering::Acquire) {
            member.resume_for_group(now);
        }
        Ok(())
    }

    fn detach_group_members(leader: &Arc<Self>) {
        let members = core::mem::take(&mut *leader.group_members.lock());
        let weak_leader = Arc::downgrade(leader);
        let now = now_ns();
        for member in members.into_iter().filter_map(|member| member.upgrade()) {
            let mut group_leader = member.group_leader.lock();
            let attached = group_leader
                .as_ref()
                .is_some_and(|owner| Weak::ptr_eq(owner, &weak_leader));
            if attached {
                *group_leader = None;
            }
            drop(group_leader);
            if attached {
                member.resume_for_group(now);
            }
        }
    }
}

/// One CPU-wide software event. Its CPU perf context runs continuously while
/// enabled, so `time_running == time_enabled` as in Linux.
#[derive(Debug)]
struct SwSystemCounter {
    state: Arc<SwEventState>,
    cpu: usize,
    enabled: AtomicBool,
    enabled_since_ns: AtomicU64,
    group_leader: IrqMutex<Option<Weak<SwSystemCounter>>>,
    group_members: IrqMutex<Vec<Weak<SwSystemCounter>>>,
}

impl SwSystemCounter {
    fn new(state: Arc<SwEventState>, cpu: usize, enabled: bool) -> Self {
        Self {
            state,
            cpu,
            enabled: AtomicBool::new(enabled),
            enabled_since_ns: AtomicU64::new(if enabled { now_ns() } else { 0 }),
            group_leader: IrqMutex::new(None),
            group_members: IrqMutex::new(Vec::new()),
        }
    }

    fn live_group_leader(&self) -> Option<Arc<Self>> {
        self.group_leader
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
            .filter(|leader| !leader.state.dead.load(Ordering::Acquire))
    }

    fn is_effectively_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
            && self
                .live_group_leader()
                .is_none_or(|leader| leader.enabled.load(Ordering::Acquire))
    }

    fn live_group_members(&self) -> Vec<Arc<Self>> {
        let mut members = self.group_members.lock();
        let live = members
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|member| !member.state.dead.load(Ordering::Acquire))
            .collect::<Vec<_>>();
        members.retain(|member| {
            member
                .upgrade()
                .is_some_and(|member| !member.state.dead.load(Ordering::Acquire))
        });
        live
    }

    fn enable_at(&self, now: u64) -> bool {
        if !self.enabled.swap(true, Ordering::AcqRel) {
            if self.is_effectively_enabled() {
                self.enabled_since_ns.store(now, Ordering::Release);
            }
            true
        } else {
            false
        }
    }

    fn disable_at(&self, now: u64) -> bool {
        if self.enabled.swap(false, Ordering::AcqRel) {
            self.close_enabled_window(now);
            true
        } else {
            false
        }
    }

    fn close_enabled_window(&self, now: u64) {
        let since = self.enabled_since_ns.swap(0, Ordering::AcqRel);
        if since != 0 {
            self.state
                .time_enabled_ns
                .fetch_add(now.saturating_sub(since), Ordering::AcqRel);
        }
    }

    fn pause_for_group(&self, now: u64) {
        if self.enabled.load(Ordering::Acquire) {
            self.close_enabled_window(now);
        }
    }

    fn resume_for_group(&self, now: u64) {
        if !self.state.dead.load(Ordering::Acquire) && self.is_effectively_enabled() {
            let _ =
                self.enabled_since_ns
                    .compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire);
        }
    }

    fn set_enabled(&self) {
        let now = now_ns();
        if self.enable_at(now) && self.live_group_leader().is_none() {
            for member in self.live_group_members() {
                member.resume_for_group(now);
            }
        }
    }

    fn set_disabled(&self) {
        let now = now_ns();
        let is_group_root = self.live_group_leader().is_none();
        if self.disable_at(now) && is_group_root {
            for member in self.live_group_members() {
                member.pause_for_group(now);
            }
        }
    }

    fn reset(&self) {
        self.state.reset();
        if self.is_effectively_enabled() {
            self.enabled_since_ns.store(now_ns(), Ordering::Release);
        } else {
            self.enabled_since_ns.store(0, Ordering::Release);
        }
    }

    fn enabled_time(&self) -> u64 {
        let enabled_since = self.enabled_since_ns.load(Ordering::Acquire);
        self.state.time_enabled_ns.load(Ordering::Acquire)
            + if self.is_effectively_enabled() && enabled_since != 0 {
                now_ns().saturating_sub(enabled_since)
            } else {
                0
            }
    }

    fn snapshot(&self) -> PerfReadValues {
        let time = self.enabled_time();
        PerfReadValues {
            value: if self.state.kind.is_clock() {
                time
            } else {
                self.state.count.load(Ordering::Acquire)
            },
            time_enabled: time,
            time_running: time,
            lost: 0,
            read_format: self.state.read_format,
        }
    }

    fn add_discrete(&self, kind: SwId) {
        if self.cpu == ax_hal::percpu::this_cpu_id()
            && self.state.kind == kind
            && !self.state.dead.load(Ordering::Acquire)
            && self.is_effectively_enabled()
        {
            self.state.count.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn link_group(leader: &Arc<Self>, member: &Arc<Self>) -> StarryResult<()> {
        if leader.cpu != member.cpu
            || leader.state.dead.load(Ordering::Acquire)
            || member.state.dead.load(Ordering::Acquire)
        {
            return Err(StarryError::InvalidInput);
        }

        let now = now_ns();
        member.enabled_since_ns.store(0, Ordering::Release);
        member.state.reset();
        *member.group_leader.lock() = Some(Arc::downgrade(leader));
        let mut members = leader.group_members.lock();
        members.retain(|entry| {
            entry
                .upgrade()
                .is_some_and(|event| !event.state.dead.load(Ordering::Acquire))
        });
        members.push(Arc::downgrade(member));
        drop(members);
        if leader.enabled.load(Ordering::Acquire) {
            member.resume_for_group(now);
        }
        Ok(())
    }

    fn detach_group_members(leader: &Arc<Self>) {
        let members = core::mem::take(&mut *leader.group_members.lock());
        let weak_leader = Arc::downgrade(leader);
        let now = now_ns();
        for member in members.into_iter().filter_map(|member| member.upgrade()) {
            let mut group_leader = member.group_leader.lock();
            let attached = group_leader
                .as_ref()
                .is_some_and(|owner| Weak::ptr_eq(owner, &weak_leader));
            if attached {
                *group_leader = None;
            }
            drop(group_leader);
            if attached {
                member.resume_for_group(now);
            }
        }
    }
}

#[derive(Debug)]
enum SwTargetCounter {
    Task(Arc<SwPerTaskCounter>),
    Cpu(Arc<SwSystemCounter>),
}

/// File backend for one software counting event.
#[derive(Debug)]
pub struct SwPerfEvent {
    state: Arc<SwEventState>,
    target: SwTargetCounter,
}

impl Drop for SwPerfEvent {
    fn drop(&mut self) {
        if !self.state.dead.swap(true, Ordering::AcqRel) {
            match &self.target {
                SwTargetCounter::Task(counter) => SwPerTaskCounter::detach_group_members(counter),
                SwTargetCounter::Cpu(counter) => SwSystemCounter::detach_group_members(counter),
            }
            PERF_SW_ACTIVE.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl PerfEventOps for SwPerfEvent {
    fn enable(&mut self) -> StarryResult<()> {
        match &self.target {
            SwTargetCounter::Task(counter) => counter.set_enabled(),
            SwTargetCounter::Cpu(counter) => counter.set_enabled(),
        }
        Ok(())
    }

    fn disable(&mut self) -> StarryResult<()> {
        match &self.target {
            SwTargetCounter::Task(counter) => counter.set_disabled(),
            SwTargetCounter::Cpu(counter) => counter.set_disabled(),
        }
        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn read_values(&mut self) -> StarryResult<PerfReadValues> {
        Ok(match &self.target {
            SwTargetCounter::Task(counter) => counter.snapshot(),
            SwTargetCounter::Cpu(counter) => counter.snapshot(),
        })
    }

    fn reset(&mut self) -> StarryResult<()> {
        match &self.target {
            SwTargetCounter::Task(counter) => counter.reset(),
            SwTargetCounter::Cpu(counter) => counter.reset(),
        }
        Ok(())
    }

    fn link_group(&mut self, leader: &mut dyn PerfEventOps) -> StarryResult<()> {
        let leader = leader
            .as_any_mut()
            .downcast_mut::<SwPerfEvent>()
            .ok_or(StarryError::InvalidInput)?;
        match (&leader.target, &self.target) {
            (SwTargetCounter::Task(leader), SwTargetCounter::Task(member)) => {
                SwPerTaskCounter::link_group(leader, member)
            }
            (SwTargetCounter::Cpu(leader), SwTargetCounter::Cpu(member)) => {
                SwSystemCounter::link_group(leader, member)
            }
            _ => Err(StarryError::InvalidInput),
        }
    }

    fn group_backend(&mut self) -> super::PerfGroupBackend {
        super::PerfGroupBackend::Software
    }
}

impl Pollable for SwPerfEvent {
    fn poll(&self) -> IoEvents {
        IoEvents::IN
    }

    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: IoEvents,
    ) {
    }
}

/// Initializes the CPU-wide registry before userspace can open perf events.
pub fn initialize() {
    SYSTEM_COUNTERS.init_once(IrqMutex::new(Vec::new()));
}

fn attach_task(thread: &Thread, counter: Arc<SwPerTaskCounter>) {
    let mut counters = thread.perf_sw_counters.lock();
    counters.retain(|counter| !counter.state.dead.load(Ordering::Acquire));
    counters.push(counter);
}

fn attach_system(counter: Arc<SwSystemCounter>) {
    let mut counters = SYSTEM_COUNTERS
        .get()
        .expect("perf software registry not initialized")
        .lock();
    counters.retain(|counter| !counter.state.dead.load(Ordering::Acquire));
    counters.push(counter);
}

/// Opens one supported software counter for a task or fixed CPU target.
pub fn perf_event_open_sw(
    attr: &perf_event_attr,
    sw_id: perf_sw_ids,
    target: &AuthorizedPerfTarget,
) -> StarryResult<SwPerfEvent> {
    let raw_period = unsafe { attr.__bindgen_anon_1.sample_period };
    // Linux accepts sample_type on a counting event and simply does not emit
    // samples while sample_period/freq is zero. Upstream `perf stat -vv` sets
    // PERF_SAMPLE_IDENTIFIER on its software counting events.
    if raw_period != 0 {
        return Err(StarryError::OperationNotSupported);
    }
    let kind = SwId::from_raw(sw_id).ok_or(StarryError::OperationNotSupported)?;
    let state = Arc::new(SwEventState::new(kind, attr));
    let enabled = attr.disabled() == 0;
    let counter = match target {
        AuthorizedPerfTarget::Task { task, cpu } => {
            let thread = task.as_thread();
            let counter = Arc::new(SwPerTaskCounter::new(
                state.clone(),
                thread.pid_identity().id(),
                cpu.map(super::target::PerfCpuId::as_usize),
                enabled,
                attr.enable_on_exec() != 0,
            ));
            attach_task(thread, counter.clone());
            if enabled {
                counter.arm_if_current(now_ns());
            }
            SwTargetCounter::Task(counter)
        }
        AuthorizedPerfTarget::Cpu(cpu) => {
            if attr.inherit() != 0 || attr.enable_on_exec() != 0 {
                return Err(StarryError::InvalidInput);
            }
            let counter = Arc::new(SwSystemCounter::new(state.clone(), cpu.as_usize(), enabled));
            attach_system(counter.clone());
            SwTargetCounter::Cpu(counter)
        }
    };
    PERF_SW_ACTIVE.fetch_add(1, Ordering::AcqRel);
    Ok(SwPerfEvent {
        state,
        target: counter,
    })
}

fn for_each_system(mut operation: impl FnMut(&SwSystemCounter)) {
    if let Some(counters) = SYSTEM_COUNTERS.get() {
        let counters = counters.lock();
        for counter in counters.iter() {
            operation(counter);
        }
    }
}

/// Scheduler entry hook for task clocks, CPU migration events, and CPU-wide
/// migration accounting.
pub fn sched_in(thread: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let now = now_ns();
    let cpu = ax_hal::percpu::this_cpu_id();
    let previous_cpu = thread.perf_sw_last_cpu.swap(cpu as u32, Ordering::AcqRel);
    let migrated = previous_cpu != CPU_UNSET && previous_cpu != cpu as u32;
    {
        let counters = thread.perf_sw_counters.lock();
        for counter in counters.iter() {
            if migrated
                && counter.state.kind == SwId::CpuMigrations
                && counter.is_effectively_enabled()
                && counter.accepts_cpu(cpu)
                && !counter.state.dead.load(Ordering::Acquire)
            {
                counter.state.count.fetch_add(1, Ordering::Relaxed);
            }
            counter.start_slice(now, cpu);
        }
    }
    if migrated {
        for_each_system(|counter| counter.add_discrete(SwId::CpuMigrations));
    }
}

/// Scheduler exit hook for task running time and context-switch events.
pub fn sched_out(thread: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let now = now_ns();
    {
        let counters = thread.perf_sw_counters.lock();
        for counter in counters.iter() {
            if counter.state.kind == SwId::ContextSwitches
                && counter.is_effectively_enabled()
                && counter.accepts_cpu(ax_hal::percpu::this_cpu_id())
                && !counter.state.dead.load(Ordering::Acquire)
            {
                counter.state.count.fetch_add(1, Ordering::Relaxed);
            }
            counter.close_slice(now);
        }
    }
    for_each_system(|counter| counter.add_discrete(SwId::ContextSwitches));
}

/// Enables bindings armed with `enable_on_exec` after the new image is fully
/// committed. Only the current task's inherited copy is affected.
pub fn on_exec(thread: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let counters = thread.perf_sw_counters.lock();
    for counter in counters.iter() {
        if counter.enable_on_exec.swap(false, Ordering::AcqRel) {
            counter.set_enabled();
        }
    }
}

/// Charges one user-address page fault to the current task and CPU contexts.
pub fn on_page_fault(thread: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let cpu = ax_hal::percpu::this_cpu_id();
    {
        let counters = thread.perf_sw_counters.lock();
        for counter in counters.iter() {
            if counter.state.kind == SwId::PageFaults
                && counter.is_effectively_enabled()
                && counter.accepts_cpu(cpu)
                && !counter.state.dead.load(Ordering::Acquire)
            {
                counter.state.count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    for_each_system(|counter| counter.add_discrete(SwId::PageFaults));
}

/// Creates per-child bindings for every live inherited event before the child
/// becomes runnable.
pub fn on_clone_inherit(parent: &Thread, child: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let inherited = {
        let counters = parent.perf_sw_counters.lock();
        counters
            .iter()
            .filter(|counter| counter.state.inherit && !counter.state.dead.load(Ordering::Acquire))
            .map(|counter| (counter.clone(), counter.clone_for(child)))
            .collect::<Vec<_>>()
    };
    for (parent_counter, child_counter) in &inherited {
        let Some(parent_leader) = parent_counter.live_group_leader() else {
            continue;
        };
        let Some((_, child_leader)) = inherited
            .iter()
            .find(|(candidate, _)| Arc::ptr_eq(candidate, &parent_leader))
        else {
            continue;
        };
        let _ = SwPerTaskCounter::link_inherited_group(child_leader, child_counter);
    }
    child
        .perf_sw_counters
        .lock()
        .extend(inherited.into_iter().map(|(_, child)| child));
}

/// Folds an exiting task's last running/enabled windows while leaving the
/// aggregate readable through an fd that outlives the task.
pub fn on_task_exit(thread: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let counters = thread.perf_sw_counters.lock();
    for counter in counters.iter() {
        counter.retire();
    }
}
