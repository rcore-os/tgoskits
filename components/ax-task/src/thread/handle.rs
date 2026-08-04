//! Strong, weak, and direct IRQ wake handles.

use alloc::sync::{Arc, Weak};
use core::{
    marker::PhantomData,
    mem::ManuallyDrop,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

mod wake_batch;
pub use wake_batch::ThreadWakeBatch;

use crate::{
    CpuId, DeadlineFlags, DeadlinePolicy, FairMode, Nice, PiWaitState, RtPriority,
    RunQueueNodeStorage, SchedulePolicy, SchedulerTickWork, SchedulerTickWorkClaim, SchedulingKey,
    SchedulingUrgency, TaskError, ThreadAffinityCompletion, ThreadExtensionView, ThreadId,
    ThreadSchedCell, ThreadState,
    inbox::{InboxKind, InboxNode},
    runtime::{PreemptGuardToken, task_runtime},
    task_work::TaskWorkDoorbell,
    timer::TaskDeadlineNode,
};

const REAP_CLAIMED: usize = 1 << (usize::BITS - 1);
const REAP_MAX_UPGRADE_READERS: usize = REAP_CLAIMED - 1;
const SCHEDULER_ACTIVITY_CLOSED: usize = 1 << (usize::BITS - 1);
const SCHEDULER_ACTIVITY_MAX_READERS: usize = SCHEDULER_ACTIVITY_CLOSED - 1;
const WAKE_PENDING: u8 = 1 << 0;
const PARK_NOTIFIED: u8 = 1 << 1;
const WAKE_STATE_PUBLISHED: u8 = WAKE_PENDING | PARK_NOTIFIED;

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

    /// Returns the current scheduling urgency key used by PI waiter ordering.
    pub fn effective_scheduling_key(&self) -> SchedulingKey {
        self.core.effective_scheduling_key()
    }

    /// Returns effective urgency without a thread-identity tie-break.
    pub fn effective_scheduling_urgency(&self) -> SchedulingUrgency {
        self.core.effective_scheduling_urgency()
    }

    /// Returns cumulative charged CPU runtime, including a running residual.
    pub fn runtime_snapshot(&self, now_ns: u64) -> ThreadRuntimeSnapshot {
        self.core.runtime_snapshot(now_ns)
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

    /// Returns the CPU currently owning the thread's scheduler placement.
    ///
    /// A switching-out thread remains assigned to its physical source until
    /// switch tail releases `on_cpu`. A detached sleeper falls back to its
    /// Linux-style wake CPU hint without presenting that hint as runqueue
    /// ownership.
    pub fn assigned_cpu(&self) -> Option<CpuId> {
        self.core.assigned_cpu()
    }

    pub(crate) fn sleep_timer(&self) -> &TaskDeadlineNode {
        &self.core.sleep_timer
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
    fn from_core(core: Arc<ThreadCore>) -> Self {
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
        self.core.wake()
    }

    #[cfg(test)]
    pub(crate) fn wake_from_cpu_hint_for_test(&self, target: CpuId) -> WakeResult {
        crate::facade::wake_thread_direct(&self.core, Some(target))
    }

    /// Wakes from ordinary task context.
    pub fn wake_from_task(&self) -> WakeResult {
        self.core.wake()
    }

    /// Returns the thread that owns this wake header.
    pub fn thread_id(&self) -> ThreadId {
        self.core.id
    }
}

impl ThreadCore {
    fn wake(self: &Arc<Self>) -> WakeResult {
        if self.state() == ThreadState::Exited {
            return WakeResult::Exited;
        }
        crate::facade::wake_thread_direct(self, None)
    }
}

/// Result of an IRQ-safe direct wake publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WakeResult {
    /// This call completed a wake transaction. The thread is runnable already,
    /// became runnable, or retained the notification for a park transition.
    Notified,
    /// An unresolved park-transition notification already represents this event.
    AlreadyPending,
    /// The destination thread has exited, so the late wake is ignored.
    Exited,
    /// No scheduler-ready CPU is currently reachable for wake delivery.
    Unavailable,
}

/// Lock-free snapshot of one thread's charged CPU runtime.
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
    // Immutable after publication. Every handle retaining this copy also pins
    // the registry-owned extension destructor through the reaper Arc contract.
    extension: Option<ThreadExtensionView>,
    scheduler_tick_work: Option<SchedulerTickWork>,
    scheduler_tick_work_generation: AtomicU64,
    scheduler_tick_observed_ns: AtomicU64,
    scheduler_tick_work_node: InboxNode,
    deadline_callback_node: InboxNode,
    base_policy: AtomicPolicy,
    effective_policy: AtomicPolicy,
    effective_key_sequence: AtomicUsize,
    effective_deadline_ns: AtomicU64,
    state: AtomicU8,
    reap_signal: Arc<ThreadReapSignal>,
    reap_gate: AtomicUsize,
    scheduler_activity_gate: AtomicUsize,
    scheduler_inbox_deliveries: AtomicUsize,
    pub(super) affinity_completion: ThreadAffinityCompletion,
    wake_state: AtomicU8,
    park_generation: AtomicU64,
    wake_cpu_hint: AtomicU32,
    policy_update_node: InboxNode,
    affinity_update_node: InboxNode,
    deadline_refresh_node: InboxNode,
    wake_batch_next: AtomicPtr<ThreadCore>,
    wake_batch_linked: AtomicBool,
    sleep_timer: TaskDeadlineNode,
    deadline_cbs_timer: TaskDeadlineNode,
    deadline_zero_lag_timer: TaskDeadlineNode,
    sleep_timer_cpu: AtomicU32,
    sleep_timer_generation: AtomicU64,
    migration_node: InboxNode,
    runtime_sequence: AtomicU64,
    charged_runtime_ns: AtomicU64,
    runtime_accounted_until_ns: AtomicU64,
    runtime_running: AtomicBool,
    pi_wait_state: PiWaitState,
}

impl ThreadCore {
    pub(crate) fn new(
        id: ThreadId,
        policy: SchedulePolicy,
        sched: Arc<ThreadSchedCell>,
        extension: Option<ThreadExtensionView>,
        scheduler_tick_work: Option<SchedulerTickWork>,
        task_work: Option<Arc<TaskWorkDoorbell>>,
    ) -> Self {
        debug_assert_eq!(id, sched.id());
        let reap_signal = Arc::new(ThreadReapSignal::new(task_work));
        Self {
            id,
            sched,
            runqueue_nodes: RunQueueNodeStorage::new(),
            extension,
            scheduler_tick_work,
            scheduler_tick_work_generation: AtomicU64::new(0),
            scheduler_tick_observed_ns: AtomicU64::new(0),
            scheduler_tick_work_node: InboxNode::new(InboxKind::TaskWork),
            deadline_callback_node: InboxNode::new(InboxKind::TaskWork),
            base_policy: AtomicPolicy::new(policy),
            effective_policy: AtomicPolicy::new(policy),
            effective_key_sequence: AtomicUsize::new(0),
            effective_deadline_ns: AtomicU64::new(0),
            state: AtomicU8::new(ThreadState::New as u8),
            reap_signal,
            reap_gate: AtomicUsize::new(0),
            scheduler_activity_gate: AtomicUsize::new(0),
            scheduler_inbox_deliveries: AtomicUsize::new(0),
            affinity_completion: ThreadAffinityCompletion::new(1),
            wake_state: AtomicU8::new(0),
            park_generation: AtomicU64::new(0),
            wake_cpu_hint: AtomicU32::new(u32::MAX),
            policy_update_node: InboxNode::new(InboxKind::OwnerControl),
            affinity_update_node: InboxNode::new(InboxKind::OwnerControl),
            deadline_refresh_node: InboxNode::new(InboxKind::OwnerControl),
            wake_batch_next: AtomicPtr::new(core::ptr::null_mut()),
            wake_batch_linked: AtomicBool::new(false),
            sleep_timer: TaskDeadlineNode::for_thread(id),
            deadline_cbs_timer: TaskDeadlineNode::deadline_cbs_for_thread(id),
            deadline_zero_lag_timer: TaskDeadlineNode::deadline_zero_lag_for_thread(id),
            sleep_timer_cpu: AtomicU32::new(u32::MAX),
            sleep_timer_generation: AtomicU64::new(0),
            migration_node: InboxNode::new(InboxKind::OwnerControl),
            runtime_sequence: AtomicU64::new(0),
            charged_runtime_ns: AtomicU64::new(0),
            runtime_accounted_until_ns: AtomicU64::new(0),
            runtime_running: AtomicBool::new(false),
            pi_wait_state: PiWaitState::new(),
        }
    }

    pub(crate) const fn runqueue_nodes(&self) -> &RunQueueNodeStorage {
        &self.runqueue_nodes
    }

    pub(crate) fn begin_runtime_accounting(&self, now_ns: u64) {
        self.begin_runtime_write();
        self.runtime_accounted_until_ns
            .store(now_ns, Ordering::Relaxed);
        self.runtime_running.store(true, Ordering::Relaxed);
        self.finish_runtime_write();
    }

    pub(crate) fn charge_runtime(&self, runtime_ns: u64, now_ns: u64) {
        self.begin_runtime_write();
        let total = self.charged_runtime_ns.load(Ordering::Relaxed);
        self.charged_runtime_ns
            .store(total.saturating_add(runtime_ns), Ordering::Relaxed);
        self.runtime_accounted_until_ns
            .store(now_ns, Ordering::Relaxed);
        self.finish_runtime_write();
    }

    pub(crate) fn finish_runtime_accounting(&self, now_ns: u64) {
        self.begin_runtime_write();
        self.runtime_accounted_until_ns
            .store(now_ns, Ordering::Relaxed);
        self.runtime_running.store(false, Ordering::Relaxed);
        self.finish_runtime_write();
    }

    pub(crate) fn runtime_snapshot(&self, now_ns: u64) -> ThreadRuntimeSnapshot {
        loop {
            let sequence = self.runtime_sequence.load(Ordering::Acquire);
            if sequence & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let charged = self.charged_runtime_ns.load(Ordering::Relaxed);
            let accounted_until = self.runtime_accounted_until_ns.load(Ordering::Relaxed);
            let running = self.runtime_running.load(Ordering::Relaxed);
            if self.runtime_sequence.load(Ordering::Acquire) == sequence {
                let residual = if running {
                    now_ns.saturating_sub(accounted_until)
                } else {
                    0
                };
                return ThreadRuntimeSnapshot {
                    charged_runtime_ns: charged.saturating_add(residual),
                    running,
                };
            }
        }
    }

    fn begin_runtime_write(&self) {
        let sequence = self.runtime_sequence.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(sequence & 1, 0, "runtime accounting has multiple writers");
    }

    fn finish_runtime_write(&self) {
        let sequence = self.runtime_sequence.fetch_add(1, Ordering::Release);
        debug_assert_eq!(sequence & 1, 1, "runtime accounting writer lost ownership");
    }

    pub(crate) fn publish_state(&self, state: ThreadState) {
        if state == ThreadState::Exited {
            self.reap_signal.mark_exited();
        }
        self.state.store(state as u8, Ordering::Release);
    }

    pub(crate) fn begin_scheduler_tick_work(&self, observed_ns: u64) -> bool {
        let Some(work) = self.scheduler_tick_work.as_ref() else {
            return false;
        };
        let Some(generation) = work.enabled_generation() else {
            return false;
        };
        self.scheduler_tick_observed_ns
            .fetch_max(observed_ns, Ordering::AcqRel);
        let mut pending = self.scheduler_tick_work_generation.load(Ordering::Acquire);
        loop {
            // Even an already-pending generation must perform an RMW. This
            // publishes the timestamp to the consumer's generation claim. If
            // the consumer raced ahead and cleared the generation, the CAS
            // fails and this producer installs a fresh physical publication.
            match self.scheduler_tick_work_generation.compare_exchange_weak(
                pending,
                generation,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return pending == 0,
                Err(current) => pending = current,
            }
        }
    }

    pub(crate) fn cancel_scheduler_tick_work(&self) {
        assert!(
            self.scheduler_tick_work_generation
                .swap(0, Ordering::AcqRel)
                != 0,
            "scheduler tick work cancellation requires a pending publication"
        );
    }

    pub(crate) fn take_scheduler_tick_work(&self) -> Option<SchedulerTickWorkClaim> {
        let generation = self
            .scheduler_tick_work_generation
            .swap(0, Ordering::AcqRel);
        assert!(
            generation != 0,
            "scheduler tick work consumption requires a pending publication"
        );
        // Keep the timestamp as a monotonic watermark instead of consuming it.
        // A new IRQ may publish after the generation claim but before this
        // load; retaining the watermark lets both the claimed work and any
        // newly queued generation observe a valid timestamp.
        let observed_ns = self.scheduler_tick_observed_ns.load(Ordering::Acquire);
        self.scheduler_tick_work
            .as_ref()
            .filter(|work| work.generation_is_enabled(generation))
            .cloned()
            .map(|work| SchedulerTickWorkClaim::new(work, generation, observed_ns))
    }

    /// Reclaims publication ownership after a transient callback conflict.
    ///
    /// A tick that arrives after [`Self::take_scheduler_tick_work`] may already
    /// have installed the same or a newer generation and published a new
    /// intrusive message. In that case the compare-exchange fails and that
    /// producer owns delivery. A disabled generation is never replayed.
    pub(crate) fn retry_scheduler_tick_work(&self, claim: &SchedulerTickWorkClaim) -> bool {
        if !claim.generation_is_enabled() {
            return false;
        }
        self.scheduler_tick_work_generation
            .compare_exchange(0, claim.generation(), Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn try_claim_reap(&self) -> bool {
        self.reap_gate
            .compare_exchange(0, REAP_CLAIMED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn external_lease_count(&self) -> usize {
        self.reap_signal.external_lease_count()
    }

    /// Reserves one owner-inbox delivery while exit publication is still open.
    ///
    /// The count outlives the producer-side activity guard and is transferred
    /// with the intrusive message. Registry resource teardown observes this
    /// count independently from scheduler-internal `Arc` references.
    pub(crate) fn reserve_scheduler_inbox_delivery(&self) -> bool {
        let Some(_activity) = self.try_scheduler_activity() else {
            return false;
        };
        if self.state() == ThreadState::Exited {
            return false;
        }
        // The AcqRel increment publishes the resource-lifetime reservation
        // before the producer's Release inbox publication. Once exit closes
        // `scheduler_activity_gate`, no new delivery count can appear; the
        // reaper's Acquire load may therefore treat an observed zero as stable.
        self.scheduler_inbox_deliveries
            .try_update(Ordering::AcqRel, Ordering::Acquire, |deliveries| {
                deliveries.checked_add(1)
            })
            .expect("scheduler inbox delivery count overflow");
        true
    }

    /// Cancels a delivery reservation that was not accepted by an inbox.
    pub(crate) fn cancel_scheduler_inbox_delivery(&self) {
        self.finish_scheduler_inbox_delivery();
    }

    /// Takes responsibility for one delivery detached from an owner inbox.
    pub(crate) fn accept_scheduler_inbox_delivery(&self) -> ThreadSchedulerInboxDelivery<'_> {
        assert!(
            self.scheduler_inbox_deliveries.load(Ordering::Acquire) != 0,
            "owner consumed an unreserved scheduler inbox delivery"
        );
        ThreadSchedulerInboxDelivery { core: self }
    }

    pub(crate) fn scheduler_inbox_delivery_count(&self) -> usize {
        self.scheduler_inbox_deliveries.load(Ordering::Acquire)
    }

    pub(crate) fn publish_affinity_completion(&self, generation: u64) -> bool {
        self.affinity_completion.publish(generation)
    }

    pub(crate) fn notify_affinity_waiters(&self) {
        self.affinity_completion.notify_waiters();
    }

    /// Enters one owner-side delivery section that must not overlap exit.
    pub(crate) fn try_scheduler_activity(&self) -> Option<ThreadSchedulerActivity<'_>> {
        let preempt = task_runtime::preempt_guard_enter();
        let mut observed = self.scheduler_activity_gate.load(Ordering::Acquire);
        loop {
            if observed & SCHEDULER_ACTIVITY_CLOSED != 0 {
                release_scheduler_preempt(preempt);
                return None;
            }
            assert!(
                observed < SCHEDULER_ACTIVITY_MAX_READERS,
                "scheduler activity reader count overflow"
            );
            match self.scheduler_activity_gate.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(ThreadSchedulerActivity {
                        core: self,
                        preempt,
                        _not_send: PhantomData,
                    });
                }
                Err(updated) => observed = updated,
            }
        }
    }

    pub(crate) fn close_owned_scheduler_activity(
        self: &Arc<Self>,
    ) -> Option<OwnedThreadSchedulerExit> {
        let preempt = task_runtime::preempt_guard_enter();
        if !self.close_scheduler_activity_gate() {
            release_scheduler_preempt(preempt);
            return None;
        }
        Some(OwnedThreadSchedulerExit {
            core: Arc::clone(self),
            preempt,
            sealed: false,
            _not_send: PhantomData,
        })
    }

    pub(crate) fn cancel_reap_claim(&self) {
        self.reap_gate.store(0, Ordering::Release);
    }

    fn finish_scheduler_activity(&self) {
        let previous = self.scheduler_activity_gate.fetch_sub(1, Ordering::Release);
        assert!(
            previous & SCHEDULER_ACTIVITY_MAX_READERS != 0,
            "unbalanced scheduler activity guard"
        );
    }

    fn close_scheduler_activity_gate(&self) -> bool {
        let previous = self
            .scheduler_activity_gate
            .fetch_or(SCHEDULER_ACTIVITY_CLOSED, Ordering::AcqRel);
        if previous & SCHEDULER_ACTIVITY_CLOSED != 0 {
            return false;
        }
        // Activity guards disable task preemption before incrementing the
        // reader count. Hard-IRQ and scheduler-frame callers are already
        // non-preemptible. Waiting here is therefore the same bounded raw-lock
        // handoff as Linux's task pi_lock: no sleeping owner can retain a
        // reader indefinitely, and no new reader can enter after the close bit.
        while self.scheduler_activity_gate.load(Ordering::Acquire) != SCHEDULER_ACTIVITY_CLOSED {
            core::hint::spin_loop();
        }
        true
    }

    fn reopen_scheduler_activity(&self) {
        assert_eq!(
            self.scheduler_activity_gate.compare_exchange(
                SCHEDULER_ACTIVITY_CLOSED,
                0,
                Ordering::Release,
                Ordering::Acquire,
            ),
            Ok(SCHEDULER_ACTIVITY_CLOSED),
            "only a quiescent uncommitted exit may reopen scheduler activity"
        );
    }

    fn finish_scheduler_inbox_delivery(&self) {
        // AcqRel pairs with the reaper's Acquire count check and also observes
        // an exit state published before the scheduler activity gate reopened.
        // The last delivery republishes task work so a reaper pass that saw a
        // non-zero count cannot become the final, lost retry.
        let previous = self
            .scheduler_inbox_deliveries
            .fetch_sub(1, Ordering::AcqRel);
        assert!(previous != 0, "unbalanced scheduler inbox delivery");
        if previous == 1 && self.reap_signal.exited.load(Ordering::Acquire) {
            self.reap_signal.publish();
        }
    }

    fn try_enter_weak_upgrade(&self) -> bool {
        let mut observed = self.reap_gate.load(Ordering::Acquire);
        loop {
            if observed & REAP_CLAIMED != 0 {
                return false;
            }
            assert!(
                observed < REAP_MAX_UPGRADE_READERS,
                "thread weak-upgrade reader count overflow"
            );
            match self.reap_gate.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(updated) => observed = updated,
            }
        }
    }

    fn exit_weak_upgrade(&self) {
        let previous = self.reap_gate.fetch_sub(1, Ordering::Release);
        assert!(
            previous != 0 && previous & REAP_CLAIMED == 0,
            "unbalanced thread weak-upgrade gate"
        );
    }

    pub(crate) fn publish_base_policy(&self, policy: SchedulePolicy) {
        self.base_policy.store(policy);
    }

    pub(crate) fn publish_effective_schedule(
        &self,
        policy: SchedulePolicy,
        entity: crate::SchedulingEntity,
    ) {
        self.effective_key_sequence.fetch_add(1, Ordering::AcqRel);
        self.effective_policy.store(policy);
        let absolute_deadline_ns = entity
            .deadline()
            .map_or(0, |deadline| deadline.absolute_deadline_ns());
        self.effective_deadline_ns
            .store(absolute_deadline_ns, Ordering::Relaxed);
        self.effective_key_sequence.fetch_add(1, Ordering::Release);
    }

    fn effective_scheduling_key(&self) -> SchedulingKey {
        loop {
            let sequence = self.effective_key_sequence.load(Ordering::Acquire);
            if sequence & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let policy = self.effective_policy.load();
            let absolute_deadline_ns = self.effective_deadline_ns.load(Ordering::Relaxed);
            if self.effective_key_sequence.load(Ordering::Acquire) != sequence {
                continue;
            }
            return match policy {
                SchedulePolicy::Deadline(_) if absolute_deadline_ns != 0 => {
                    SchedulingKey::new(policy.class_rank(), absolute_deadline_ns, self.id.as_u64())
                }
                _ => policy.scheduling_key(self.id.as_u64()),
            };
        }
    }

    fn effective_scheduling_urgency(&self) -> SchedulingUrgency {
        let key = self.effective_scheduling_key();
        SchedulingUrgency::new(key.class_rank(), key.primary())
    }

    pub(crate) fn set_wake_cpu_hint(&self, cpu: CpuId) {
        self.wake_cpu_hint.store(cpu.as_u32(), Ordering::Release);
    }

    pub(crate) fn base_policy(&self) -> SchedulePolicy {
        self.base_policy.load()
    }

    pub(crate) fn wake_cpu_hint(&self) -> Option<CpuId> {
        let cpu = self.wake_cpu_hint.load(Ordering::Acquire);
        (cpu != u32::MAX).then(|| CpuId::new(cpu))
    }

    fn assigned_cpu(&self) -> Option<CpuId> {
        self.sched.assigned_cpu().or_else(|| self.wake_cpu_hint())
    }

    pub(crate) const fn id(&self) -> ThreadId {
        self.id
    }

    pub(crate) const fn extension_view(&self) -> Option<ThreadExtensionView> {
        self.extension
    }

    pub(crate) fn sched(&self) -> &Arc<ThreadSchedCell> {
        &self.sched
    }

    pub(crate) const fn pi_wait_state(&self) -> &PiWaitState {
        &self.pi_wait_state
    }

    pub(crate) const fn policy_update_node(&self) -> &InboxNode {
        &self.policy_update_node
    }

    pub(crate) const fn affinity_update_node(&self) -> &InboxNode {
        &self.affinity_update_node
    }

    pub(crate) const fn deadline_cbs_timer(&self) -> &TaskDeadlineNode {
        &self.deadline_cbs_timer
    }

    pub(crate) const fn deadline_zero_lag_timer(&self) -> &TaskDeadlineNode {
        &self.deadline_zero_lag_timer
    }

    pub(crate) const fn deadline_refresh_node(&self) -> &InboxNode {
        &self.deadline_refresh_node
    }

    pub(crate) const fn migration_node(&self) -> &InboxNode {
        &self.migration_node
    }

    pub(crate) const fn scheduler_tick_work_node(&self) -> &InboxNode {
        &self.scheduler_tick_work_node
    }

    pub(crate) const fn deadline_callback_node(&self) -> &InboxNode {
        &self.deadline_callback_node
    }

    pub(crate) fn publish_wake(&self) -> bool {
        self.wake_state
            .fetch_or(WAKE_STATE_PUBLISHED, Ordering::AcqRel)
            & WAKE_PENDING
            != 0
    }

    pub(crate) fn consume_wake(&self, preserve_park_notification: bool) -> bool {
        let consumed = if preserve_park_notification {
            WAKE_PENDING
        } else {
            WAKE_STATE_PUBLISHED
        };
        self.wake_state.fetch_and(!consumed, Ordering::AcqRel) & WAKE_PENDING != 0
    }

    pub(crate) fn discard_failed_wake(&self) {
        self.wake_state
            .fetch_and(!WAKE_STATE_PUBLISHED, Ordering::AcqRel);
    }

    #[cfg(test)]
    pub(crate) fn wake_is_pending(&self) -> bool {
        self.wake_state.load(Ordering::Acquire) & WAKE_PENDING != 0
    }

    pub(crate) fn register_sleep_timer(&self, cpu: CpuId, generation: u64) {
        self.sleep_timer_cpu.store(cpu.as_u32(), Ordering::Relaxed);
        self.sleep_timer_generation
            .store(generation, Ordering::Release);
    }

    pub(crate) fn sleep_timer_cpu(&self) -> Option<CpuId> {
        let generation = self.sleep_timer_generation.load(Ordering::Acquire);
        if generation == 0 {
            return None;
        }
        let cpu = self.sleep_timer_cpu.load(Ordering::Relaxed);
        (cpu != u32::MAX).then(|| CpuId::new(cpu))
    }

    pub(crate) fn sleep_timer_cpu_for(&self, generation: u64) -> Option<CpuId> {
        (self.sleep_timer_generation.load(Ordering::Acquire) == generation)
            .then(|| self.sleep_timer_cpu.load(Ordering::Relaxed))
            .filter(|cpu| *cpu != u32::MAX)
            .map(CpuId::new)
    }

    pub(crate) fn complete_sleep_timer(&self, generation: u64) -> bool {
        if self
            .sleep_timer_generation
            .compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.sleep_timer_cpu.store(u32::MAX, Ordering::Release);
        true
    }

    pub(crate) fn take_park_notification(&self) -> bool {
        self.wake_state
            .fetch_and(!WAKE_STATE_PUBLISHED, Ordering::AcqRel)
            & PARK_NOTIFIED
            != 0
    }

    pub(crate) fn next_park_generation(&self) -> Result<u64, TaskError> {
        let mut generation = self.park_generation.load(Ordering::Acquire);
        loop {
            let next = generation
                .checked_add(1)
                .ok_or(TaskError::InvalidConfiguration)?;
            match self.park_generation.compare_exchange_weak(
                generation,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(next),
                Err(observed) => generation = observed,
            }
        }
    }

    pub(crate) fn park_generation(&self) -> u64 {
        self.park_generation.load(Ordering::Acquire)
    }

    pub(crate) fn state(&self) -> ThreadState {
        match self.state.load(Ordering::Acquire) {
            0 => ThreadState::New,
            1 => ThreadState::Ready,
            2 => ThreadState::Running,
            3 => ThreadState::Parking,
            4 => ThreadState::Blocked,
            5 => ThreadState::Waking,
            6 => ThreadState::Exited,
            _ => unreachable!("thread state is published only from ThreadState"),
        }
    }
}

#[derive(Debug)]
struct AtomicPolicy {
    sequence: AtomicUsize,
    kind: AtomicU8,
    first: AtomicU64,
    second: AtomicU64,
    third: AtomicU64,
    flags: AtomicU32,
}

impl AtomicPolicy {
    fn new(policy: SchedulePolicy) -> Self {
        let (kind, first, second, third, flags) = encode_policy(policy);
        Self {
            sequence: AtomicUsize::new(0),
            kind: AtomicU8::new(kind),
            first: AtomicU64::new(first),
            second: AtomicU64::new(second),
            third: AtomicU64::new(third),
            flags: AtomicU32::new(flags),
        }
    }

    fn load(&self) -> SchedulePolicy {
        loop {
            let start = self.sequence.load(Ordering::Acquire);
            if start & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let encoded = (
                self.kind.load(Ordering::Relaxed),
                self.first.load(Ordering::Relaxed),
                self.second.load(Ordering::Relaxed),
                self.third.load(Ordering::Relaxed),
                self.flags.load(Ordering::Relaxed),
            );
            if self.sequence.load(Ordering::Acquire) == start {
                return decode_policy(encoded);
            }
        }
    }

    fn store(&self, policy: SchedulePolicy) {
        let (kind, first, second, third, flags) = encode_policy(policy);
        self.sequence.fetch_add(1, Ordering::AcqRel);
        self.kind.store(kind, Ordering::Relaxed);
        self.first.store(first, Ordering::Relaxed);
        self.second.store(second, Ordering::Relaxed);
        self.third.store(third, Ordering::Relaxed);
        self.flags.store(flags, Ordering::Relaxed);
        self.sequence.fetch_add(1, Ordering::Release);
    }
}

fn encode_policy(policy: SchedulePolicy) -> (u8, u64, u64, u64, u32) {
    match policy {
        SchedulePolicy::Fair { nice, mode } => {
            let kind = match mode {
                FairMode::Normal => 0,
                FairMode::Batch => 1,
                FairMode::Idle => 2,
            };
            (kind, nice.get() as i64 as u64, 0, 0, 0)
        }
        SchedulePolicy::Fifo { priority } => (3, priority.get() as u64, 0, 0, 0),
        SchedulePolicy::RoundRobin {
            priority,
            quantum_ns,
        } => (4, priority.get() as u64, quantum_ns, 0, 0),
        SchedulePolicy::Deadline(policy) => (
            5,
            policy.runtime_ns(),
            policy.deadline_ns(),
            policy.period_ns(),
            policy.flags().bits(),
        ),
    }
}

fn decode_policy(encoded: (u8, u64, u64, u64, u32)) -> SchedulePolicy {
    let (kind, first, second, third, flags) = encoded;
    match kind {
        0..=2 => {
            let mode = match kind {
                0 => FairMode::Normal,
                1 => FairMode::Batch,
                _ => FairMode::Idle,
            };
            SchedulePolicy::fair(Nice::new(first as i64 as i8).unwrap_or(Nice::ZERO), mode)
        }
        3 => SchedulePolicy::fifo(
            RtPriority::new(first as u8)
                .unwrap_or_else(|_| RtPriority::new(1).expect("constant RT priority is valid")),
        ),
        4 => SchedulePolicy::round_robin_with_quantum(
            RtPriority::new(first as u8)
                .unwrap_or_else(|_| RtPriority::new(1).expect("constant RT priority is valid")),
            second,
        )
        .unwrap_or_default(),
        5 => {
            let flags = DeadlineFlags::from_bits(flags).unwrap_or(DeadlineFlags::NONE);
            DeadlinePolicy::new(first, second, third, flags)
                .map(SchedulePolicy::deadline)
                .unwrap_or_default()
        }
        _ => SchedulePolicy::default(),
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use super::*;

    fn test_core(id: ThreadId, policy: SchedulePolicy) -> Arc<ThreadCore> {
        let sched = Arc::new(ThreadSchedCell::new_test(id, policy));
        Arc::new(ThreadCore::new(id, policy, sched, None, None, None))
    }

    #[test]
    fn unavailable_wake_without_placement_can_be_retried() {
        let wake = ThreadWakeHandle::from_core(test_core(
            ThreadId::from_parts(0, 1),
            SchedulePolicy::default(),
        ));

        assert_eq!(wake.wake(), WakeResult::Unavailable);
        assert_eq!(wake.wake(), WakeResult::Unavailable);
    }

    #[test]
    fn wake_batch_is_intrusive_and_coalesces_duplicate_threads() {
        let first = test_core(ThreadId::from_parts(0, 1), SchedulePolicy::default());
        let second = test_core(ThreadId::from_parts(1, 1), SchedulePolicy::default());
        let first_wake = ThreadWakeHandle::from_core(Arc::clone(&first));
        let duplicate = first_wake.clone();
        let second_wake = ThreadWakeHandle::from_core(Arc::clone(&second));
        let mut batch = ThreadWakeBatch::new();

        assert!(batch.push(first_wake));
        assert!(!batch.push(duplicate));
        assert!(batch.push(second_wake));
        assert_eq!(batch.len(), 2);
        assert_eq!(batch.wake_all(), 2);
        assert_eq!(first.reap_signal.external_lease_count(), 0);
        assert_eq!(second.reap_signal.external_lease_count(), 0);
    }

    #[test]
    fn reaper_claim_closes_and_reopens_weak_upgrade_on_retry() {
        let handle = ThreadHandle::from_core(test_core(
            ThreadId::from_parts(0, 1),
            SchedulePolicy::default(),
        ));
        let weak = handle.downgrade();

        assert!(handle.core.try_claim_reap());
        assert!(weak.upgrade().is_none());
        handle.core.cancel_reap_claim();
        assert!(weak.upgrade().is_some());
    }

    #[test]
    fn scheduler_exit_closes_before_waiting_for_inflight_activity() {
        let core = test_core(ThreadId::from_parts(0, 1), SchedulePolicy::default());
        let activity = core
            .try_scheduler_activity()
            .expect("fresh thread must accept scheduler activity");
        let closer_core = Arc::clone(&core);
        let entered = Arc::new(AtomicBool::new(false));
        let returned = Arc::new(AtomicBool::new(false));
        let succeeded = Arc::new(AtomicBool::new(false));
        let closer = {
            let entered = Arc::clone(&entered);
            let returned = Arc::clone(&returned);
            let succeeded = Arc::clone(&succeeded);
            std::thread::spawn(move || {
                entered.store(true, Ordering::Release);
                let exit = closer_core.close_owned_scheduler_activity();
                succeeded.store(exit.is_some(), Ordering::Release);
                returned.store(true, Ordering::Release);
                drop(exit);
            })
        };

        while !entered.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
        let closed_before_return = loop {
            let gate = core.scheduler_activity_gate.load(Ordering::Acquire);
            if gate & SCHEDULER_ACTIVITY_CLOSED != 0 {
                break true;
            }
            if returned.load(Ordering::Acquire) {
                break false;
            }
            core::hint::spin_loop();
        };
        drop(activity);
        closer.join().unwrap();

        assert!(
            closed_before_return,
            "exit must reject new activity before waiting for the old reader"
        );
        assert!(
            succeeded.load(Ordering::Acquire),
            "an in-flight reader is not a recoverable exit failure"
        );
    }

    #[test]
    fn exhausted_park_generation_is_rejected_without_wrapping() {
        let core = test_core(ThreadId::from_parts(0, 1), SchedulePolicy::default());
        core.park_generation.store(u64::MAX, Ordering::Release);

        assert_eq!(
            core.next_park_generation(),
            Err(TaskError::InvalidConfiguration)
        );
        assert_eq!(core.park_generation(), u64::MAX);
    }
}
