//! Core task state shared by schedulers and wake paths.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering};

/// CPU identifier used by the bare task core.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct CpuId(pub usize);

/// Fixed-width CPU mask for the core scheduler protocols.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuMask(u64);

impl CpuMask {
    /// Creates a mask with one CPU enabled.
    pub const fn one(cpu: CpuId) -> Self {
        Self(1u64 << cpu.0)
    }

    /// Creates a mask with the lowest `count` CPUs enabled.
    pub const fn first(count: usize) -> Self {
        if count >= 64 {
            Self(u64::MAX)
        } else {
            Self((1u64 << count) - 1)
        }
    }

    /// Creates a CPU mask from raw bits.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Returns whether the mask contains `cpu`.
    pub const fn contains(self, cpu: CpuId) -> bool {
        (self.0 & (1u64 << cpu.0)) != 0
    }

    /// Returns the raw mask bits.
    pub const fn bits(self) -> u64 {
        self.0
    }
}

/// Unique task identifier.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TaskId(pub u64);

impl TaskId {
    /// Allocates a fresh task id.
    pub fn allocate() -> Self {
        static ID_COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Returns the raw numeric id.
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Core task scheduling states.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskState {
    /// Task is currently running.
    Running = 1,
    /// Task is ready to run.
    Ready   = 2,
    /// Task is blocked on an event.
    Blocked = 3,
    /// Task has exited.
    Exited  = 4,
}

impl TaskState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::Running,
            2 => Self::Ready,
            3 => Self::Blocked,
            4 => Self::Exited,
            _ => Self::Exited,
        }
    }
}

/// Shared task reference used by the bare task core.
pub type TaskRef = Arc<TaskCore>;

/// OS-independent task state.
pub struct TaskCore {
    id: TaskId,
    state: AtomicU8,
    cpu_id: AtomicUsize,
    cpumask: AtomicU64,
    on_cpu: AtomicBool,
    in_wait_queue: AtomicBool,
    timer_ticket_id: AtomicU64,
    preempt_pending: AtomicBool,
    force_resched_pending: AtomicBool,
    preempt_disable_count: AtomicUsize,
    interrupted: AtomicBool,
    wake_pending: AtomicBool,
    wake_seq: AtomicU64,
    wake_bits: AtomicU64,
    wake_generation: AtomicU64,
    wake_next: AtomicPtr<()>,
}

impl TaskCore {
    /// Creates a task core in [`TaskState::Ready`].
    pub fn new(id: TaskId, cpu_id: CpuId) -> Self {
        Self {
            id,
            state: AtomicU8::new(TaskState::Ready as u8),
            cpu_id: AtomicUsize::new(cpu_id.0),
            cpumask: AtomicU64::new(CpuMask::one(cpu_id).bits()),
            on_cpu: AtomicBool::new(false),
            in_wait_queue: AtomicBool::new(false),
            timer_ticket_id: AtomicU64::new(0),
            preempt_pending: AtomicBool::new(false),
            force_resched_pending: AtomicBool::new(false),
            preempt_disable_count: AtomicUsize::new(0),
            interrupted: AtomicBool::new(false),
            wake_pending: AtomicBool::new(false),
            wake_seq: AtomicU64::new(0),
            wake_bits: AtomicU64::new(0),
            wake_generation: AtomicU64::new(1),
            wake_next: AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    /// Returns the task id.
    pub const fn id(&self) -> TaskId {
        self.id
    }

    /// Returns the current state.
    pub fn state(&self) -> TaskState {
        TaskState::from_raw(self.state.load(Ordering::Acquire))
    }

    /// Stores a new state.
    pub fn set_state(&self, state: TaskState) {
        self.state.store(state as u8, Ordering::Release);
    }

    /// Performs a state transition if the task is currently in `from`.
    pub fn transition_state(&self, from: TaskState, to: TaskState) -> bool {
        self.state
            .compare_exchange(from as u8, to as u8, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Returns the task's last selected CPU.
    pub fn cpu_id(&self) -> CpuId {
        CpuId(self.cpu_id.load(Ordering::Acquire))
    }

    /// Sets the task's last selected CPU.
    pub fn set_cpu_id(&self, cpu_id: CpuId) {
        self.cpu_id.store(cpu_id.0, Ordering::Release);
    }

    /// Sets the CPU affinity mask.
    pub fn set_cpumask(&self, cpumask: CpuMask) {
        self.cpumask.store(cpumask.bits(), Ordering::Release);
    }

    /// Returns the CPU affinity mask.
    pub fn cpumask(&self) -> CpuMask {
        CpuMask(self.cpumask.load(Ordering::Acquire))
    }

    /// Returns whether the task is marked as present in a wait queue.
    pub fn in_wait_queue(&self) -> bool {
        self.in_wait_queue.load(Ordering::Acquire)
    }

    /// Updates wait-queue membership.
    pub fn set_in_wait_queue(&self, in_wait_queue: bool) {
        self.in_wait_queue.store(in_wait_queue, Ordering::Release);
    }

    /// Returns whether this task is still finishing a context switch on a CPU.
    pub fn on_cpu(&self) -> bool {
        self.on_cpu.load(Ordering::Acquire)
    }

    /// Updates the context-switch ownership marker.
    pub fn set_on_cpu(&self, on_cpu: bool) {
        self.on_cpu.store(on_cpu, Ordering::Release);
    }

    /// Returns the currently armed timer ticket.
    pub fn timer_ticket(&self) -> u64 {
        self.timer_ticket_id.load(Ordering::Acquire)
    }

    /// Stores a non-zero timer ticket.
    pub fn set_timer_ticket(&self, timer_ticket_id: u64) {
        assert!(timer_ticket_id != 0);
        self.timer_ticket_id
            .store(timer_ticket_id, Ordering::Release);
    }

    /// Marks the current timer ticket as expired/cancelled.
    pub fn timer_ticket_expired(&self) {
        self.timer_ticket_id.store(0, Ordering::Release);
    }

    /// Sets ordinary preemption pending state.
    pub fn set_preempt_pending(&self, pending: bool) {
        self.preempt_pending.store(pending, Ordering::Release);
    }

    /// Returns ordinary preemption pending state.
    pub fn preempt_pending(&self) -> bool {
        self.preempt_pending.load(Ordering::Acquire)
    }

    /// Sets force-reschedule pending state.
    pub fn set_force_resched_pending(&self, pending: bool) {
        self.force_resched_pending.store(pending, Ordering::Release);
    }

    /// Returns force-reschedule pending state.
    pub fn force_resched_pending(&self) -> bool {
        self.force_resched_pending.load(Ordering::Acquire)
    }

    /// Atomically consumes force-reschedule pending state.
    pub fn take_force_resched_pending(&self) -> bool {
        self.force_resched_pending.swap(false, Ordering::AcqRel)
    }

    /// Returns the preemption disable nesting depth.
    pub fn preempt_count(&self) -> usize {
        self.preempt_disable_count.load(Ordering::Acquire)
    }

    /// Returns whether preemption is allowed for the expected guard depth.
    pub fn can_preempt(&self, current_disable_count: usize) -> bool {
        self.preempt_count() == current_disable_count
    }

    /// Increments the preemption disable nesting depth.
    pub fn disable_preempt(&self) {
        self.preempt_disable_count.fetch_add(1, Ordering::Release);
    }

    /// Decrements the preemption disable nesting depth and returns true if it reached zero.
    pub fn enable_preempt(&self) -> bool {
        self.preempt_disable_count.fetch_sub(1, Ordering::Release) == 1
    }

    /// Returns whether the task has a pending interrupt.
    pub fn interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Acquire)
    }

    /// Publishes an interrupt to the task.
    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Release);
    }

    /// Clears the interrupt flag.
    pub fn clear_interrupt(&self) {
        self.interrupted.store(false, Ordering::Release);
    }

    /// Atomically consumes the interrupt flag.
    pub fn take_interrupt(&self) -> bool {
        self.interrupted.swap(false, Ordering::AcqRel)
    }

    /// Publishes coalesced wake bits.
    pub fn publish_wake_bits(&self, bits: u64) {
        if bits != 0 {
            self.wake_bits.fetch_or(bits, Ordering::AcqRel);
        }
    }

    /// Bumps the wake sequence and returns the new value.
    pub fn bump_wake_seq(&self) -> u64 {
        self.wake_seq.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Returns the current wake sequence.
    pub fn wake_seq(&self) -> u64 {
        self.wake_seq.load(Ordering::Acquire)
    }

    /// Takes coalesced wake bits.
    pub fn take_wake_bits(&self) -> u64 {
        self.wake_bits.swap(0, Ordering::AcqRel)
    }

    /// Marks the task pending for hard-IRQ wake queue insertion.
    pub fn mark_wake_pending(&self) -> bool {
        !self.wake_pending.swap(true, Ordering::AcqRel)
    }

    /// Clears and returns the previous pending bit.
    pub fn take_wake_pending(&self) -> bool {
        self.wake_pending.swap(false, Ordering::AcqRel)
    }

    /// Returns the wake generation.
    pub fn wake_generation(&self) -> u64 {
        self.wake_generation.load(Ordering::Acquire)
    }

    /// Returns whether `generation` still matches this task.
    pub fn wake_generation_matches(&self, generation: u64) -> bool {
        self.wake_generation() == generation && self.state() != TaskState::Exited
    }

    /// Expires old wake handles.
    pub fn expire_wakers(&self) {
        self.wake_generation.fetch_add(1, Ordering::AcqRel);
        self.wake_pending.store(false, Ordering::Release);
        self.wake_bits.store(0, Ordering::Release);
        self.wake_next
            .store(core::ptr::null_mut(), Ordering::Release);
    }

    /// Stores the intrusive IRQ wake link.
    pub fn set_wake_next<T>(&self, next: *mut T) {
        self.wake_next.store(next.cast::<()>(), Ordering::Release);
    }

    /// Loads the intrusive IRQ wake link.
    pub fn wake_next<T>(&self) -> *mut T {
        self.wake_next.load(Ordering::Acquire).cast::<T>()
    }

    /// Clears the intrusive IRQ wake link.
    pub fn clear_wake_link(&self) {
        self.wake_next
            .store(core::ptr::null_mut(), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::{CpuId, CpuMask, TaskCore, TaskId, TaskState};

    #[test]
    fn task_core_owns_scheduler_and_wake_metadata() {
        let task = TaskCore::new(TaskId(7), CpuId(0));

        assert_eq!(task.state(), TaskState::Ready);
        assert!(task.transition_state(TaskState::Ready, TaskState::Running));
        assert_eq!(task.state(), TaskState::Running);

        task.set_cpu_id(CpuId(2));
        task.set_cpumask(CpuMask::one(CpuId(2)));
        assert_eq!(task.cpu_id(), CpuId(2));
        assert!(task.cpumask().contains(CpuId(2)));

        assert!(!task.in_wait_queue());
        task.set_in_wait_queue(true);
        assert!(task.in_wait_queue());

        task.set_timer_ticket(11);
        assert_eq!(task.timer_ticket(), 11);
        task.timer_ticket_expired();
        assert_eq!(task.timer_ticket(), 0);

        task.set_on_cpu(true);
        assert!(task.on_cpu());
        task.set_on_cpu(false);
        assert!(!task.on_cpu());

        task.set_preempt_pending(true);
        task.set_force_resched_pending(true);
        assert!(task.preempt_pending());
        assert!(task.force_resched_pending());
        assert_eq!(task.preempt_count(), 0);
        task.disable_preempt();
        assert_eq!(task.preempt_count(), 1);
        assert!(!task.can_preempt(0));
        task.enable_preempt();
        assert!(task.can_preempt(0));
        assert!(task.take_force_resched_pending());

        assert!(!task.interrupted());
        task.interrupt();
        assert!(task.interrupted());
        assert!(task.take_interrupt());
        assert!(!task.interrupted());

        task.publish_wake_bits(0b0011);
        task.publish_wake_bits(0b0100);
        assert_eq!(task.take_wake_bits(), 0b0111);
        assert_eq!(task.bump_wake_seq(), 1);
        assert_eq!(task.wake_seq(), 1);
        assert!(task.mark_wake_pending());
        assert!(!task.mark_wake_pending());
        assert!(task.take_wake_pending());
    }
}
