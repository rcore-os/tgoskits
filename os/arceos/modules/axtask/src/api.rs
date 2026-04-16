//! Task APIs for multi-task configuration.

use alloc::{
    string::String,
    sync::{Arc, Weak},
};

use ax_kernel_guard::NoPreemptIrqSave;

#[cfg(feature = "lockdep")]
pub use crate::lockdep::{HeldLock, HeldLockStack};
pub(crate) use crate::run_queue::{current_run_queue, select_run_queue};
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

/// The wrapper type for [`ax_cpumask::CpuMask`] with SMP configuration.
pub type AxCpuMask = ax_cpumask::CpuMask<{ ax_config::plat::MAX_CPU_NUM }>;

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

/// Gets the current task, or returns [`None`] if the current task is not
/// initialized.
pub fn current_may_uninit() -> Option<CurrentTask> {
    CurrentTask::try_get()
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
    use spin::Lazy;

    static CPU_MASK_FULL: Lazy<AxCpuMask> = Lazy::new(|| {
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
pub fn init_scheduler_secondary() {
    crate::run_queue::init_secondary();
}

/// Handles periodic timer ticks for the task manager.
///
/// For example, advance scheduler states, checks timed events, etc.
#[cfg(feature = "irq")]
#[cfg_attr(doc, doc(cfg(feature = "irq")))]
pub fn on_timer_tick() {
    use ax_kernel_guard::NoOp;
    crate::timers::check_events();
    // Since irq and preemption are both disabled here,
    // we can get current run queue with the default `ax_kernel_guard::NoOp`.
    current_run_queue::<NoOp>().scheduler_timer_tick();
}

/// Adds the given task to the run queue, returns the task reference.
pub fn spawn_task(task: TaskInner) -> AxTaskRef {
    let task_ref = task.into_arc();
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

/// Spawns a new task with the given name and the default stack size ([`ax_config::TASK_STACK_SIZE`]).
///
/// Returns the task reference.
pub fn spawn_with_name<F>(f: F, name: String) -> AxTaskRef
where
    F: FnOnce() + Send + 'static,
{
    spawn_raw(f, name, ax_config::TASK_STACK_SIZE)
}

/// Spawns a new task with the default parameters.
///
/// The default task name is an empty string. The default task stack size is
/// [`ax_config::TASK_STACK_SIZE`].
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
            const MIGRATION_TASK_STACK_SIZE: usize = 4096;
            // Spawn a new migration task for migrating.
            let migration_task = TaskInner::new(
                move || crate::run_queue::migrate_entry(curr),
                "migration-task".into(),
                MIGRATION_TASK_STACK_SIZE,
            )
            .into_arc();

            // Migrate the current task to the correct CPU using the migration task.
            current_run_queue::<NoPreemptIrqSave>().migrate_current(migration_task);

            assert!(
                cpumask.get(ax_hal::percpu::this_cpu_id()),
                "Migration failed"
            );
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
pub fn sleep(dur: core::time::Duration) {
    sleep_until(ax_hal::time::wall_time() + dur);
}

/// Current task is going to sleep, it will be woken up at the given deadline.
///
/// If the feature `irq` is not enabled, it uses busy-wait instead.
pub fn sleep_until(deadline: ax_hal::time::TimeValue) {
    #[cfg(feature = "irq")]
    might_sleep();
    #[cfg(feature = "irq")]
    current_run_queue::<NoPreemptIrqSave>().sleep_until(deadline);
    #[cfg(not(feature = "irq"))]
    ax_hal::time::busy_wait_until(deadline);
}

/// Exits the current task.
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
             preempt_count={}",
            ax_hal::asm::irqs_enabled(),
            current_preempt_count()
        );
    }
}

/// The idle task routine.
///
/// It runs an infinite loop that keeps trying to hand over the CPU before
/// waiting for the next interrupt.
pub fn run_idle() -> ! {
    loop {
        yield_now_unchecked();
        trace!("idle task: waiting for IRQs...");
        #[cfg(feature = "irq")]
        ax_hal::asm::wait_for_irqs();
    }
}
