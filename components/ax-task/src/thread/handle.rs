//! Strong, weak, and direct IRQ wake handles.

use alloc::sync::{Arc, Weak};
use core::{
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use crate::{
    CpuId, DeadlineFlags, DeadlinePolicy, FairMode, Nice, RtPriority, SchedulePolicy,
    SchedulingKey, ThreadId, ThreadState,
    inbox::{InboxKind, InboxMessage, InboxNode, PublishResult},
    runtime::{RuntimeCpuId, task_runtime},
    timer::TimerNode,
};

const REAP_CLAIMED: usize = 1 << (usize::BITS - 1);
const REAP_MAX_UPGRADE_READERS: usize = REAP_CLAIMED - 1;

/// A strong reference used to inspect and control a live thread.
#[derive(Clone, Debug)]
pub struct ThreadHandle {
    pub(crate) core: Arc<ThreadCore>,
}

impl ThreadHandle {
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
        ThreadWakeHandle {
            core: Arc::clone(&self.core),
        }
    }

    /// Returns the current scheduling urgency key used by PI waiter ordering.
    pub fn effective_scheduling_key(&self) -> SchedulingKey {
        self.core.effective_scheduling_key()
    }

    /// Returns cumulative charged CPU runtime, including a running residual.
    pub fn runtime_snapshot(&self, now_ns: u64) -> ThreadRuntimeSnapshot {
        self.core.runtime_snapshot(now_ns)
    }

    pub(crate) fn sleep_timer(&self) -> Pin<&TimerNode> {
        // SAFETY: `ThreadCore` is held in an Arc and therefore never moves.
        unsafe { Pin::new_unchecked(&self.core.sleep_timer) }
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
        core.exit_weak_upgrade();
        Some(ThreadHandle { core })
    }
}

/// A stable direct wake header reference.
///
/// [`Self::wake`] performs only bounded atomic operations and is safe in hard IRQ
/// context. Creating, cloning, and dropping this owning reference are task-context
/// operations; coroutine wakers defer their final release to the task-system
/// reaper.
#[derive(Clone, Debug)]
pub struct ThreadWakeHandle {
    pub(crate) core: Arc<ThreadCore>,
}

impl ThreadWakeHandle {
    /// Publishes a wake without allocating, taking a lock, or invoking callbacks.
    pub fn wake(&self) -> WakeResult {
        if self.core.state() == ThreadState::Exited {
            return WakeResult::Exited;
        }
        // This sticky bit closes wake-before-park independently from inbox
        // delivery. The owner consumes it while publishing/committing PARKING.
        self.core.notify_park();
        if self.core.wake_pending.swap(true, Ordering::AcqRel) {
            return WakeResult::AlreadyPending;
        }
        let Some(target) = self.target_cpu() else {
            // No inbox owns this request. Roll the publication bit back so a
            // later placement can retry instead of observing a phantom wake.
            self.core.wake_pending.store(false, Ordering::Release);
            return WakeResult::Unavailable;
        };
        let Some(cpu) = crate::facade::cpu_local_for_wake(target) else {
            self.core.wake_pending.store(false, Ordering::Release);
            return WakeResult::Unavailable;
        };
        let core = Arc::as_ptr(&self.core);
        // SAFETY: this retained strong count is transferred to the inbox
        // payload and released by the owner drain after consuming the node.
        unsafe { Arc::increment_strong_count(core) };
        // SAFETY: Arc allocation addresses are stable. The transferred strong
        // count keeps the embedded node alive until owner-side drain.
        let node = unsafe { Pin::new_unchecked(&(*core).remote_wake_node) };
        let message = InboxMessage::remote_wake_with_payload(
            self.thread_id(),
            target,
            core.expose_provenance(),
        );
        match cpu.publish_remote_wake(node, message) {
            (PublishResult::Published, send_ipi) => {
                // The sticky inbox request remains visible even when the IPI
                // transport reports an error; idle recheck still consumes it.
                if send_ipi {
                    let _status =
                        task_runtime::send_scheduler_ipi(RuntimeCpuId::new(target.as_u32()));
                }
                WakeResult::Notified
            }
            (PublishResult::AlreadyPending, _) => {
                // SAFETY: publication did not take ownership of the retained
                // reference, so this path releases it immediately.
                unsafe { Arc::decrement_strong_count(core) };
                WakeResult::AlreadyPending
            }
            (PublishResult::WrongKind, _) => {
                // SAFETY: publication rejected the node before taking ownership.
                unsafe { Arc::decrement_strong_count(core) };
                self.core.wake_pending.store(false, Ordering::Release);
                WakeResult::Unavailable
            }
        }
    }

    /// Returns the thread that owns this wake header.
    pub fn thread_id(&self) -> ThreadId {
        self.core.id
    }

    /// Returns the CPU most recently selected for direct wake placement.
    pub fn target_cpu(&self) -> Option<CpuId> {
        let cpu = self.core.target_cpu.load(Ordering::Acquire);
        (cpu != u32::MAX).then(|| CpuId::new(cpu))
    }
}

/// Result of an IRQ-safe direct wake publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WakeResult {
    /// This call published a new pending wake.
    Notified,
    /// A pending wake already represents this event.
    AlreadyPending,
    /// The destination thread has exited, so the late wake is ignored.
    Exited,
    /// The target CPU has not published its scheduler inbox yet.
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
pub(crate) struct ThreadCore {
    id: ThreadId,
    base_policy: AtomicPolicy,
    effective_policy: AtomicPolicy,
    effective_key_sequence: AtomicUsize,
    effective_deadline_ns: AtomicU64,
    state: AtomicU8,
    reap_gate: AtomicUsize,
    wake_pending: AtomicBool,
    park_notification: AtomicBool,
    park_generation: AtomicU64,
    target_cpu: AtomicU32,
    remote_wake_node: InboxNode,
    policy_update_node: InboxNode,
    sleep_timer: TimerNode,
    sleep_timer_cpu: AtomicU32,
    sleep_timer_generation: AtomicU64,
    migration_node: InboxNode,
    runtime_sequence: AtomicU64,
    charged_runtime_ns: AtomicU64,
    runtime_accounted_until_ns: AtomicU64,
    runtime_running: AtomicBool,
}

impl ThreadCore {
    pub(crate) fn new(id: ThreadId, policy: SchedulePolicy) -> Self {
        Self {
            id,
            base_policy: AtomicPolicy::new(policy),
            effective_policy: AtomicPolicy::new(policy),
            effective_key_sequence: AtomicUsize::new(0),
            effective_deadline_ns: AtomicU64::new(0),
            state: AtomicU8::new(ThreadState::New as u8),
            reap_gate: AtomicUsize::new(0),
            wake_pending: AtomicBool::new(false),
            park_notification: AtomicBool::new(false),
            park_generation: AtomicU64::new(0),
            target_cpu: AtomicU32::new(u32::MAX),
            remote_wake_node: InboxNode::new(InboxKind::RemoteWake),
            policy_update_node: InboxNode::new(InboxKind::Migration),
            sleep_timer: TimerNode::for_thread(id),
            sleep_timer_cpu: AtomicU32::new(u32::MAX),
            sleep_timer_generation: AtomicU64::new(0),
            migration_node: InboxNode::new(InboxKind::Migration),
            runtime_sequence: AtomicU64::new(0),
            charged_runtime_ns: AtomicU64::new(0),
            runtime_accounted_until_ns: AtomicU64::new(0),
            runtime_running: AtomicBool::new(false),
        }
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
        self.state.store(state as u8, Ordering::Release);
    }

    pub(crate) fn try_claim_reap(&self) -> bool {
        self.reap_gate
            .compare_exchange(0, REAP_CLAIMED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn cancel_reap_claim(&self) {
        self.reap_gate.store(0, Ordering::Release);
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

    pub(crate) fn set_target_cpu(&self, cpu: CpuId) {
        self.target_cpu.store(cpu.as_u32(), Ordering::Release);
    }

    pub(crate) fn target_cpu(&self) -> Option<CpuId> {
        let cpu = self.target_cpu.load(Ordering::Acquire);
        (cpu != u32::MAX).then(|| CpuId::new(cpu))
    }

    pub(crate) const fn id(&self) -> ThreadId {
        self.id
    }

    pub(crate) const fn policy_update_node(&self) -> &InboxNode {
        &self.policy_update_node
    }

    pub(crate) const fn migration_node(&self) -> &InboxNode {
        &self.migration_node
    }

    pub(crate) fn take_wake(&self) -> bool {
        self.wake_pending.swap(false, Ordering::AcqRel)
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

    pub(crate) fn notify_park(&self) {
        self.park_notification.store(true, Ordering::Release);
    }

    pub(crate) fn take_park_notification(&self) -> bool {
        self.park_notification.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn next_park_generation(&self) -> u64 {
        self.park_generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub(crate) fn park_generation(&self) -> u64 {
        self.park_generation.load(Ordering::Acquire)
    }

    fn state(&self) -> ThreadState {
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

    #[test]
    fn unavailable_wake_without_placement_can_be_retried() {
        let wake = ThreadWakeHandle {
            core: Arc::new(ThreadCore::new(
                ThreadId::from_parts(0, 1),
                SchedulePolicy::default(),
            )),
        };

        assert_eq!(wake.wake(), WakeResult::Unavailable);
        assert_eq!(wake.wake(), WakeResult::Unavailable);
    }

    #[test]
    fn reaper_claim_closes_and_reopens_weak_upgrade_on_retry() {
        let handle = ThreadHandle {
            core: Arc::new(ThreadCore::new(
                ThreadId::from_parts(0, 1),
                SchedulePolicy::default(),
            )),
        };
        let weak = handle.downgrade();

        assert!(handle.core.try_claim_reap());
        assert!(weak.upgrade().is_none());
        handle.core.cancel_reap_claim();
        assert!(weak.upgrade().is_some());
    }
}
