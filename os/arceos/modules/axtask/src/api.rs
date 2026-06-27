//! Task APIs for multi-task configuration.

use alloc::{
    collections::BTreeMap,
    string::String,
    sync::{Arc, Weak},
};

#[cfg(feature = "lockdep")]
use ax_kernel_guard::IrqSave;
use ax_kernel_guard::NoPreemptIrqSave;
use ax_memory_addr::VirtAddr;

#[cfg(feature = "lockdep")]
pub use crate::lockdep::{HeldLock, HeldLockStack};
pub(crate) use crate::run_queue::{current_run_queue, select_run_queue, select_wake_run_queue};
#[cfg_attr(doc, doc(cfg(all(feature = "multitask", feature = "task-ext"))))]
#[cfg(feature = "task-ext")]
pub use crate::task::{AxTaskExt, TaskExt};
#[cfg_attr(doc, doc(cfg(all(feature = "multitask", feature = "irq"))))]
#[cfg(feature = "irq")]
pub use crate::timers::register_timer_callback;
#[cfg_attr(doc, doc(cfg(feature = "multitask")))]
pub use crate::{
    task::{CurrentTask, TaskId, TaskInner, TaskState},
    wait_queue::WaitQueue,
};

/// The reference type of a task.
pub type AxTaskRef = Arc<AxTask>;

/// The weak reference type of a task.
pub type WeakAxTaskRef = Weak<AxTask>;

/// Guards a region where mutex ownership validation is relaxed for task
/// teardown. When the guard is live, [`RawMutex::unlock()`] may release a
/// mutex owned by an already-exited task on behalf of that ex-owner.
///
/// This is the **only** legitimate call site for bypassing the "thread must
/// own the mutex it releases" invariant. The guard is created exclusively by
/// the per-CPU gc task when it drops exited [`TaskInner`] references whose
/// drop impl may still hold a [`MutexGuard`]; without this bypass the gc
/// task would trip the `owner_id == current_id` assertion while cleaning up
/// dead owners.
///
/// # Safety
///
/// Must only be constructed from the per-CPU gc task when dropping resources
/// of tasks that have already transitioned to [`TaskState::Exited`]. Every
/// creation must be paired with exactly one destruction (drop).
///
/// [`MutexGuard`]: ax_sync::MutexGuard
/// [`RawMutex::unlock()`]: ax_sync::RawMutex::unlock()
pub struct ForceUnlockGuard(());

impl ForceUnlockGuard {
    /// Enters a force-unlock region.
    ///
    /// # Safety
    ///
    /// Must only be called from the per-CPU gc task path, before dropping
    /// resources of exited tasks.
    #[allow(clippy::new_without_default)]
    pub unsafe fn new() -> Self {
        crate::run_queue::inc_force_unlock();
        Self(())
    }

    /// Returns `true` when the current CPU's gc task is inside a
    /// force-unlock region (i.e., tearing down exited tasks and
    /// permitted to release mutexes on behalf of dead owners on this
    /// CPU).
    ///
    /// The counter is per-CPU, so a GC teardown on one core never
    /// weakens the owner assertion on any other core.
    pub fn is_active() -> bool {
        crate::run_queue::is_force_unlock_active()
    }
}

impl Drop for ForceUnlockGuard {
    fn drop(&mut self) {
        crate::run_queue::dec_force_unlock();
    }
}

#[cfg(feature = "multitask")]
static TASK_REGISTRY: spin::LazyLock<spin::RwLock<BTreeMap<u64, WeakAxTaskRef>>> =
    spin::LazyLock::new(|| spin::RwLock::new(BTreeMap::new()));

/// The wrapper type for [`ax_cpumask::CpuMask`] with SMP configuration.
pub type AxCpuMask = ax_cpumask::CpuMask<{ crate::build_info::CPU_CAPACITY }>;

/// Returns the default stack size used by task creation helpers.
pub fn default_task_stack_size() -> usize {
    crate::build_info::DEFAULT_TASK_STACK_SIZE
}

cfg_if::cfg_if! {
    if #[cfg(feature = "sched-rr")] {
        const MAX_TIME_SLICE: usize = 5;
        pub(crate) type AxTask = ax_sched::RRTask<TaskInner, MAX_TIME_SLICE>;
        pub(crate) type Scheduler = ax_sched::RRScheduler<TaskInner, MAX_TIME_SLICE>;
    } else if #[cfg(feature = "sched-cfs")] {
        pub(crate) type AxTask = ax_sched::CFSTask<TaskInner>;
        pub(crate) type Scheduler = ax_sched::CFScheduler<TaskInner>;
    } else {
        // If no scheduler features are set, use FIFO as the default.
        pub(crate) type AxTask = ax_sched::FifoTask<TaskInner>;
        pub(crate) type Scheduler = ax_sched::FifoScheduler<TaskInner>;
    }
}

#[cfg(feature = "preempt")]
struct KernelGuardIfImpl;

#[cfg(feature = "preempt")]
#[ax_crate_interface::impl_interface]
impl ax_kernel_guard::KernelGuardIf for KernelGuardIfImpl {
    fn disable_preempt() {
        if let Some(curr) = current_may_uninit() {
            curr.disable_preempt();
        }
    }

    fn enable_preempt() {
        if let Some(curr) = current_may_uninit() {
            curr.enable_preempt(true);
        }
    }
}

#[cfg(feature = "lockdep")]
struct KspinLockdepIfImpl;

#[cfg(feature = "lockdep")]
#[ax_crate_interface::impl_interface]
impl ax_kspin::lockdep::KspinLockdepIf for KspinLockdepIfImpl {
    fn collect_current_task_held_locks(snapshot: &mut ax_kspin::lockdep::HeldLockSnapshot) {
        let _lockdep_irq_guard = IrqSave::new();
        if let Some(curr) = current_may_uninit() {
            curr.with_held_locks(|stack| snapshot.extend(stack));
        }
    }

    fn push_current_task_held_lock(held: ax_kspin::lockdep::HeldLock) {
        let _lockdep_irq_guard = IrqSave::new();
        if let Some(curr) = current_may_uninit() {
            curr.with_held_locks(|stack| stack.push(held));
        }
    }

    fn pop_current_task_held_lock(lock_addr: usize) {
        let _lockdep_irq_guard = IrqSave::new();
        if let Some(curr) = current_may_uninit() {
            curr.with_held_locks(|stack| stack.pop_checked(lock_addr));
        }
    }

    fn console_write_str(s: &str) {
        ax_hal::console::write_bytes(s.as_bytes());
    }

    fn fatal() -> ! {
        ax_hal::power::system_off()
    }
}

/// Gets the current task, or returns [`None`] if the current task is not
/// initialized.
pub fn current_may_uninit() -> Option<CurrentTask> {
    CurrentTask::try_get()
}

/// Reports whether the given fault address hits the current task's stack guard page.
#[cfg(feature = "stack-guard-page")]
pub fn diagnose_current_stack_guard_page_fault(fault_addr: VirtAddr) -> bool {
    current_may_uninit().is_some_and(|curr| curr.diagnose_stack_guard_page_fault(fault_addr))
}

/// Gets the current task.
///
/// # Panics
///
/// Panics if the current task is not initialized.
pub fn current() -> CurrentTask {
    CurrentTask::get()
}

#[cfg(feature = "lockdep")]
pub fn with_current_lockdep_stack<R>(f: impl FnOnce(&mut HeldLockStack) -> R) -> R {
    current().with_held_locks(f)
}

/// Initializes the task scheduler (for the primary CPU).
pub fn init_scheduler() {
    info!("Initialize scheduling...");

    // Initialize the run queue.
    crate::run_queue::init();

    info!("  use {} scheduler.", Scheduler::scheduler_name());
}

pub(crate) fn cpu_mask_full() -> AxCpuMask {
    use spin::LazyLock;

    static CPU_MASK_FULL: LazyLock<AxCpuMask> = LazyLock::new(|| {
        let cpu_num = ax_hal::cpu_num();
        let mut cpumask = AxCpuMask::new();
        for cpu_id in 0..cpu_num {
            cpumask.set(cpu_id, true);
        }
        cpumask
    });

    *CPU_MASK_FULL
}

/// Initializes the task scheduler for secondary CPUs.
pub fn init_scheduler_secondary(stack_ptr: VirtAddr, stack_size: usize) {
    crate::run_queue::init_secondary(stack_ptr, stack_size);
}

/// Handles periodic timer ticks for the task manager.
///
/// For example, advance scheduler states, checks timed events, etc.
#[cfg(feature = "irq")]
#[cfg_attr(doc, doc(cfg(feature = "irq")))]
pub fn on_timer_tick() {
    on_timer_irq(true);
}

/// Handles a hardware timer interrupt.
#[cfg(feature = "irq")]
#[cfg_attr(doc, doc(cfg(feature = "irq")))]
pub fn on_timer_irq(scheduler_tick: bool) {
    use ax_kernel_guard::NoOp;
    crate::timers::check_events(scheduler_tick);
    if scheduler_tick {
        // Since irq and preemption are both disabled here,
        // we can get current run queue with the default `ax_kernel_guard::NoOp`.
        current_run_queue::<NoOp>().scheduler_timer_tick();
    }
}

#[cfg(feature = "irq")]
#[doc(hidden)]
pub fn next_timer_deadline_nanos() -> Option<u64> {
    crate::timers::next_deadline_nanos()
}

#[cfg(feature = "irq")]
#[doc(hidden)]
pub fn note_programmed_timer_deadline_nanos(deadline_nanos: u64) {
    crate::timers::note_programmed_deadline_nanos(deadline_nanos);
}

/// Adds the given task to the run queue, returns the task reference.
pub fn spawn_task(task: TaskInner) -> AxTaskRef {
    let task_ref = task.into_arc();
    register_task(&task_ref);
    select_run_queue::<NoPreemptIrqSave>(&task_ref).add_task(task_ref.clone());
    task_ref
}

/// Spawns a new task with the given parameters.
///
/// Returns the task reference.
pub fn spawn_raw<F>(f: F, name: String, stack_size: usize) -> AxTaskRef
where
    F: FnOnce() + Send + 'static,
{
    spawn_task(TaskInner::new(f, name, stack_size))
}

/// Spawns a new task with the given name and the default stack size.
///
/// Returns the task reference.
pub fn spawn_with_name<F>(f: F, name: String) -> AxTaskRef
where
    F: FnOnce() + Send + 'static,
{
    spawn_raw(f, name, default_task_stack_size())
}

/// Spawns a new task with the default parameters.
///
/// The default task name is an empty string. The default task stack size is
/// [`default_task_stack_size`].
///
/// Returns the task reference.
pub fn spawn<F>(f: F) -> AxTaskRef
where
    F: FnOnce() + Send + 'static,
{
    spawn_with_name(f, String::new())
}

/// Set the priority for current task.
///
/// The range of the priority is dependent on the underlying scheduler. For
/// example, in the [CFS] scheduler, the priority is the nice value, ranging from
/// -20 to 19.
///
/// Returns `true` if the priority is set successfully.
///
/// [CFS]: https://en.wikipedia.org/wiki/Completely_Fair_Scheduler
pub fn set_priority(prio: isize) -> bool {
    current_run_queue::<NoPreemptIrqSave>().set_current_priority(prio)
}

/// Set the affinity for the current task.
/// [`AxCpuMask`] is used to specify the CPU affinity.
/// Returns `true` if the affinity is set successfully.
///
/// TODO: support set the affinity for other tasks.
#[track_caller]
pub fn set_current_affinity(cpumask: AxCpuMask) -> bool {
    might_sleep();

    if cpumask.is_empty() {
        false
    } else {
        let curr = current().clone();

        curr.set_cpumask(cpumask);
        // After setting the affinity, we need to check if current cpu matches
        // the affinity. If not, we need to migrate the task to the correct CPU.
        #[cfg(feature = "smp")]
        if !cpumask.get(ax_hal::percpu::this_cpu_id()) {
            // Spawn a new migration task for migrating.
            let migration_task = TaskInner::new(
                move || crate::run_queue::migrate_entry(curr),
                "migration-task".into(),
                default_task_stack_size(),
            )
            .into_arc();

            // Migrate the current task to the correct CPU using the migration task.
            current_run_queue::<NoPreemptIrqSave>().migrate_current(migration_task);
        }
        true
    }
}

/// Current task gives up the CPU time voluntarily, and switches to another
/// ready task.
#[track_caller]
pub fn yield_now() {
    might_sleep();

    yield_now_unchecked();
}

/// Gives up the CPU from a kernel-internal path.
///
/// This bypasses the public `might_sleep()` guard and is intended only for
/// carefully reviewed scheduler or syscall paths that must yield while running
/// under internal kernel guards.
#[doc(hidden)]
pub(crate) fn yield_now_unchecked() {
    current_run_queue::<NoPreemptIrqSave>().yield_current()
}

/// Current task is going to sleep for the given duration.
///
/// If the feature `irq` is not enabled, it uses busy-wait instead.
#[track_caller]
pub fn sleep(dur: core::time::Duration) {
    sleep_until(ax_hal::time::monotonic_time() + dur);
}

/// Current task is going to sleep, it will be woken up at the given deadline.
/// The deadline is measured against the monotonic clock.
///
/// If the feature `irq` is not enabled, it uses busy-wait instead.
#[track_caller]
pub fn sleep_until(deadline: ax_hal::time::TimeValue) {
    #[cfg(feature = "irq")]
    might_sleep();
    #[cfg(feature = "irq")]
    current_run_queue::<NoPreemptIrqSave>().sleep_until(deadline);
    #[cfg(not(feature = "irq"))]
    ax_hal::time::busy_wait_until(deadline);
}

/// Exits the current task.
#[track_caller]
pub fn exit(exit_code: i32) -> ! {
    might_sleep();

    current_run_queue::<NoPreemptIrqSave>().exit_current(exit_code)
}

fn current_preempt_count() -> usize {
    #[cfg(feature = "preempt")]
    {
        current_may_uninit().map_or(0, |curr| curr.preempt_count())
    }
    #[cfg(not(feature = "preempt"))]
    {
        0
    }
}

fn current_task_id() -> Option<u64> {
    current_may_uninit().map(|curr| curr.id().as_u64())
}

/// Returns whether the current context is atomic, meaning sleeping or
/// rescheduling is not allowed.
///
/// This matches the intent of Linux's `might_sleep()`: catch misuse from
/// IRQ-disabled or preempt-disabled regions before a sleep-like action happens.
pub(crate) fn in_atomic_context() -> bool {
    #[cfg(feature = "irq")]
    if !ax_hal::asm::irqs_enabled() {
        return true;
    }

    #[cfg(feature = "preempt")]
    if current_preempt_count() != 0 {
        return true;
    }

    false
}

/// Marks an operation as one that may sleep or reschedule.
///
/// Panics if it is executed in an atomic context.
#[track_caller]
pub fn might_sleep() {
    if in_atomic_context() {
        panic!(
            "sleeping or rescheduling is not allowed in atomic context: irq_enabled={}, \
             preempt_count={}, cpu_id={}, task_id={:?}",
            ax_hal::asm::irqs_enabled(),
            current_preempt_count(),
            ax_hal::percpu::this_cpu_id(),
            current_task_id()
        );
    }
}

/// Wakes a task that may be sleeping, ensuring it can observe a newly-
/// delivered signal.
///
/// `TaskInner::interrupt()` sets the task's interrupt flag and fires the
/// interrupt waker, which unblocks the task via `AxWaker::wake_by_ref`. This
/// covers the common case where the task is blocked in `block_on` with
/// `interruptible` wrapping. For tasks blocked on raw `WaitQueue` objects
/// (which do not register an interrupt waker), this function provides an
/// escape hatch by additionally force-unblocking when the task appears to
/// be parked on a wait queue.
pub fn wake_task(task: &AxTaskRef) {
    // Fire the interrupt: sets the flag and wakes the interrupt_waker.
    // For tasks in block_on (the common case), AxWaker::wake_by_ref already
    // unblocks the task via the registered waker callback.
    task.interrupt();

    // For tasks blocked on a raw WaitQueue, interrupt_waker.wake() is a
    // no-op (no waker registered). Force-unblock by transitioning the task
    // from Blocked to Ready and placing it on the run queue of its
    // affinity CPU.
    //
    // SAFETY: unblock_task uses a CAS on the task state (Blocked → Ready),
    // so if the task is concurrently being woken by its WaitQueue, the CAS
    // fails and this is a harmless no-op. The stale entry in the WaitQueue
    // is benign: when WaitQueue::notify_one eventually pops it, the
    // subsequent unblock_task call will again CAS-fail (task already Ready
    // or Running).
    if task.state() == TaskState::Blocked {
        let mut rq = select_run_queue::<NoPreemptIrqSave>(task);
        rq.unblock_task(task.clone(), false);
    }
}

/// Registers a task for lookup by its scheduler task id.
///
/// This keeps a weak reference only; expired entries are ignored by lookup.
#[cfg(feature = "multitask")]
pub fn register_task(task: &AxTaskRef) {
    TASK_REGISTRY
        .write()
        .insert(task.id().as_u64(), Arc::downgrade(task));
}

/// Finds a task by its scheduler task id.
#[cfg(feature = "multitask")]
pub fn task_by_id(task_id: u64) -> Option<AxTaskRef> {
    if task_id == 0 {
        return current_may_uninit().map(|curr| curr.clone());
    }

    TASK_REGISTRY
        .read()
        .get(&task_id)
        .and_then(|task| task.upgrade())
}

/// Wakes a task by its scheduler task id.
#[cfg(feature = "multitask")]
pub fn wake_task_by_id(task_id: u64) -> bool {
    let Some(task) = task_by_id(task_id) else {
        return false;
    };
    wake_task(&task);
    true
}

#[cfg(not(feature = "multitask"))]
pub fn register_task(_task: &AxTaskRef) {}

#[cfg(not(feature = "multitask"))]
pub fn task_by_id(_task_id: u64) -> Option<AxTaskRef> {
    None
}

#[cfg(not(feature = "multitask"))]
pub fn wake_task_by_id(_task_id: u64) -> bool {
    false
}

/// The idle task routine.
///
/// It runs an infinite loop that keeps trying to hand over the CPU before
/// waiting for the next interrupt.
pub fn run_idle() -> ! {
    loop {
        yield_now_unchecked();
        trace!("idle task: waiting for IRQs...");
        #[cfg(all(feature = "irq", not(feature = "host-test")))]
        ax_hal::asm::wait_for_irqs();
    }
}
