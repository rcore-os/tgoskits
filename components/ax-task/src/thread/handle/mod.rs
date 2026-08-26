//! Strong, weak, and direct IRQ-wake handles.

use alloc::sync::{Arc, Weak};
#[cfg(feature = "lockdep")]
use core::cell::UnsafeCell;
use core::{
    marker::PhantomData,
    mem::ManuallyDrop,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

mod wake_batch;
pub use wake_batch::ThreadWakeBatch;

use crate::{
    CpuId, DeadlineFlags, DeadlinePolicy, FairMode, Nice, ParkPublication, PiWaitNodeStorage,
    PiWaitState, RtPriority, RunQueueNodeStorage, SchedulePolicy, SchedulerTickCpuTime,
    SchedulerTickWork, SchedulerTickWorkClaim, SchedulingKey, SchedulingUrgency, TaskError,
    ThreadAffinityCompletion, ThreadExtensionView, ThreadId, ThreadLifecycle, ThreadSchedCell,
    ThreadState, WakePublication,
    inbox::{InboxKind, InboxNode},
    runtime::{PreemptGuardToken, task_runtime},
    task_work::TaskWorkDoorbell,
    timer::TaskDeadlineNode,
};

const REAP_CLAIMED: usize = 1 << (usize::BITS - 1);
const REAP_MAX_UPGRADE_READERS: usize = REAP_CLAIMED - 1;
const SCHEDULER_ACTIVITY_CLOSED: usize = 1 << (usize::BITS - 1);
const SCHEDULER_ACTIVITY_MAX_READERS: usize = SCHEDULER_ACTIVITY_CLOSED - 1;

#[cfg(feature = "lockdep")]
struct ThreadHeldLocks {
    stack: UnsafeCell<crate::sync::lockdep::HeldLockStack>,
}

#[cfg(feature = "lockdep")]
impl ThreadHeldLocks {
    const fn new() -> Self {
        Self {
            stack: UnsafeCell::new(crate::sync::lockdep::HeldLockStack::new()),
        }
    }

    unsafe fn with_mut<R>(
        &self,
        operation: impl FnOnce(&mut crate::sync::lockdep::HeldLockStack) -> R,
    ) -> R {
        // SAFETY: the caller owns the current-task and migration-exclusion
        // contract documented on `ThreadCore::with_held_locks`.
        unsafe { operation(&mut *self.stack.get()) }
    }
}

#[cfg(feature = "lockdep")]
impl core::fmt::Debug for ThreadHeldLocks {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("ThreadHeldLocks(..)")
    }
}

// SAFETY: the stack is accessed only by its currently executing task while
// local IRQ exclusion prevents migration and scheduler replacement. Other
// threads may retain `ThreadCore` references but cannot access this field.
#[cfg(feature = "lockdep")]
unsafe impl Sync for ThreadHeldLocks {}

/// A strong reference used to inspect and control a live thread.
#[derive(Debug)]
pub struct ThreadHandle {
    pub(crate) core: ManuallyDrop<Arc<ThreadCore>>,
    reap_signal: Arc<ThreadReapSignal>,
}

impl Drop for ThreadHandle {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: `core` is wrapped solely so this destructor can release
            // the strong count before publishing the reaper retry. It is
            // dropped exactly once here and never accessed afterwards.
            ManuallyDrop::drop(&mut self.core);
        }
        self.reap_signal.release_external_lease();
    }
}

impl Clone for ThreadHandle {
    fn clone(&self) -> Self {
        let core = Arc::clone(&self.core);
        let reap_signal = Arc::clone(&self.reap_signal);
        reap_signal.acquire_external_lease();
        Self {
            core: ManuallyDrop::new(core),
            reap_signal,
        }
    }
}

impl ThreadHandle {
    pub(crate) fn from_core(core: Arc<ThreadCore>) -> Self {
        let reap_signal = Arc::clone(&core.reap_signal);
        reap_signal.acquire_external_lease();
        Self {
            core: ManuallyDrop::new(core),
            reap_signal,
        }
    }

    /// Returns the immutable runtime publication for this live scheduler
    /// thread.
    #[doc(hidden)]
    pub fn runtime_publication(&self) -> crate::runtime::CurrentThreadPublication {
        crate::runtime::CurrentThreadPublication::from_core(self.id(), &self.core)
    }

    pub(crate) fn runtime_core_arc(&self) -> &Arc<ThreadCore> {
        &self.core
    }

    /// Returns the generation-checked registry identity.
    pub fn id(&self) -> ThreadId {
        self.core.id
    }

    /// Returns the thread's base scheduling policy.
    pub fn policy(&self) -> SchedulePolicy {
        self.core.base_policy.load()
    }

    /// Returns the policy after priority-inheritance donation is applied.
    pub fn effective_policy(&self) -> SchedulePolicy {
        self.core.effective_policy.load()
    }

    /// Returns the most recently published lifecycle state.
    pub fn state(&self) -> ThreadState {
        self.core.state()
    }

    /// Creates a non-owning lifecycle observer.
    pub fn downgrade(&self) -> WeakThreadHandle {
        WeakThreadHandle {
            core: Arc::downgrade(&self.core),
        }
    }

    /// Creates a direct wake handle that does not consult the thread registry.
    pub fn wake_handle(&self) -> ThreadWakeHandle {
        ThreadWakeHandle::from_core(Arc::clone(&self.core))
    }

    /// Returns the physical CPU that must cross a scheduler boundary.
    ///
    /// Unlike direct wake placement, this snapshot remains on the source CPU
    /// until switch tail releases the outgoing context. A task-context caller
    /// can therefore publish state, take this snapshot, and rendezvous with the
    /// returned CPU; either the thread is still active there or it crossed a
    /// scheduler boundary after the publication.
    pub fn scheduler_fence_cpu(&self) -> Option<CpuId> {
        self.core.sched().scheduler_fence_cpu()
    }

    /// Returns Linux `task_cpu()`: the last committed runqueue assignment.
    ///
    /// This remains the previous CPU while a task sleeps and changes to the
    /// destination when an rq-to-rq migration commits. Physical execution
    /// ownership is exposed separately by [`Self::scheduler_fence_cpu`].
    /// A wake-placement hint is never reported as scheduler placement.
    pub fn assigned_cpu(&self) -> Option<CpuId> {
        self.core.assigned_cpu()
    }

    pub(crate) fn extension_view(&self) -> Option<crate::ThreadExtensionView> {
        self.core.extension_view()
    }
}

impl Eq for ThreadHandle {}

impl PartialEq for ThreadHandle {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

/// A non-owning thread observer for ordinary task context.
#[derive(Clone, Debug)]
pub struct WeakThreadHandle {
    core: Weak<ThreadCore>,
}

impl WeakThreadHandle {
    /// Attempts to acquire a strong reference while the thread header is alive.
    pub fn upgrade(&self) -> Option<ThreadHandle> {
        let core = self.core.upgrade()?;
        if !core.try_enter_weak_upgrade() {
            return None;
        }
        let handle = ThreadHandle::from_core(core);
        handle.core.exit_weak_upgrade();
        Some(handle)
    }
}

/// A stable direct wake header reference.
///
/// [`Self::wake`] performs only bounded atomic operations and is safe in hard IRQ
/// context. Creating, cloning, and dropping this owning reference are task-context
/// operations. A coroutine whose last raw-waker reference is released in hard IRQ
/// defers only that zero-reference allocation to the typed task-system reaper.
#[derive(Debug)]
pub struct ThreadWakeHandle {
    pub(crate) core: ManuallyDrop<Arc<ThreadCore>>,
    reap_signal: Arc<ThreadReapSignal>,
}

impl Drop for ThreadWakeHandle {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: identical ownership rule to ThreadHandle::drop above.
            ManuallyDrop::drop(&mut self.core);
        }
        self.reap_signal.release_external_lease();
    }
}

impl Clone for ThreadWakeHandle {
    fn clone(&self) -> Self {
        let core = Arc::clone(&self.core);
        let reap_signal = Arc::clone(&self.reap_signal);
        reap_signal.acquire_external_lease();
        Self {
            core: ManuallyDrop::new(core),
            reap_signal,
        }
    }
}

impl ThreadWakeHandle {
    pub(crate) fn from_core(core: Arc<ThreadCore>) -> Self {
        let reap_signal = Arc::clone(&core.reap_signal);
        reap_signal.acquire_external_lease();
        Self {
            core: ManuallyDrop::new(core),
            reap_signal,
        }
    }

    /// Directly wakes the thread without allocating, sleeping, or invoking callbacks.
    ///
    /// This IRQ-safe operation may acquire the thread scheduler lock and the
    /// selected CPU's raw runqueue lock.
    pub fn wake(&self) -> WakeResult {
        self.core.wake(WakeIntent::Normal)
    }

    /// Wakes from task context when the caller expects to block shortly.
    ///
    /// This is Linux's `WF_SYNC` contract. It remains a scheduling hint: the
    /// waker commits the destination runqueue activation before returning.
    pub fn wake_sync(&self) -> WakeResult {
        debug_assert!(!crate::runtime::task_runtime::in_hard_irq());
        self.core.wake(WakeIntent::Sync)
    }

    /// Wakes from ordinary task context.
    pub fn wake_from_task(&self) -> WakeResult {
        self.core.wake(WakeIntent::Normal)
    }

    pub(crate) fn deliver_wait_claim_from_task(
        &self,
        claim: &crate::WaitWakeClaim,
        intent: WakeIntent,
    ) -> crate::WaitWakeDelivery {
        crate::facade::wake_wait_claim_from_task(&self.core, claim, intent)
    }

    /// Returns the thread that owns this wake header.
    pub fn thread_id(&self) -> ThreadId {
        self.core.id
    }
}

impl ThreadCore {
    fn wake(self: &Arc<Self>, intent: WakeIntent) -> WakeResult {
        if self.state() == ThreadState::Exited {
            return WakeResult::Exited;
        }
        crate::facade::wake_thread_from_current_cpu(self, intent)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WakeIntent {
    Normal,
    Sync,
}

impl WakeIntent {
    pub(crate) const fn is_sync(self) -> bool {
        matches!(self, Self::Sync)
    }
}

/// Result of an IRQ-safe wake publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WakeResult {
    /// This call completed a logical wake transaction. The thread is runnable,
    /// retained a park notification, or has owner-CPU activation committed.
    Notified,
    /// An unresolved park-transition notification already represents this event.
    AlreadyPending,
    /// The destination thread has exited, so the late wake is ignored.
    Exited,
    /// No scheduler-ready CPU is currently reachable for wake delivery.
    Unavailable,
}

/// Runqueue-coherent snapshot of one thread's charged CPU runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThreadRuntimeSnapshot {
    charged_runtime_ns: u64,
    running: bool,
}

impl ThreadRuntimeSnapshot {
    /// Returns cumulative CPU runtime, including the current running residual.
    pub const fn charged_runtime_ns(self) -> u64 {
        self.charged_runtime_ns
    }

    /// Returns whether the snapshot included a live running residual.
    pub const fn is_running(self) -> bool {
        self.running
    }
}

#[derive(Debug)]
struct ThreadReapSignal {
    exited: AtomicBool,
    external_leases: AtomicUsize,
    task_work: Option<Arc<TaskWorkDoorbell>>,
}

#[must_use = "the scheduler activity guard serializes owner delivery against exit"]
pub(crate) struct ThreadSchedulerActivity<'thread> {
    core: &'thread ThreadCore,
    preempt: PreemptGuardToken,
    _not_send: PhantomData<*mut ()>,
}

impl Drop for ThreadSchedulerActivity<'_> {
    fn drop(&mut self) {
        self.core.finish_scheduler_activity();
        release_scheduler_preempt(self.preempt);
    }
}

#[must_use = "the owned scheduler exit guard closes new activity until exit commits"]
pub(crate) struct OwnedThreadSchedulerExit {
    core: Arc<ThreadCore>,
    preempt: PreemptGuardToken,
    sealed: bool,
    _not_send: PhantomData<*mut ()>,
}

impl OwnedThreadSchedulerExit {
    pub(crate) fn seal(&mut self) {
        self.sealed = true;
    }
}

impl Drop for OwnedThreadSchedulerExit {
    fn drop(&mut self) {
        if !self.sealed {
            self.core.reopen_scheduler_activity();
        }
        release_scheduler_preempt(self.preempt);
    }
}

fn release_scheduler_preempt(token: PreemptGuardToken) {
    if token.is_none() {
        return;
    }
    // SAFETY: scheduler activity and exit guards are !Send and consume the
    // exact token returned on this execution context after publishing their
    // final gate state.
    unsafe { task_runtime::preempt_guard_exit(token) };
}

#[must_use = "dropping the delivery lease makes an exited thread reapable"]
pub(crate) struct ThreadSchedulerInboxDelivery<'thread> {
    core: &'thread ThreadCore,
}

impl Drop for ThreadSchedulerInboxDelivery<'_> {
    fn drop(&mut self) {
        self.core.finish_scheduler_inbox_delivery();
    }
}

impl ThreadReapSignal {
    fn new(task_work: Option<Arc<TaskWorkDoorbell>>) -> Self {
        Self {
            exited: AtomicBool::new(false),
            external_leases: AtomicUsize::new(0),
            task_work,
        }
    }

    fn mark_exited(&self) {
        self.exited.store(true, Ordering::Release);
    }

    fn publish(&self) {
        if let Some(task_work) = &self.task_work {
            task_work.publish();
        }
    }

    fn acquire_external_lease(&self) {
        self.external_leases
            .try_update(Ordering::AcqRel, Ordering::Acquire, |leases| {
                leases.checked_add(1)
            })
            .expect("thread external-lifetime lease count overflow");
    }

    fn release_external_lease(&self) {
        let previous = self.external_leases.fetch_sub(1, Ordering::AcqRel);
        assert!(previous != 0, "unbalanced thread external-lifetime lease");
        if previous == 1 && self.exited.load(Ordering::Acquire) {
            self.publish();
        }
    }

    fn external_lease_count(&self) -> usize {
        self.external_leases.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub(crate) struct ThreadCore {
    id: ThreadId,
    sched: Arc<ThreadSchedCell>,
    runqueue_nodes: RunQueueNodeStorage,
    pi_wait_nodes: PiWaitNodeStorage,
    // Immutable after publication. Every handle retaining this copy also pins
    // the registry-owned extension destructor through the reaper Arc contract.
    extension: Option<ThreadExtensionView>,
    scheduler_tick_cpu_time: Option<Arc<SchedulerTickCpuTime>>,
    scheduler_tick_work: Option<SchedulerTickWork>,
    scheduler_tick_work_generation: AtomicU64,
    scheduler_tick_observed_ns: AtomicU64,
    scheduler_tick_work_node: InboxNode,
    deadline_callback_node: InboxNode,
    base_policy: AtomicPolicy,
    effective_policy: AtomicPolicy,
    effective_key_sequence: AtomicUsize,
    effective_deadline_active: AtomicBool,
    effective_deadline_ns: AtomicU64,
    state: Arc<ThreadLifecycle>,
    reap_signal: Arc<ThreadReapSignal>,
    reap_gate: AtomicUsize,
    scheduler_activity_gate: AtomicUsize,
    scheduler_inbox_deliveries: AtomicUsize,
    pub(super) affinity_completion: ThreadAffinityCompletion,
    park_generation: AtomicU64,
    wake_cpu_hint: AtomicU32,
    wake_affinity: WakeAffinityState,
    affinity_update_node: InboxNode,
    deadline_refresh_node: InboxNode,
    remote_wake_node: InboxNode,
    wake_batch_next: AtomicPtr<ThreadCore>,
    wake_batch_linked: AtomicBool,
    sleep_timer: TaskDeadlineNode,
    deadline_cbs_timer: TaskDeadlineNode,
    deadline_zero_lag_timer: TaskDeadlineNode,
    sleep_timer_cpu: AtomicU32,
    sleep_timer_generation: AtomicU64,
    migration_node: InboxNode,
    committed_runtime_ns: AtomicU64,
    #[cfg(feature = "lockdep")]
    held_locks: ThreadHeldLocks,
    pi_wait_state: PiWaitState,
}

impl ThreadCore {
    pub(crate) fn new(
        id: ThreadId,
        policy: SchedulePolicy,
        sched: Arc<ThreadSchedCell>,
        extension: Option<ThreadExtensionView>,
        scheduler_tick_cpu_time: Option<Arc<SchedulerTickCpuTime>>,
        scheduler_tick_work: Option<SchedulerTickWork>,
        task_work: Option<Arc<TaskWorkDoorbell>>,
    ) -> Self {
        debug_assert_eq!(id, sched.id());
        let lifecycle = Arc::clone(sched.lifecycle());
        let reap_signal = Arc::new(ThreadReapSignal::new(task_work));
        Self {
            id,
            sched,
            runqueue_nodes: RunQueueNodeStorage::new(),
            pi_wait_nodes: PiWaitNodeStorage::new(),
            extension,
            scheduler_tick_cpu_time,
            scheduler_tick_work,
            scheduler_tick_work_generation: AtomicU64::new(0),
            scheduler_tick_observed_ns: AtomicU64::new(0),
            scheduler_tick_work_node: InboxNode::new(InboxKind::TaskWork),
            deadline_callback_node: InboxNode::new(InboxKind::TaskWork),
            base_policy: AtomicPolicy::new(policy),
            effective_policy: AtomicPolicy::new(policy),
            effective_key_sequence: AtomicUsize::new(0),
            effective_deadline_active: AtomicBool::new(false),
            effective_deadline_ns: AtomicU64::new(0),
            state: lifecycle,
            reap_signal,
            reap_gate: AtomicUsize::new(0),
            scheduler_activity_gate: AtomicUsize::new(0),
            scheduler_inbox_deliveries: AtomicUsize::new(0),
            affinity_completion: ThreadAffinityCompletion::new(1),
            park_generation: AtomicU64::new(0),
            wake_cpu_hint: AtomicU32::new(u32::MAX),
            wake_affinity: WakeAffinityState::new(),
            affinity_update_node: InboxNode::new(InboxKind::OwnerControl),
            deadline_refresh_node: InboxNode::new(InboxKind::OwnerControl),
            remote_wake_node: InboxNode::new(InboxKind::OwnerControl),
            wake_batch_next: AtomicPtr::new(core::ptr::null_mut()),
            wake_batch_linked: AtomicBool::new(false),
            sleep_timer: TaskDeadlineNode::for_thread(id),
            deadline_cbs_timer: TaskDeadlineNode::deadline_cbs_for_thread(id),
            deadline_zero_lag_timer: TaskDeadlineNode::deadline_zero_lag_for_thread(id),
            sleep_timer_cpu: AtomicU32::new(u32::MAX),
            sleep_timer_generation: AtomicU64::new(0),
            migration_node: InboxNode::new(InboxKind::OwnerControl),
            committed_runtime_ns: AtomicU64::new(0),
            #[cfg(feature = "lockdep")]
            held_locks: ThreadHeldLocks::new(),
            pi_wait_state: PiWaitState::new(),
        }
    }

    pub(crate) const fn runqueue_nodes(&self) -> &RunQueueNodeStorage {
        &self.runqueue_nodes
    }

    pub(crate) const fn pi_wait_nodes(&self) -> &PiWaitNodeStorage {
        &self.pi_wait_nodes
    }

    /// Mutates lockdep state owned by this currently executing thread.
    ///
    /// # Safety
    ///
    /// The caller must prove that this is the current thread and prevent local
    /// IRQ entry, migration, and scheduler replacement for the complete call.
    #[cfg(feature = "lockdep")]
    pub(crate) unsafe fn with_held_locks<R>(
        &self,
        operation: impl FnOnce(&mut crate::sync::lockdep::HeldLockStack) -> R,
    ) -> R {
        unsafe { self.held_locks.with_mut(operation) }
    }
}

mod lifecycle;
mod policy;
mod runtime_accounting;
mod wake_affinity;
mod wake_state;

use policy::AtomicPolicy;
use wake_affinity::WakeAffinityState;
