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
    sync::{IrqMutex, Mutex},
    task::{PidIdentityId, Thread},
};

/// Number of live software events. Hot-path hooks return after one atomic load
/// when no task or CPU software event exists.
static PERF_SW_ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Sentinel used before a task has run while software accounting is active.
pub(crate) const CPU_UNSET: u32 = u32::MAX;

static SYSTEM_COUNTERS: LazyInit<IrqMutex<Vec<Arc<SwSystemCounter>>>> = LazyInit::new();
// Like perf_event_context::mutex, all CPU events share one task-context
// transaction lock per CPU, including members reached through another FD.
static SYSTEM_CONTEXTS: LazyInit<Vec<Mutex<()>>> = LazyInit::new();

/// Scheduler and task-context controls serialize on this per-thread boundary.
/// Even without an event, switch hooks keep the current running CPU published,
/// so a remote opener can start a software clock in the existing interval.
#[derive(Default)]
pub(crate) struct SwTaskContext {
    counters: Vec<Arc<SwPerTaskCounter>>,
    running_cpu: Option<usize>,
}

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
    inherit_thread: bool,
    exclude_user: bool,
    exclude_kernel: bool,
    /// Serializes family controls with cloning, never acquired by IRQ hooks.
    control: Mutex<()>,
    bindings: Mutex<Vec<Weak<SwPerTaskCounter>>>,
    dead: AtomicBool,
    count: AtomicU64,
    clock: IrqMutex<SwClock>,
    time_enabled_ns: AtomicU64,
}

/// Lifetime running time and resettable clock value share one commit boundary.
/// A slice ending after RESET contributes its full duration to running time,
/// but only its post-reset portion to the event value.
#[derive(Debug, Default)]
struct SwClock {
    runtime_ns: u64,
    count_ns: u64,
    reset_at_ns: u64,
}

impl SwClock {
    fn reset(&mut self, now: u64) {
        self.count_ns = 0;
        self.reset_at_ns = now;
    }

    fn finish_slice(&mut self, since: u64, now: u64) {
        self.runtime_ns = self.runtime_ns.saturating_add(now.saturating_sub(since));
        self.count_ns = self
            .count_ns
            .saturating_add(now.saturating_sub(since.max(self.reset_at_ns)));
    }
}

impl SwEventState {
    fn new(kind: SwId, attr: &perf_event_attr) -> Self {
        Self {
            kind,
            read_format: attr.read_format,
            inherit: attr.inherit() != 0,
            inherit_thread: attr.inherit_thread() != 0,
            exclude_user: attr.exclude_user() != 0,
            exclude_kernel: attr.exclude_kernel() != 0,
            control: Mutex::new(()),
            bindings: Mutex::new(Vec::new()),
            dead: AtomicBool::new(false),
            count: AtomicU64::new(0),
            clock: IrqMutex::new(SwClock::default()),
            time_enabled_ns: AtomicU64::new(0),
        }
    }

    fn reset(&self) {
        let mut clock = self.clock.lock();
        clock.reset(now_ns());
        self.count.store(0, Ordering::Release);
    }

    fn accepts_mode(&self, user: bool) -> bool {
        if user {
            !self.exclude_user
        } else {
            !self.exclude_kernel
        }
    }

    fn set_family_enabled(&self, enabled: bool) {
        let _control = self.control.lock();
        let bindings = self
            .bindings
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        for binding in bindings {
            let Some(context) = binding.context.upgrade() else {
                continue;
            };
            let context = context.lock();
            if binding.retired.load(Ordering::Acquire) {
                continue;
            }
            if enabled {
                binding.set_enabled_on(context.running_cpu);
            } else {
                binding.set_disabled();
            }
        }
    }

    fn read_task_family(&self) -> PerfReadValues {
        // Like Linux's child_mutex, serialize the family walk with inheritance
        // and controls. Scheduler hooks only need their task context and clock.
        let _control = self.control.lock();
        let bindings = self
            .bindings
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        for binding in &bindings {
            let Some(context) = binding.context.upgrade() else {
                continue;
            };
            let _context = context.lock();
            binding.checkpoint();
        }
        // Each checkpoint advances its binding's cursors. A later switch-out
        // therefore commits only the remainder, not the interval just read.
        let clock = self.clock.lock();
        PerfReadValues {
            value: if self.kind.is_clock() {
                clock.count_ns
            } else {
                self.count.load(Ordering::Acquire)
            },
            time_enabled: self.time_enabled_ns.load(Ordering::Acquire),
            time_running: clock.runtime_ns,
            lost: 0,
            read_format: self.read_format,
        }
    }
}

/// Slice-local state for an event attached to one task. An inherited child gets
/// a new instance so two tasks never race over `run_since_ns` or CPU history.
#[derive(Debug)]
pub struct SwPerTaskCounter {
    state: Arc<SwEventState>,
    context: Weak<IrqMutex<SwTaskContext>>,
    owner: PidIdentityId,
    cpu_filter: Option<usize>,
    enabled: AtomicBool,
    enable_on_exec: AtomicBool,
    retired: AtomicBool,
    enabled_since_ns: AtomicU64,
    run_since_ns: AtomicU64,
    /// A sibling keeps only weak ownership of its leader. Closing the leader
    /// therefore makes the sibling standalone instead of creating a cycle.
    group_leader: IrqMutex<Option<Weak<SwPerTaskCounter>>>,
    /// The leader owns no sibling; scheduler-visible task bindings retain them.
    group_members: IrqMutex<Vec<Weak<SwPerTaskCounter>>>,
}

impl SwPerTaskCounter {
    fn new(
        state: Arc<SwEventState>,
        context: Weak<IrqMutex<SwTaskContext>>,
        owner: PidIdentityId,
        cpu_filter: Option<usize>,
        enabled: bool,
        enable_on_exec: bool,
    ) -> Self {
        let now = now_ns();
        Self {
            state,
            context,
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

    fn clone_for(&self, child: &Thread, enabled: bool, enable_on_exec: bool) -> Arc<Self> {
        Arc::new(Self::new(
            self.state.clone(),
            Arc::downgrade(&child.perf_sw_counters),
            child.pid_identity().id(),
            self.cpu_filter,
            enabled,
            enable_on_exec,
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

    fn start_slice(&self, now: u64, cpu: usize) {
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
        let mut clock = self.state.clock.lock();
        let since = self.run_since_ns.swap(0, Ordering::AcqRel);
        if since != 0 {
            clock.finish_slice(since, now);
        }
    }

    fn enable_at(&self, now: u64, cpu: Option<usize>) -> bool {
        if !self.enabled.swap(true, Ordering::AcqRel) {
            if self.is_effectively_enabled() {
                self.enabled_since_ns.store(now, Ordering::Release);
                self.arm_on(now, cpu);
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

    fn resume_for_group(&self, now: u64, cpu: Option<usize>) {
        if !self.retired.load(Ordering::Acquire)
            && !self.state.dead.load(Ordering::Acquire)
            && self.is_effectively_enabled()
        {
            let _ =
                self.enabled_since_ns
                    .compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire);
            self.arm_on(now, cpu);
        }
    }

    fn set_enabled_on(&self, cpu: Option<usize>) {
        let now = now_ns();
        if self.enable_at(now, cpu) && self.live_group_leader().is_none() {
            for member in self.group_members.lock().iter().filter_map(Weak::upgrade) {
                member.resume_for_group(now, cpu);
            }
        }
    }

    fn set_disabled(&self) {
        let now = now_ns();
        let is_group_root = self.live_group_leader().is_none();
        if self.disable_at(now) && is_group_root {
            for member in self.group_members.lock().iter().filter_map(Weak::upgrade) {
                member.pause_for_group(now);
            }
        }
    }

    fn arm_on(&self, now: u64, cpu: Option<usize>) {
        if let Some(cpu) = cpu {
            self.start_slice(now, cpu);
        }
    }

    fn reset(&self) {
        self.state.reset();
    }

    /// Commits live windows without stopping the binding. The caller holds
    /// its task context, excluding schedule, enable/disable, and exit updates.
    fn checkpoint(&self) {
        let mut clock = self.state.clock.lock();
        let now = now_ns();
        let since = self.run_since_ns.load(Ordering::Acquire);
        if since != 0 {
            clock.finish_slice(since, now);
            self.run_since_ns.store(now, Ordering::Release);
        }
        let since = self.enabled_since_ns.load(Ordering::Acquire);
        if since != 0 {
            self.state
                .time_enabled_ns
                .fetch_add(now.saturating_sub(since), Ordering::AcqRel);
            self.enabled_since_ns.store(now, Ordering::Release);
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
        // Inherited group publication must not race the root fd's family-wide
        // detach. A closed leader is rejected before changing either binding.
        let _control = leader.state.control.lock();
        if leader.owner != member.owner
            || leader.cpu_filter != member.cpu_filter
            || leader.state.dead.load(Ordering::Acquire)
            || member.state.dead.load(Ordering::Acquire)
        {
            return Err(StarryError::InvalidInput);
        }

        let context = member.context.upgrade().ok_or(StarryError::NoSuchProcess)?;
        let context = context.lock();
        let now = now_ns();
        member.run_since_ns.store(0, Ordering::Release);
        member.enabled_since_ns.store(0, Ordering::Release);
        if reset_new_event {
            member.state.reset();
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
            member.resume_for_group(now, context.running_cpu);
        }
        Ok(())
    }

    fn detach_group_members(leader: &Arc<Self>) {
        let Some(context) = leader.context.upgrade() else {
            return;
        };
        let context = context.lock();
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
                member.resume_for_group(now, context.running_cpu);
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
    context: &'static Mutex<()>,
    enabled: AtomicBool,
    enabled_since_ns: AtomicU64,
    clock_offset_ns: AtomicU64,
    group_leader: IrqMutex<Option<Weak<SwSystemCounter>>>,
    group_members: IrqMutex<Vec<Weak<SwSystemCounter>>>,
}

impl SwSystemCounter {
    fn new(state: Arc<SwEventState>, cpu: usize, enabled: bool) -> Self {
        Self {
            state,
            cpu,
            context: SYSTEM_CONTEXTS
                .get()
                .and_then(|contexts| contexts.get(cpu))
                .expect("software CPU context initialized before event creation"),
            enabled: AtomicBool::new(enabled),
            enabled_since_ns: AtomicU64::new(if enabled { now_ns() } else { 0 }),
            clock_offset_ns: AtomicU64::new(0),
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

    fn enable_at(&self, now: u64, before_publish: impl FnOnce()) -> bool {
        if !self.enabled.swap(true, Ordering::AcqRel) {
            if self.is_effectively_enabled() {
                before_publish();
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
        self.set_enabled_observed(|| {});
    }

    fn set_enabled_observed(&self, before_publish: impl FnOnce()) {
        let _context = self.context.lock();
        let now = now_ns();
        if self.enable_at(now, before_publish) && self.live_group_leader().is_none() {
            for member in self.live_group_members() {
                member.resume_for_group(now);
            }
        }
    }

    fn set_disabled(&self) {
        let _context = self.context.lock();
        let now = now_ns();
        let is_group_root = self.live_group_leader().is_none();
        if self.disable_at(now) && is_group_root {
            for member in self.live_group_members() {
                member.pause_for_group(now);
            }
        }
    }

    fn reset(&self) {
        let _context = self.context.lock();
        self.state.reset();
        self.clock_offset_ns
            .store(self.enabled_time(), Ordering::Release);
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
        let _context = self.context.lock();
        let time = self.enabled_time();
        PerfReadValues {
            value: if self.state.kind.is_clock() {
                time.saturating_sub(self.clock_offset_ns.load(Ordering::Acquire))
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
        let _context = leader.context.lock();
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
        // Serialize closure with clone registration and group publication;
        // scheduler hooks never acquire this sleeping family control lock.
        let _control = self.state.control.lock();
        // Publish leader death and detach siblings in the same CPU transaction
        // used by enable/disable/read; a member cannot revive an old window.
        let _cpu_context = match &self.target {
            SwTargetCounter::Cpu(counter) => Some(counter.context.lock()),
            SwTargetCounter::Task(_) => None,
        };
        if !self.state.dead.swap(true, Ordering::AcqRel) {
            match &self.target {
                SwTargetCounter::Task(_) => {
                    let bindings = self
                        .state
                        .bindings
                        .lock()
                        .iter()
                        .filter_map(Weak::upgrade)
                        .collect::<Vec<_>>();
                    for binding in &bindings {
                        SwPerTaskCounter::detach_group_members(binding);
                    }
                }
                SwTargetCounter::Cpu(counter) => SwSystemCounter::detach_group_members(counter),
            }
            PERF_SW_ACTIVE.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl PerfEventOps for SwPerfEvent {
    fn enable(&mut self) -> StarryResult<()> {
        match &self.target {
            SwTargetCounter::Task(_) => self.state.set_family_enabled(true),
            SwTargetCounter::Cpu(counter) => counter.set_enabled(),
        }
        Ok(())
    }

    fn disable(&mut self) -> StarryResult<()> {
        match &self.target {
            SwTargetCounter::Task(_) => self.state.set_family_enabled(false),
            SwTargetCounter::Cpu(counter) => counter.set_disabled(),
        }
        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn read_values(&mut self) -> StarryResult<PerfReadValues> {
        Ok(match &self.target {
            SwTargetCounter::Task(_) => self.state.read_task_family(),
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
    SYSTEM_CONTEXTS.init_once(
        (0..ax_runtime::hal::cpu_num()).map(|_| Mutex::new(())).collect(),
    );
    SYSTEM_COUNTERS.init_once(IrqMutex::new(Vec::new()));
}

fn attach_task(thread: &Thread, counter: Arc<SwPerTaskCounter>) {
    counter.state.bindings.lock().push(Arc::downgrade(&counter));
    let mut context = thread.perf_sw_counters.lock();
    context
        .counters
        .retain(|counter| !counter.state.dead.load(Ordering::Acquire));
    counter.arm_on(now_ns(), context.running_cpu);
    context.counters.push(counter);
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
                Arc::downgrade(&thread.perf_sw_counters),
                thread.pid_identity().id(),
                cpu.map(super::target::PerfCpuId::as_usize),
                enabled,
                attr.enable_on_exec() != 0,
            ));
            PERF_SW_ACTIVE.fetch_add(1, Ordering::AcqRel);
            attach_task(thread, counter.clone());
            SwTargetCounter::Task(counter)
        }
        AuthorizedPerfTarget::Cpu(cpu) => {
            if attr.inherit() != 0 || attr.enable_on_exec() != 0 {
                return Err(StarryError::InvalidInput);
            }
            let counter = Arc::new(SwSystemCounter::new(state.clone(), cpu.as_usize(), enabled));
            PERF_SW_ACTIVE.fetch_add(1, Ordering::AcqRel);
            attach_system(counter.clone());
            SwTargetCounter::Cpu(counter)
        }
    };
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
    let mut context = thread.perf_sw_counters.lock();
    let cpu = ax_hal::percpu::this_cpu_id();
    context.running_cpu = Some(cpu);
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let now = now_ns();
    let previous_cpu = thread.perf_sw_last_cpu.swap(cpu as u32, Ordering::AcqRel);
    let migrated = previous_cpu != CPU_UNSET && previous_cpu != cpu as u32;
    {
        for counter in context.counters.iter() {
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
    drop(context);
    if migrated {
        for_each_system(|counter| counter.add_discrete(SwId::CpuMigrations));
    }
}

/// Scheduler exit hook for task running time and context-switch events.
pub fn sched_out(thread: &Thread) {
    let mut context = thread.perf_sw_counters.lock();
    context.running_cpu = None;
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let now = now_ns();
    {
        for counter in context.counters.iter() {
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
    drop(context);
    for_each_system(|counter| counter.add_discrete(SwId::ContextSwitches));
}

/// Enables bindings armed with `enable_on_exec` after the new image is fully
/// committed. Only the current task's inherited copy is affected.
pub fn on_exec(thread: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let context = thread.perf_sw_counters.lock();
    for counter in context.counters.iter() {
        if counter.enable_on_exec.swap(false, Ordering::AcqRel) {
            counter.set_enabled_on(context.running_cpu);
        }
    }
}

/// Charges one user-address page fault to the current task and CPU contexts.
pub fn on_page_fault(thread: &Thread, user: bool) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let cpu = ax_hal::percpu::this_cpu_id();
    {
        let counters = thread.perf_sw_counters.lock();
        for counter in counters.counters.iter() {
            if counter.state.kind == SwId::PageFaults
                && counter.is_effectively_enabled()
                && counter.accepts_cpu(cpu)
                && !counter.state.dead.load(Ordering::Acquire)
                && counter.state.accepts_mode(user)
            {
                counter.state.count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    for_each_system(|counter| {
        if counter.state.accepts_mode(user) {
            counter.add_discrete(SwId::PageFaults);
        }
    });
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
            .counters
            .iter()
            .filter(|counter| {
                counter.state.inherit
                    && (!counter.state.inherit_thread
                        || Arc::ptr_eq(&parent.proc_data, &child.proc_data))
                    && !counter.state.dead.load(Ordering::Acquire)
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    let inherited = inherited
        .into_iter()
        .map(|parent_counter| {
            // Family control precedes the task context; no sleeping lock is taken
            // while the scheduler-facing context is held.
            let _control = parent_counter.state.control.lock();
            let (enabled, enable_on_exec) = {
                let _context = parent.perf_sw_counters.lock();
                (
                    parent_counter.enabled.load(Ordering::Acquire),
                    parent_counter.enable_on_exec.load(Ordering::Acquire),
                )
            };
            let child_counter = parent_counter.clone_for(child, enabled, enable_on_exec);
            let mut bindings = parent_counter.state.bindings.lock();
            bindings.retain(|binding| binding.strong_count() != 0);
            bindings.push(Arc::downgrade(&child_counter));
            (Arc::clone(&parent_counter), child_counter)
        })
        .collect::<Vec<_>>();
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
        .counters
        .extend(inherited.into_iter().map(|(_, child)| child));
}

/// Folds an exiting task's last running/enabled windows while leaving the
/// aggregate readable through an fd that outlives the task.
pub fn on_task_exit(thread: &Thread) {
    if PERF_SW_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let counters = thread.perf_sw_counters.lock();
    for counter in counters.counters.iter() {
        counter.retire();
    }
}

#[cfg(all(test, axtest))]
mod tests {
    use super::*;

    #[axtest::axtest]
    fn group_disable_cannot_split_group_enable() {
        use ax_runtime::task::{
            sched::{CpuId, CpuSet, RtPriority, SchedulePolicy},
            sync::WaitQueue,
            thread::current::current_thread_handle,
        };
        use super::super::{PerfContextKey, PerfEvent, target::PerfCpuId};

        if SYSTEM_COUNTERS.get().is_none() {
            initialize();
        }
        let make_event = || {
            // SAFETY: perf_event_attr contains only integer fields and unions.
            let mut attr: perf_event_attr = unsafe { core::mem::zeroed() };
            attr.read_format = 3;
            let state = Arc::new(SwEventState::new(SwId::CpuClock, &attr));
            let counter = Arc::new(SwSystemCounter::new(Arc::clone(&state), 0, false));
            PERF_SW_ACTIVE.fetch_add(1, Ordering::AcqRel);
            PerfEvent::new(alloc::boxed::Box::new(SwPerfEvent {
                state, target: SwTargetCounter::Cpu(counter),
            }), Some(PerfContextKey::Cpu(PerfCpuId::new(0))), false, false).unwrap()
        };
        let leader = Arc::new(make_event());
        let mut member = make_event();
        member.transaction = Arc::clone(&leader.transaction);
        let member = Arc::new(member);
        member.event.lock().link_group(&mut **leader.event.lock()).unwrap();
        *member.group_leader.lock() = Some(Arc::downgrade(&leader));
        leader.members.lock().push(Arc::downgrade(&member));

        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(WaitQueue::new());
        let mut cpu0 = CpuSet::empty(ax_runtime::hal::cpu_num());
        assert!(cpu0.insert(CpuId::new(0)));
        let mut cpu1 = CpuSet::empty(ax_runtime::hal::cpu_num());
        assert!(cpu1.insert(CpuId::new(1)));
        let current = current_thread_handle().unwrap();
        let old_affinity = current.affinity().unwrap();
        let old_policy = current.base_policy();
        current.set_affinity_and_wait(cpu0.clone()).unwrap();
        current.set_policy(SchedulePolicy::fifo(RtPriority::new(20).unwrap())).unwrap();
        let publisher = {
            let leader = Arc::clone(&leader);
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            let gate = Arc::clone(&gate);
            crate::task::spawn_kernel_thread_with_affinity(move || {
                leader.control_group_observed(Some(true), || {
                    entered.store(true, Ordering::Release);
                    gate.notify_all();
                    gate.wait_until(|| release.load(Ordering::Acquire));
                }).unwrap();
            }, "perf-group-enable".into(), cpu1)
        };
        gate.wait_until(|| entered.load(Ordering::Acquire));
        let disabler = {
            let member = Arc::clone(&member);
            let done = Arc::clone(&done);
            crate::task::spawn_kernel_thread_with_policy_and_affinity(move || {
                member.control_group(Some(false)).unwrap();
                done.store(true, Ordering::Release);
            }, "perf-group-disable".into(),
            SchedulePolicy::fifo(RtPriority::new(30).unwrap()), cpu0)
        };
        crate::task::yield_now();
        let premature = done.load(Ordering::Acquire);
        release.store(true, Ordering::Release);
        gate.notify_all();
        crate::task::join_kernel_thread(publisher);
        crate::task::join_kernel_thread(disabler);
        current.set_policy(old_policy).unwrap();
        current.set_affinity_and_wait(old_affinity).unwrap();
        assert!(!premature, "GROUP DISABLE split an unfinished GROUP ENABLE");
        for event in [leader, member] {
            let stopped = event.read_values().unwrap();
            assert_eq!(event.read_values().unwrap().time_enabled, stopped.time_enabled);
        }
    }

    #[axtest::axtest]
    fn cpu_group_disable_waits_for_member_enable_publication() {
        use ax_runtime::task::{
            sched::{CpuId, CpuSet, RtPriority, SchedulePolicy},
            sync::WaitQueue,
            thread::current::current_thread_handle,
        };

        if SYSTEM_COUNTERS.get().is_none() {
            initialize();
        }
        // SAFETY: perf_event_attr consists of integers and integer unions.
        let mut attr: perf_event_attr = unsafe { core::mem::zeroed() };
        attr.read_format = 3;
        let leader = Arc::new(SwSystemCounter::new(
            Arc::new(SwEventState::new(SwId::CpuClock, &attr)), 0, true,
        ));
        let member = Arc::new(SwSystemCounter::new(
            Arc::new(SwEventState::new(SwId::CpuClock, &attr)), 0, false,
        ));
        SwSystemCounter::link_group(&leader, &member).unwrap();
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(WaitQueue::new());
        let mut cpu0 = CpuSet::empty(ax_runtime::hal::cpu_num());
        assert!(cpu0.insert(CpuId::new(0)));
        let mut cpu1 = CpuSet::empty(ax_runtime::hal::cpu_num());
        assert!(cpu1.insert(CpuId::new(1)));
        let current = current_thread_handle().unwrap();
        let old_affinity = current.affinity().unwrap();
        let old_policy = current.base_policy();
        current.set_affinity_and_wait(cpu0.clone()).unwrap();
        current.set_policy(SchedulePolicy::fifo(RtPriority::new(20).unwrap())).unwrap();

        let publisher = {
            let member = Arc::clone(&member);
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            let gate = Arc::clone(&gate);
            crate::task::spawn_kernel_thread_with_affinity(move || {
                member.set_enabled_observed(|| {
                    entered.store(true, Ordering::Release);
                    gate.notify_all();
                    gate.wait_until(|| release.load(Ordering::Acquire));
                });
            }, "perf-member-enable".into(), cpu1)
        };
        gate.wait_until(|| entered.load(Ordering::Acquire));
        let disabler = {
            let leader = Arc::clone(&leader);
            let done = Arc::clone(&done);
            crate::task::spawn_kernel_thread_with_policy_and_affinity(move || {
                leader.set_disabled();
                done.store(true, Ordering::Release);
            }, "perf-leader-disable".into(),
            SchedulePolicy::fifo(RtPriority::new(30).unwrap()), cpu0)
        };
        // The higher-priority same-CPU task must run until it either blocks on
        // the context transaction or incorrectly completes the disable.
        crate::task::yield_now();
        let premature = done.load(Ordering::Acquire);
        release.store(true, Ordering::Release);
        gate.notify_all();
        crate::task::join_kernel_thread(publisher);
        crate::task::join_kernel_thread(disabler);
        current.set_policy(old_policy).unwrap();
        current.set_affinity_and_wait(old_affinity).unwrap();
        assert!(!premature, "leader disable passed an unfinished member enable");
        assert_eq!(member.enabled_since_ns.load(Ordering::Acquire), 0);
        let stopped = member.snapshot();
        assert_eq!(member.snapshot().time_enabled, stopped.time_enabled);
    }

    #[axtest::axtest]
    fn reset_clips_event_value_without_discarding_running_time() {
        let mut clock = super::SwClock::default();
        clock.finish_slice(100, 200);
        clock.reset(250);
        // One slice crosses reset; another inherited binding overlaps it.
        clock.finish_slice(200, 300);
        clock.finish_slice(220, 310);
        assert_eq!(clock.runtime_ns, 290);
        assert_eq!(clock.count_ns, 110);
        clock.reset(320);
        clock.finish_slice(310, 330);
        assert_eq!(clock.runtime_ns, 310);
        assert_eq!(clock.count_ns, 10);
    }
}
