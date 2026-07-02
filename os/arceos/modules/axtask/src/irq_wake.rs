//! Task wake support for both hard IRQ and task context.
//!
//! The hard-IRQ path only records state and links a preallocated per-task node
//! into a per-CPU pending list. Scheduler locks are acquired later by the drain
//! path running with the usual task/scheduler guards. Task-context wake uses a
//! separate handle that may call into the scheduler directly.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use ax_hal::percpu::this_cpu_id;
use ax_kernel_guard::NoPreemptIrqSave;
use ax_lazyinit::LazyInit;

#[cfg(all(feature = "smp", any(feature = "ipi", feature = "irq-wake-ipi")))]
use crate::run_queue::kick_remote_cpu_for_irq_wake;
use crate::{AxTask, AxTaskRef, TaskInner, WeakAxTaskRef, current, current_may_uninit};

#[ax_percpu::def_percpu]
static IRQ_WAKE_QUEUE: LazyInit<IrqWakeQueue> = LazyInit::new();

#[cfg(all(test, feature = "host-test"))]
static HOST_TEST_IRQ_WAKE_QUEUE: spin::Once<IrqWakeQueue> = spin::Once::new();

#[ax_percpu::def_percpu]
static IRQ_WAKE_DRAINING: AtomicBool = AtomicBool::new(false);

/// Coalesced wake bits.
pub type WakeBits = u64;

/// Monotonic wake sequence.
pub type WakeSeq = u64;

/// Result returned by wake operations.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WakeResult {
    woke: bool,
    local: bool,
    remote: bool,
}

impl WakeResult {
    /// Returns whether this call transitioned the task into the pending wake list.
    pub const fn woke(self) -> bool {
        self.woke
    }

    /// Returns whether the IRQ epilogue should run scheduler wake processing.
    pub const fn should_resched(self) -> bool {
        self.woke
    }

    /// Returns whether the target task is on the current CPU.
    pub const fn local(self) -> bool {
        self.local
    }

    /// Returns whether the target task is on another CPU.
    pub const fn remote(self) -> bool {
        self.remote
    }
}

#[derive(Clone)]
struct WakeHandle {
    task: WeakAxTaskRef,
    task_id: u64,
    generation: u64,
}

impl WakeHandle {
    fn new(task: AxTaskRef) -> Self {
        Self {
            task_id: task.id().as_u64(),
            generation: task.irq_wake_generation(),
            task: Arc::downgrade(&task),
        }
    }

    fn valid_task(&self) -> Option<AxTaskRef> {
        let task = self.task.upgrade()?;
        if task.id().as_u64() != self.task_id {
            return None;
        }
        if !task.irq_wake_generation_matches(self.generation) {
            return None;
        }
        Some(task)
    }

    fn task_id(&self) -> u64 {
        self.task_id
    }

    const fn generation(&self) -> u64 {
        self.generation
    }

    fn seq(&self) -> WakeSeq {
        self.task.upgrade().map_or(0, |task| task.irq_wake_seq())
    }

    fn take_bits(&self) -> WakeBits {
        self.task
            .upgrade()
            .map_or(0, |task| task.take_irq_wake_bits())
    }
}

/// Cloneable hard-IRQ-safe handle that wakes one kernel task.
///
/// This type is safe to store inside boxed IRQ callbacks. It never calls
/// arbitrary Rust [`core::task::Waker`] implementations and never takes
/// scheduler or wait-queue locks from the IRQ callback.
#[derive(Clone)]
pub struct HardIrqWaker {
    handle: WakeHandle,
}

impl HardIrqWaker {
    /// Returns the task id captured by this waker.
    pub fn task_id(&self) -> u64 {
        self.handle.task_id()
    }

    /// Returns the task generation captured by this waker.
    pub const fn generation(&self) -> u64 {
        self.handle.generation()
    }

    /// Returns the current wake sequence.
    pub fn seq(&self) -> WakeSeq {
        self.handle.seq()
    }

    /// Takes coalesced wake bits.
    pub fn take_bits(&self) -> WakeBits {
        self.handle.take_bits()
    }

    /// Wakes the captured task from hard IRQ context.
    pub fn wake_from_irq(&self, bits: WakeBits) -> WakeResult {
        let Some(task) = self.handle.valid_task() else {
            return WakeResult::default();
        };
        task.publish_irq_wake_bits(bits);
        task.bump_irq_wake_seq();
        if !task.mark_irq_wake_pending() {
            return WakeResult {
                woke: false,
                local: false,
                remote: false,
            };
        }

        let target_cpu = task.cpu_id() as usize;
        let Some(queue) = irq_wake_queue_for_cpu(target_cpu) else {
            task.take_irq_wake_pending();
            return WakeResult::default();
        };
        queue.push(&task);
        #[cfg(all(test, feature = "host-test"))]
        let local = true;
        #[cfg(not(all(test, feature = "host-test")))]
        let local = target_cpu == this_cpu_id();
        #[cfg(all(feature = "smp", any(feature = "ipi", feature = "irq-wake-ipi")))]
        let remote = if !local {
            kick_remote_cpu_for_irq_wake(target_cpu);
            true
        } else {
            false
        };
        #[cfg(not(all(feature = "smp", any(feature = "ipi", feature = "irq-wake-ipi"))))]
        let remote = false;
        WakeResult {
            woke: true,
            local,
            remote,
        }
    }
}

/// Cloneable task-context handle that wakes one kernel task.
///
/// This type may take scheduler locks and must not be called from hard IRQ
/// callbacks. Use [`HardIrqWaker`] for callbacks registered with IRQ dispatch.
#[derive(Clone)]
pub struct TaskWaker {
    handle: WakeHandle,
}

impl TaskWaker {
    pub(crate) fn new(task: AxTaskRef) -> Self {
        Self {
            handle: WakeHandle::new(task),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_hard_irq_waker_for_test(waker: HardIrqWaker) -> Self {
        Self {
            handle: waker.handle,
        }
    }

    /// Creates a hard-IRQ-safe handle for the same task and generation.
    pub fn to_hard_irq_waker(&self) -> HardIrqWaker {
        HardIrqWaker {
            handle: self.handle.clone(),
        }
    }

    /// Returns the task id captured by this waker.
    pub fn task_id(&self) -> u64 {
        self.handle.task_id()
    }

    /// Returns the task generation captured by this waker.
    pub const fn generation(&self) -> u64 {
        self.handle.generation()
    }

    /// Returns the current wake sequence.
    pub fn seq(&self) -> WakeSeq {
        self.handle.seq()
    }

    /// Takes coalesced wake bits.
    pub fn take_bits(&self) -> WakeBits {
        self.handle.take_bits()
    }

    /// Wakes the captured task from task context.
    pub fn wake(&self, bits: WakeBits) -> WakeResult {
        let Some(task) = self.handle.valid_task() else {
            return WakeResult::default();
        };
        task.publish_irq_wake_bits(bits);
        task.bump_irq_wake_seq();

        #[cfg(all(test, feature = "host-test"))]
        let local = true;
        #[cfg(not(all(test, feature = "host-test")))]
        let local = task.cpu_id() as usize == this_cpu_id();

        if current_may_uninit()
            .as_ref()
            .is_some_and(|current| current.ptr_eq(&task))
            && task.transition_state(crate::TaskState::Blocked, crate::TaskState::Running)
        {
            return WakeResult {
                woke: true,
                local,
                remote: false,
            };
        }

        let woke = crate::run_queue::wake_task_from_irq_queue(task);
        WakeResult {
            woke,
            local,
            remote: woke && !local,
        }
    }
}

/// Returns a task-context waker for the current task.
pub fn current_task_waker() -> TaskWaker {
    TaskWaker::new(current().clone())
}

/// Returns a task-context waker for the current task when task state is initialized.
pub fn try_current_task_waker() -> Option<TaskWaker> {
    current_may_uninit().map(|task| TaskWaker::new(task.clone()))
}

/// Returns a hard-IRQ-safe waker for the current task.
pub fn current_hard_irq_waker() -> HardIrqWaker {
    current_task_waker().to_hard_irq_waker()
}

/// Drains the current CPU's IRQ wake queue into the scheduler.
pub fn drain_irq_wake_queue_current_cpu() -> usize {
    let _guard = NoPreemptIrqSave::new();
    #[cfg(all(feature = "smp", any(feature = "ipi", feature = "irq-wake-ipi")))]
    crate::run_queue::clear_remote_irq_wake_pending_for_current_cpu();

    let draining = unsafe { IRQ_WAKE_DRAINING.current_ref_raw() };
    if draining.swap(true, Ordering::AcqRel) {
        return 0;
    }
    let Some(queue) = irq_wake_queue_for_cpu(this_cpu_id()) else {
        draining.store(false, Ordering::Release);
        return 0;
    };
    let mut drained = 0;
    loop {
        while let Some(task) = queue.pop() {
            task.clear_irq_wake_link();
            if !task.take_irq_wake_pending() {
                continue;
            }
            if current_may_uninit()
                .as_ref()
                .is_some_and(|current| current.ptr_eq(&task))
            {
                if task.transition_state(crate::TaskState::Blocked, crate::TaskState::Running) {
                    drained += 1;
                }
                continue;
            }
            if crate::run_queue::wake_task_from_irq_queue(task) {
                drained += 1;
            }
        }

        draining.store(false, Ordering::Release);
        if queue.is_empty()
            || draining
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            break;
        }
    }
    drained
}

pub(crate) fn init_irq_wake_queue_current_cpu() {
    IRQ_WAKE_QUEUE.with_current(|queue| {
        if !queue.is_inited() {
            queue.init_once(IrqWakeQueue::new());
        }
    });
    #[cfg(all(test, feature = "host-test"))]
    HOST_TEST_IRQ_WAKE_QUEUE.call_once(IrqWakeQueue::new);
}

pub(crate) fn expire_task_irq_wakers(task: &TaskInner) {
    task.expire_irq_wakers();
}

fn irq_wake_queue_for_cpu(cpu_id: usize) -> Option<&'static IrqWakeQueue> {
    #[cfg(all(test, feature = "host-test"))]
    {
        let _ = cpu_id;
        HOST_TEST_IRQ_WAKE_QUEUE.get()
    }
    #[cfg(all(feature = "smp", not(all(test, feature = "host-test"))))]
    {
        debug_assert!(cpu_id < crate::build_info::CPU_CAPACITY);
        unsafe { IRQ_WAKE_QUEUE.remote_ref_raw(cpu_id) }.get()
    }
    #[cfg(all(not(feature = "smp"), not(all(test, feature = "host-test"))))]
    {
        let _ = cpu_id;
        unsafe { IRQ_WAKE_QUEUE.current_ref_raw() }.get()
    }
}

struct IrqWakeQueue {
    head: AtomicPtr<AxTask>,
}

impl IrqWakeQueue {
    const fn new() -> Self {
        Self {
            head: AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    fn push(&self, task: &AxTaskRef) {
        let task_ptr = Arc::as_ptr(task) as *mut AxTask;
        // Keep the task alive before publishing the raw pointer. Exactly one
        // extra reference is consumed by the drain path with `Arc::from_raw`.
        unsafe { Arc::increment_strong_count(task_ptr) };
        let mut head = self.head.load(Ordering::Acquire);
        loop {
            task.set_irq_wake_next(head);
            match self.head.compare_exchange_weak(
                head,
                task_ptr,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(next) => {
                    head = next;
                    // The pointer was not published; retain the extra Arc and retry.
                }
            }
        }
    }

    fn pop(&self) -> Option<AxTaskRef> {
        loop {
            let head = self.head.load(Ordering::Acquire);
            if head.is_null() {
                return None;
            }
            let task = unsafe { &*head };
            let next = task.irq_wake_next();
            if self
                .head
                .compare_exchange(head, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(unsafe { Arc::from_raw(head) });
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire).is_null()
    }
}
