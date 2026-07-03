use alloc::{collections::VecDeque, sync::Arc};
use core::mem::MaybeUninit;
#[cfg(feature = "smp")]
use core::ptr::NonNull;
#[cfg(all(feature = "smp", feature = "ipi"))]
use core::sync::atomic::{AtomicBool, Ordering};

use ax_hal::percpu::this_cpu_id;
use ax_kernel_guard::BaseGuard;
use ax_kspin::{SpinNoIrqGuard, SpinRaw};
use ax_lazyinit::LazyInit;
use ax_memory_addr::VirtAddr;
use ax_sched::BaseScheduler;

use crate::{
    AxCpuMask, AxTaskRef, Scheduler, TaskInner, WaitQueue,
    task::{CurrentTask, TASK_STACK_ALIGN, TaskStack, TaskState},
    wait_queue::WaitQueueGuard,
};

macro_rules! percpu_static {
    ($(
        $(#[$comment:meta])*
        $name:ident: $ty:ty = $init:expr
    ),* $(,)?) => {
        $(
            $(#[$comment])*
            #[ax_percpu::def_percpu]
            static $name: $ty = $init;
        )*
    };
}

percpu_static! {
    RUN_QUEUE: LazyInit<AxRunQueue> = LazyInit::new(),
    EXITED_TASKS: VecDeque<AxTaskRef> = VecDeque::new(),
    WAIT_FOR_EXIT: WaitQueue = WaitQueue::new(),
    IDLE_TASK: LazyInit<AxTaskRef> = LazyInit::new(),
    /// Stores a raw pointer to the previous task running on this CPU.
    /// The pointer is valid only within the window between `switch_to` storing it
    /// and `clear_prev_task_on_cpu` consuming it — both in the same non-preemptible
    /// call chain, so the task cannot be freed while the pointer is held.
    #[cfg(feature = "smp")]
    PREV_TASK: Option<NonNull<crate::AxTask>> = None,
}

/// An array of references to run queues, one for each CPU, indexed by cpu_id.
///
/// This static variable holds references to the run queues for each CPU in the system.
///
/// # Safety
///
/// Access to this variable is marked as `unsafe` because it contains `MaybeUninit` references,
/// which require careful handling to avoid undefined behavior. The array should be fully
/// initialized before being accessed to ensure safe usage.
static mut RUN_QUEUES: [MaybeUninit<&'static mut AxRunQueue>; crate::build_info::CPU_CAPACITY] =
    [ARRAY_REPEAT_VALUE; crate::build_info::CPU_CAPACITY];
#[allow(clippy::declare_interior_mutable_const)] // It's ok because it's used only for initialization `RUN_QUEUES`.
const ARRAY_REPEAT_VALUE: MaybeUninit<&'static mut AxRunQueue> = MaybeUninit::uninit();

#[cfg(not(feature = "host-test"))]
fn main_task_stack() -> TaskStack {
    let (stack_ptr, stack_size) = ax_hal::mem::boot_stack_bounds(this_cpu_id());
    TaskStack::borrowed(stack_ptr, stack_size, TASK_STACK_ALIGN)
}

#[cfg(feature = "host-test")]
fn main_task_stack() -> TaskStack {
    TaskStack::alloc(crate::default_task_stack_size())
}

/// Returns a reference to the current run queue in [`CurrentRunQueueRef`].
///
/// ## Safety
///
/// This function returns a static reference to the current run queue, which
/// is inherently unsafe. It assumes that the `RUN_QUEUE` has been properly
/// initialized and is not accessed concurrently in a way that could cause
/// data races or undefined behavior.
///
/// ## Returns
///
/// * [`CurrentRunQueueRef`] - a static reference to the current [`AxRunQueue`].
#[inline(always)]
pub(crate) fn current_run_queue<G: BaseGuard>() -> CurrentRunQueueRef<'static, G> {
    let irq_state = G::acquire();
    CurrentRunQueueRef {
        inner: unsafe { RUN_QUEUE.current_ref_mut_raw() },
        current_task: crate::current(),
        state: irq_state,
        _phantom: core::marker::PhantomData,
    }
}

/// Selects the run queue index based on a CPU set bitmap and load balancing.
///
/// This function filters the available run queues based on the provided `cpumask` and
/// selects the run queue index for the next task. The selection is based on a round-robin algorithm.
///
/// ## Arguments
///
/// * `cpumask` - A bitmap representing the CPUs that are eligible for task execution.
///
/// ## Returns
///
/// The index (cpu_id) of the selected run queue.
///
/// ## Panics
///
/// This function will panic if `cpu_mask` is empty, indicating that there are no available CPUs for task execution.
#[cfg(feature = "smp")]
// The modulo operation is safe here because `CPU_CAPACITY` is always greater than 1 with "smp" enabled.
#[allow(clippy::modulo_one)]
#[inline]
fn select_run_queue_index(cpumask: AxCpuMask) -> usize {
    use core::sync::atomic::{AtomicUsize, Ordering};
    static RUN_QUEUE_INDEX: AtomicUsize = AtomicUsize::new(0);

    assert!(!cpumask.is_empty(), "No available CPU for task execution");

    // Round-robin selection of the run queue index.
    loop {
        let index =
            RUN_QUEUE_INDEX.fetch_add(1, Ordering::SeqCst) % crate::build_info::CPU_CAPACITY;
        if cpumask.get(index) {
            return index;
        }
    }
}

/// Retrieves a `'static` reference to the run queue corresponding to the given index.
///
/// This function asserts that the provided index is within the range of available CPUs
/// and returns a reference to the corresponding run queue.
///
/// ## Arguments
///
/// * `index` - The index of the run queue to retrieve.
///
/// ## Returns
///
/// A reference to the `AxRunQueue` corresponding to the provided index.
///
/// ## Panics
///
/// This function will panic if the index is out of bounds.
#[cfg(feature = "smp")]
#[inline]
fn get_run_queue(index: usize) -> &'static mut AxRunQueue {
    unsafe { RUN_QUEUES[index].assume_init_mut() }
}

#[cfg(all(feature = "smp", feature = "ipi"))]
#[cfg_attr(all(test, feature = "host-test"), allow(dead_code))]
fn request_current_reschedule() {
    clear_remote_reschedule_pending_for_current_cpu();
    #[cfg(all(feature = "preempt", feature = "host-test"))]
    if let Some(curr) = crate::current_may_uninit() {
        curr.set_force_resched_pending(true);
    }
    #[cfg(all(feature = "preempt", not(feature = "host-test")))]
    if crate::current_may_uninit().is_some() {
        CurrentRunQueueRef::<ax_kernel_guard::NoOp>::force_resched_from_irq();
    }
}

#[cfg(all(test, feature = "smp", feature = "ipi", feature = "host-test"))]
static REMOTE_RESCHEDULE_REQUESTS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

#[cfg(all(
    feature = "smp",
    feature = "ipi",
    not(all(test, feature = "host-test"))
))]
static REMOTE_RESCHEDULE_PENDING: [AtomicBool; crate::build_info::CPU_CAPACITY] =
    [const { AtomicBool::new(false) }; crate::build_info::CPU_CAPACITY];

#[cfg(all(test, feature = "smp", feature = "ipi", feature = "host-test"))]
static REMOTE_RESCHEDULE_PENDING: AtomicBool = AtomicBool::new(false);

#[cfg(all(feature = "smp", feature = "ipi"))]
pub(crate) fn clear_remote_reschedule_pending_for_current_cpu() {
    #[cfg(not(all(test, feature = "host-test")))]
    REMOTE_RESCHEDULE_PENDING[this_cpu_id()].store(false, Ordering::Release);
    #[cfg(all(test, feature = "host-test"))]
    REMOTE_RESCHEDULE_PENDING.store(false, Ordering::Release);
}

#[cfg(all(feature = "smp", feature = "ipi"))]
fn request_remote_reschedule_if_not_pending<F>(pending: &AtomicBool, request: F)
where
    F: FnOnce(),
{
    if !pending.swap(true, Ordering::AcqRel) {
        request();
    }
}

#[cfg(all(feature = "smp", feature = "ipi"))]
fn force_remote_reschedule_request<F>(pending: &AtomicBool, request: F)
where
    F: FnOnce(),
{
    pending.store(true, Ordering::Release);
    request();
}

#[cfg(all(
    feature = "smp",
    feature = "ipi",
    not(all(test, feature = "host-test"))
))]
fn request_remote_reschedule(cpu_id: usize) {
    request_remote_reschedule_if_not_pending(&REMOTE_RESCHEDULE_PENDING[cpu_id], || {
        ax_ipi::run_on_cpu(cpu_id, request_current_reschedule);
    });
}

#[cfg(all(
    feature = "smp",
    feature = "ipi",
    not(all(test, feature = "host-test"))
))]
fn force_remote_reschedule(cpu_id: usize) {
    force_remote_reschedule_request(&REMOTE_RESCHEDULE_PENDING[cpu_id], || {
        ax_ipi::run_on_cpu(cpu_id, request_current_reschedule);
    });
}

#[cfg(all(test, feature = "smp", feature = "ipi", feature = "host-test"))]
fn request_remote_reschedule(cpu_id: usize) {
    let _ = cpu_id;
    // Host tests run with one dummy CPU and a no-op send_ipi(), so record the
    // scheduler-visible request that a real ax-ipi callback would carry.
    request_remote_reschedule_if_not_pending(&REMOTE_RESCHEDULE_PENDING, || {
        REMOTE_RESCHEDULE_REQUESTS.fetch_add(1, Ordering::Release);
    });
}

#[cfg(all(test, feature = "smp", feature = "ipi", feature = "host-test"))]
fn force_remote_reschedule(cpu_id: usize) {
    let _ = cpu_id;
    force_remote_reschedule_request(&REMOTE_RESCHEDULE_PENDING, || {
        REMOTE_RESCHEDULE_REQUESTS.fetch_add(1, Ordering::Release);
    });
}

#[cfg(all(feature = "smp", feature = "ipi"))]
fn kick_remote_cpu(cpu_id: usize) {
    if cpu_id != this_cpu_id() {
        // axruntime's IPI handler only drains ax-ipi callbacks. A bare hardware
        // IPI can wake an idle CPU, but it does not ask a running remote CPU to
        // reschedule after a task is queued there.
        request_remote_reschedule(cpu_id);
    }
}

#[cfg(all(feature = "smp", feature = "ipi"))]
fn force_kick_remote_cpu(cpu_id: usize) {
    if cpu_id != this_cpu_id() {
        force_remote_reschedule(cpu_id);
    }
}

#[cfg(all(test, feature = "smp", feature = "ipi", feature = "host-test"))]
mod tests {
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // Host-test mode collapses per-CPU state into process-global statics, so
    // keep the shared pending/count assertions in one test.
    #[test]
    fn remote_reschedule_request_is_coalesced_and_forced() {
        const REMOTE_CPU: usize = 1;

        super::REMOTE_RESCHEDULE_REQUESTS.store(0, Ordering::Release);
        super::REMOTE_RESCHEDULE_PENDING.store(false, Ordering::Release);

        super::kick_remote_cpu(REMOTE_CPU);

        assert_eq!(
            super::REMOTE_RESCHEDULE_REQUESTS.load(Ordering::Acquire),
            1,
            "remote CPU kicks must enqueue a scheduler-visible reschedule request",
        );
        super::kick_remote_cpu(REMOTE_CPU);

        assert_eq!(
            super::REMOTE_RESCHEDULE_REQUESTS.load(Ordering::Acquire),
            1,
            "remote CPU kicks should coalesce identical pending reschedule requests",
        );

        super::clear_remote_reschedule_pending_for_current_cpu();
        super::kick_remote_cpu(REMOTE_CPU);

        assert_eq!(
            super::REMOTE_RESCHEDULE_REQUESTS.load(Ordering::Acquire),
            2,
            "remote CPU kicks must be accepted again after the pending bit is cleared",
        );

        #[cfg(feature = "preempt")]
        crate::tests::run_in_test_scheduler(|| {
            let curr = crate::current();

            curr.set_preempt_pending(false);
            curr.set_force_resched_pending(false);
            super::REMOTE_RESCHEDULE_PENDING.store(true, Ordering::Release);

            super::request_current_reschedule();

            assert!(
                curr.force_resched_pending_for_test(),
                "remote IPI reschedule must request forced rotation",
            );
            assert!(
                !curr.preempt_pending_for_test(),
                "remote IPI reschedule must not rely on ordinary RR preemption",
            );
            assert!(
                !super::REMOTE_RESCHEDULE_PENDING.load(Ordering::Acquire),
                "remote IPI callback must clear the coalescing bit when it is delivered",
            );

            curr.set_force_resched_pending(false);
            curr.set_preempt_pending(false);
        });

        #[cfg(feature = "preempt")]
        {
            super::kick_remote_cpu(REMOTE_CPU);
            assert_eq!(
                super::REMOTE_RESCHEDULE_REQUESTS.load(Ordering::Acquire),
                3,
                "a delivered remote IPI must allow a later kick to enqueue a new callback",
            );
        }

        super::REMOTE_RESCHEDULE_PENDING.store(false, Ordering::Release);
        super::REMOTE_RESCHEDULE_REQUESTS.store(0, Ordering::Release);
    }

    #[test]
    fn forced_remote_reschedule_bypasses_stale_pending() {
        let pending = AtomicBool::new(true);
        let requests = AtomicUsize::new(0);

        super::force_remote_reschedule_request(&pending, || {
            requests.fetch_add(1, Ordering::Release);
        });

        assert_eq!(
            requests.load(Ordering::Acquire),
            1,
            "forced remote kicks must bypass stale pending coalescing",
        );

        super::request_remote_reschedule_if_not_pending(&pending, || {
            requests.fetch_add(1, Ordering::Release);
        });

        assert_eq!(
            requests.load(Ordering::Acquire),
            1,
            "ordinary remote kicks should still coalesce stale pending requests",
        );

        super::force_remote_reschedule_request(&pending, || {
            requests.fetch_add(1, Ordering::Release);
        });

        assert_eq!(
            requests.load(Ordering::Acquire),
            2,
            "forced remote kicks must not coalesce required migration reschedules",
        );
    }
}

/// Selects the appropriate run queue for the provided task.
///
/// * In a single-core system, this function always returns a reference to the global run queue.
/// * In a multi-core system, this function selects the run queue based on the task's CPU affinity and load balance.
///
/// ## Arguments
///
/// * `task` - A reference to the task for which a run queue is being selected.
///
/// ## Returns
///
/// * [`AxRunQueueRef`] - a static reference to the selected [`AxRunQueue`] (current or remote).
///
/// ## TODO
///
/// 1. Implement better load balancing across CPUs for more efficient task distribution.
/// 2. Use a more generic load balancing algorithm that can be customized or replaced.
#[inline]
pub(crate) fn select_run_queue<G: BaseGuard>(task: &AxTaskRef) -> AxRunQueueRef<'static, G> {
    let irq_state = G::acquire();
    #[cfg(not(feature = "smp"))]
    {
        let _ = task;
        // When SMP is disabled, all tasks are scheduled on the same global run queue.
        AxRunQueueRef {
            inner: unsafe { RUN_QUEUE.current_ref_mut_raw() },
            state: irq_state,
            _phantom: core::marker::PhantomData,
        }
    }
    #[cfg(feature = "smp")]
    {
        // When SMP is enabled, prefer the current CPU to keep the task's
        // cache warm. Fall back to round-robin only when affinity forbids it.
        let current_cpu = this_cpu_id();
        let index = if task.cpumask().get(current_cpu) {
            current_cpu
        } else {
            select_run_queue_index(task.cpumask())
        };
        AxRunQueueRef {
            inner: get_run_queue(index),
            state: irq_state,
            _phantom: core::marker::PhantomData,
        }
    }
}

/// Selects a run queue for waking a blocked task.
///
/// Unlike new task placement, wakeups prefer the CPU that performs the wakeup
/// when the task affinity allows it. This keeps most wakeups local while still
/// falling back to the task's previous CPU or the normal selector if affinity
/// requires it.
#[inline]
pub(crate) fn select_wake_run_queue<G: BaseGuard>(task: &AxTaskRef) -> AxRunQueueRef<'static, G> {
    let irq_state = G::acquire();
    #[cfg(not(feature = "smp"))]
    {
        let _ = task;
        AxRunQueueRef {
            inner: unsafe { RUN_QUEUE.current_ref_mut_raw() },
            state: irq_state,
            _phantom: core::marker::PhantomData,
        }
    }
    #[cfg(feature = "smp")]
    {
        let current_cpu = this_cpu_id();
        let last_cpu = task.cpu_id() as usize;
        let cpumask = task.cpumask();
        let index = if cpumask.get(current_cpu) {
            current_cpu
        } else if last_cpu < crate::build_info::CPU_CAPACITY && cpumask.get(last_cpu) {
            last_cpu
        } else {
            select_run_queue_index(cpumask)
        };
        AxRunQueueRef {
            inner: get_run_queue(index),
            state: irq_state,
            _phantom: core::marker::PhantomData,
        }
    }
}

/// [`AxRunQueue`] represents a run queue for global system or a specific CPU.
pub(crate) struct AxRunQueue {
    /// The ID of the CPU this run queue is associated with.
    cpu_id: usize,
    /// The core scheduler of this run queue.
    /// Since irq and preempt are preserved by the kernel guard hold by `AxRunQueueRef`,
    /// we just use a simple raw spin lock here.
    scheduler: SpinRaw<Scheduler>,
}

/// A reference to the run queue with specific guard.
///
/// Note:
/// [`AxRunQueueRef`] is used to get a reference to the run queue on current CPU
/// or a remote CPU, which is used to add tasks to the run queue or unblock tasks.
/// If you want to perform scheduling operations on the current run queue,
/// see [`CurrentRunQueueRef`].
pub(crate) struct AxRunQueueRef<'a, G: BaseGuard> {
    inner: &'a mut AxRunQueue,
    state: G::State,
    _phantom: core::marker::PhantomData<G>,
}

impl<G: BaseGuard> Drop for AxRunQueueRef<'_, G> {
    fn drop(&mut self) {
        G::release(self.state);
    }
}

/// A reference to the current run queue with specific guard.
///
/// Note:
/// [`CurrentRunQueueRef`] is used to get a reference to the run queue on current CPU,
/// in which scheduling operations can be performed.
pub(crate) struct CurrentRunQueueRef<'a, G: BaseGuard> {
    inner: &'a mut AxRunQueue,
    current_task: CurrentTask,
    state: G::State,
    _phantom: core::marker::PhantomData<G>,
}

impl<G: BaseGuard> Drop for CurrentRunQueueRef<'_, G> {
    fn drop(&mut self) {
        G::release(self.state);
    }
}

/// Management operations for run queue, including adding tasks, unblocking tasks, etc.
impl<G: BaseGuard> AxRunQueueRef<'_, G> {
    /// Adds a task to the scheduler.
    ///
    /// This function is used to add a new task to the scheduler.
    pub fn add_task(&mut self, task: AxTaskRef) {
        let cpu_id = self.inner.cpu_id;
        debug!("task add: {} on run_queue {}", task.id_name(), cpu_id);
        assert!(task.is_ready());
        #[cfg(feature = "smp")]
        task.set_cpu_id(cpu_id as _);
        self.inner.scheduler.lock().add_task(task);
        #[cfg(all(feature = "smp", feature = "ipi"))]
        kick_remote_cpu(cpu_id);
    }

    /// Unblock one task by inserting it into the run queue.
    ///
    /// This function does nothing if the task is not in [`TaskState::Blocked`],
    /// which means the task is already unblocked by other cores.
    pub fn unblock_task(&mut self, task: AxTaskRef, resched: bool) {
        let task_id_name = if log::log_enabled!(log::Level::Debug) {
            Some(task.id_name())
        } else {
            None
        };
        // Try to change the state of the task from `Blocked` to `Ready`,
        // if successful, the task will be put into this run queue,
        // otherwise, the task is already unblocked by other cores.
        // Note:
        // target task can not be insert into the run queue until it finishes its scheduling process.
        if self
            .inner
            .put_task_with_state(task, TaskState::Blocked, resched)
        {
            // Since now, the task to be unblocked is in the `Ready` state.
            let cpu_id = self.inner.cpu_id;
            if let Some(task_id_name) = task_id_name {
                debug!("task unblock: {task_id_name} on run_queue {cpu_id}");
            }
            // Note: when the task is unblocked on another CPU's run queue,
            // we just ignore the `resched` flag.
            if resched && cpu_id == this_cpu_id() {
                #[cfg(feature = "preempt")]
                crate::current().set_preempt_pending(true);
            }
            #[cfg(all(feature = "smp", feature = "ipi"))]
            kick_remote_cpu(cpu_id);
        }
    }
}

/// Core functions of run queue.
impl<G: BaseGuard> CurrentRunQueueRef<'_, G> {
    /// Unblock one task by inserting it into the current CPU's run queue.
    ///
    /// See [`AxRunQueueRef::unblock_task`] for the state-transition details.
    #[cfg(feature = "irq")]
    pub(crate) fn unblock_task(&mut self, task: AxTaskRef, resched: bool) {
        let task_id_name = if log::log_enabled!(log::Level::Debug) {
            Some(task.id_name())
        } else {
            None
        };
        if self
            .inner
            .put_task_with_state(task, TaskState::Blocked, resched)
        {
            let cpu_id = self.inner.cpu_id;
            if let Some(task_id_name) = task_id_name {
                debug!("task unblock: {task_id_name} on run_queue {cpu_id}");
            }
            if resched {
                #[cfg(feature = "preempt")]
                crate::current().set_preempt_pending(true);
            }
        }
    }

    #[cfg(feature = "irq")]
    pub fn scheduler_timer_tick(&mut self) {
        let curr = &self.current_task;
        if !curr.is_idle() && self.inner.scheduler.lock().task_tick(curr) {
            #[cfg(feature = "preempt")]
            curr.set_preempt_pending(true);
        }
    }

    /// Yield the current task and reschedule.
    /// This function will put the current task into this run queue with `Ready` state,
    /// and reschedule to the next task on this run queue.
    pub fn yield_current(&mut self) {
        let curr = &self.current_task;
        trace!("task yield: {}", curr.id_name());
        assert!(curr.is_running());

        #[cfg(feature = "smp")]
        if !curr.cpumask().get(self.inner.cpu_id) {
            self.migrate_current_to_affinity();
            return;
        }

        self.inner
            .put_task_with_state(curr.clone(), TaskState::Running, false);

        self.inner.resched();
    }

    /// Migrate the current task to a new run queue matching its CPU affinity and reschedule.
    /// This function will spawn a new `migration_task` to perform the migration, which will set
    /// current task to `Ready` state and select a proper run queue for it according to its CPU affinity,
    /// switch to the migration task immediately after migration task is prepared.
    ///
    /// Note: the ownership of migrating task (which is current task) is handed over to the migration task,
    /// before the migration task inserted it into the target run queue.
    #[cfg(feature = "smp")]
    pub fn migrate_current(&mut self, migration_task: AxTaskRef) {
        let curr = &self.current_task;
        trace!("task migrate: {}", curr.id_name());
        assert!(curr.is_running());

        // Mark current task's state as `Ready`,
        // but, do not put current task to the scheduler of this run queue.
        curr.set_state(TaskState::Ready);

        // Call `switch_to` to reschedule to the migration task that performs the migration directly.
        self.inner.switch_to(crate::current(), migration_task);
    }

    /// Preempts the current task and reschedules.
    /// This function is used to preempt the current task and reschedule
    /// to next task on current run queue.
    ///
    /// This function is called by `current_check_preempt_pending` with IRQs and preemption disabled.
    ///
    /// Note:
    /// preemption may happened in `enable_preempt`, which is called
    /// each time a [`ax_kspin::NoPreemptGuard`] is dropped.
    #[cfg(feature = "preempt")]
    pub fn preempt_resched(&mut self) {
        // There is no need to disable IRQ and preemption here, because
        // they both have been disabled in `current_check_preempt_pending`.
        let curr = &self.current_task;
        assert!(curr.is_running());

        // When we call `preempt_resched()`, both IRQs and preemption must
        // have been disabled by `ax_kernel_guard::NoPreemptIrqSave`. So we need
        // to set `current_disable_count` to 1 in `can_preempt()` to obtain
        // the preemption permission.
        let can_preempt = curr.can_preempt(1);

        trace!(
            "current task is to be preempted: {}, allow={}",
            curr.id_name(),
            can_preempt
        );
        if can_preempt {
            #[cfg(feature = "smp")]
            if !curr.cpumask().get(self.inner.cpu_id) {
                self.migrate_current_to_affinity();
                return;
            }

            self.inner
                .put_task_with_state(curr.clone(), TaskState::Running, true);
            self.inner.resched();
        } else {
            curr.set_preempt_pending(true);
        }
    }

    #[cfg(feature = "preempt")]
    pub fn force_resched(&mut self) {
        self.force_resched_with_preempt_count(1);
    }

    #[cfg(feature = "preempt")]
    fn force_resched_with_preempt_count(&mut self, current_disable_count: usize) {
        let curr = &self.current_task;
        assert!(curr.is_running());

        let can_preempt = curr.can_preempt(current_disable_count);
        trace!(
            "current task is forced to reschedule: {}, allow={}",
            curr.id_name(),
            can_preempt
        );
        if can_preempt {
            #[cfg(feature = "smp")]
            if !curr.cpumask().get(self.inner.cpu_id) {
                self.migrate_current_to_affinity();
                return;
            }

            self.inner
                .put_task_with_state(curr.clone(), TaskState::Running, false);
            self.inner.resched();
        } else {
            curr.set_force_resched_pending(true);
        }
    }

    #[cfg(all(
        feature = "smp",
        feature = "ipi",
        feature = "preempt",
        not(feature = "host-test")
    ))]
    fn force_resched_from_irq() {
        let mut rq = current_run_queue::<ax_kernel_guard::NoOp>();
        rq.force_resched_with_preempt_count(0);
    }

    /// Exit the current task with the specified exit code.
    /// This function will never return.
    pub fn exit_current(&mut self, exit_code: i32) -> ! {
        let curr = &self.current_task;
        debug!("task exit: {}, exit_code={}", curr.id_name(), exit_code);
        assert!(curr.is_running(), "task is not running: {:?}", curr.state());
        assert!(!curr.is_idle());
        if curr.is_init() {
            // Safety: it is called from `current_run_queue::<NoPreemptIrqSave>().exit_current(exit_code)`,
            // which disabled IRQs and preemption.
            unsafe {
                EXITED_TASKS.current_ref_mut_raw().clear();
            }
            ax_hal::power::system_off();
        } else {
            curr.set_state(TaskState::Exited);

            // Notify the joiner task.
            curr.notify_exit(exit_code);

            // Safety: it is called from `current_run_queue::<NoPreemptIrqSave>().exit_current(exit_code)`,
            // which disabled IRQs and preemption.
            unsafe {
                // Push current task to the `EXITED_TASKS` list, which will be consumed by the GC task.
                EXITED_TASKS.current_ref_mut_raw().push_back(curr.clone());
                // Wake up the GC task to drop the exited tasks.
                WAIT_FOR_EXIT.current_ref_mut_raw().notify_one(false);
            }

            // Schedule to next task.
            self.inner.resched();
        }
        unreachable!("task exited!");
    }

    /// Block the current task, put current task into the wait queue and reschedule.
    /// Mark the state of current task as `Blocked`, set the `in_wait_queue` flag as true.
    /// Note:
    ///     1. The caller must hold the lock of the wait queue.
    ///     2. The caller must ensure that the current task is in the running state.
    ///     3. The caller must ensure that the current task is not the idle task.
    ///     4. The lock of the wait queue will be released explicitly after current task is pushed into it.
    pub fn blocked_resched(&mut self, mut wq_guard: WaitQueueGuard) {
        let curr = &self.current_task;
        assert!(curr.is_running());
        assert!(!curr.is_idle());
        // we must not block current task with preemption disabled.
        // Current expected preempt count is 2.
        // 1 for `NoPreemptIrqSave`, 1 for wait queue's `SpinNoIrq`.
        #[cfg(feature = "preempt")]
        assert!(curr.can_preempt(2));

        // Mark the task as blocked, this has to be done before adding it to the wait queue
        // while holding the lock of the wait queue.
        curr.set_state(TaskState::Blocked);

        // A preemptive future wake can re-enter a wait path before a previous
        // wait-queue entry has been consumed. Avoid leaving a stale duplicate
        // waiter that may receive mutex ownership after the task is running.
        if !curr.in_wait_queue() {
            curr.set_in_wait_queue(true);
            wq_guard.push_back(curr.clone());
        }
        // Drop the lock of wait queue explicitly.
        drop(wq_guard);

        // Current task's state has been changed to `Blocked` and added to the wait queue.
        // Note that the state may have been set as `Ready` in `unblock_task()`,
        // see `unblock_task()` for details.

        debug!("task block: {}", curr.id_name());
        self.inner.resched();
    }

    /// Block the current task, put current task into the wait queue and reschedule.
    /// This is special just for future.
    pub fn future_blocked_resched(&mut self, mut woke: SpinNoIrqGuard<'_, bool>) {
        let curr = &self.current_task;
        assert!(curr.is_running());
        assert!(!curr.is_idle());
        // we must not block current task with preemption disabled.
        // Current expected preempt count is 2 for `NoPreemptIrqSave` and `woke`.
        #[cfg(feature = "preempt")]
        assert!(curr.can_preempt(2));

        // Mark the task as blocked, this has to be done before adding it to the wait queue
        // while holding the lock of the wait queue.
        curr.set_state(TaskState::Blocked);
        *woke = false;
        drop(woke);

        // Current task's state has been changed to `Blocked` and added to the wait queue.
        // Note that the state may have been set as `Ready` in `unblock_task()`,
        // see `unblock_task()` for details.

        debug!("task block: {}", curr.id_name());
        self.inner.resched();
    }

    #[cfg(feature = "irq")]
    pub fn sleep_until(&mut self, deadline: ax_hal::time::TimeValue) {
        let curr = &self.current_task;
        debug!("task sleep: {}, deadline={:?}", curr.id_name(), deadline);
        assert!(curr.is_running());
        assert!(!curr.is_idle());

        while ax_hal::time::monotonic_time() < deadline {
            crate::timers::set_alarm_wakeup(deadline, curr.clone());
            curr.set_state(TaskState::Blocked);
            self.inner.resched();
        }
    }

    pub fn set_current_priority(&mut self, prio: isize) -> bool {
        self.inner
            .scheduler
            .lock()
            .set_priority(&self.current_task, prio)
    }

    #[cfg(feature = "smp")]
    fn migrate_current_to_affinity(&mut self) {
        let curr = self.current_task.clone();
        let migration_task = TaskInner::new(
            move || crate::run_queue::migrate_entry(curr),
            "migration-task".into(),
            crate::default_task_stack_size(),
        )
        .into_arc();

        self.migrate_current(migration_task);
    }
}

impl AxRunQueue {
    /// Create a new run queue for the specified CPU.
    /// The run queue is initialized with a per-CPU gc task in its scheduler.
    fn new(cpu_id: usize) -> Self {
        let gc_task =
            TaskInner::new(gc_entry, "gc".into(), crate::default_task_stack_size()).into_arc();
        // gc task should be pinned to the current CPU.
        gc_task.set_cpumask(AxCpuMask::one_shot(cpu_id));

        let mut scheduler = Scheduler::new();
        scheduler.add_task(gc_task);
        Self {
            cpu_id,
            scheduler: SpinRaw::new(scheduler),
        }
    }

    /// Puts target task into current run queue with `Ready` state
    /// if its state matches `current_state` (except idle task).
    ///
    /// If `preempt`, keep current task's time slice, otherwise reset it.
    ///
    /// Returns `true` if the target task is put into this run queue successfully,
    /// otherwise `false`.
    fn put_task_with_state(
        &mut self,
        task: AxTaskRef,
        current_state: TaskState,
        preempt: bool,
    ) -> bool {
        // If the task's state matches `current_state`, set its state to `Ready` and
        // put it back to the run queue (except idle task).
        if task.transition_state(current_state, TaskState::Ready) && !task.is_idle() {
            #[cfg(feature = "smp")]
            let waking_current_task = current_state == TaskState::Blocked
                && self.cpu_id == this_cpu_id()
                && crate::current().ptr_eq(&task);
            // If the task is blocked, wait for the task to finish its scheduling process.
            // See `unblock_task()` for details.
            if current_state == TaskState::Blocked {
                // Wait for next task's scheduling process to complete.
                // If the owning (remote) CPU is still in the middle of schedule() with
                // this task (next task) as prev, wait until it's done referencing the task.
                //
                // Pairs with the `clear_prev_task_on_cpu()`.
                //
                // Note:
                // 1. This should be placed after the judgement of `TaskState::Blocked,`,
                //    because the task may have been woken up by other cores.
                // 2. This can be placed in the front of `switch_to()`
                #[cfg(feature = "smp")]
                {
                    // A scheduler tracepoint or other IRQ-safe notification can wake the
                    // task that is currently being switched out on this CPU. Waiting for
                    // `on_cpu` there would wait for the very switch we are still inside.
                    if !waking_current_task {
                        while task.on_cpu() {
                            // Wait for the task to finish its scheduling process.
                            core::hint::spin_loop();
                        }
                    }
                }
            }
            // TODO: priority
            #[cfg(feature = "smp")]
            task.set_cpu_id(self.cpu_id as _);
            self.scheduler.lock().put_prev_task(task, preempt);
            true
        } else {
            false
        }
    }

    /// Core reschedule subroutine.
    /// Pick the next task to run and switch to it.
    fn resched(&mut self) {
        let next = self
            .scheduler
            .lock()
            .pick_next_task()
            .unwrap_or_else(|| unsafe {
                // Safety: IRQs must be disabled at this time.
                IDLE_TASK.current_ref_raw().get_unchecked().clone()
            });
        assert!(
            next.is_ready(),
            "next {} is not ready: {:?}",
            next.id_name(),
            next.state()
        );
        self.switch_to(crate::current(), next);
    }

    fn switch_to(&mut self, prev_task: CurrentTask, next_task: AxTaskRef) {
        // Make sure that IRQs are disabled by kernel guard or other means.
        #[cfg(all(feature = "irq", not(feature = "host-test")))]
        assert!(
            !ax_hal::asm::irqs_enabled(),
            "IRQs must be disabled during scheduling"
        );
        trace!(
            "context switch: {} -> {}",
            prev_task.id_name(),
            next_task.id_name()
        );
        #[cfg(feature = "stack-canary")]
        prev_task.check_stack_canary();
        #[cfg(feature = "preempt")]
        next_task.set_preempt_pending(false);
        next_task.set_state(TaskState::Running);
        if prev_task.ptr_eq(&next_task) {
            return;
        }

        // Claim the task as running, we do this before switching to it
        // such that any running task will have this set.
        #[cfg(feature = "smp")]
        next_task.set_on_cpu(true);

        #[cfg(feature = "task-ext")]
        {
            use crate::TaskExt;

            if let Some(ext) = prev_task.task_ext() {
                ext.on_leave()
            }
            if let Some(ext) = next_task.task_ext() {
                ext.on_enter()
            }
        }

        // `prev_task.state()` must be sampled before the architectural switch:
        // callers like `exit_current` already set it to `Exited`/`Blocked`,
        // and that pre-switch state is what `sched:sched_switch` reports.
        #[cfg(feature = "tracepoint-hooks")]
        ax_crate_interface::call_interface!(
            crate::sched_tracepoint::SchedTracepoint::on_sched_switch(
                prev_task.id().as_u64(),
                next_task.id().as_u64(),
                prev_task.state() as u32,
            )
        );

        unsafe {
            let prev_ctx_ptr = prev_task.ctx_mut_ptr();
            let next_ctx_ptr = next_task.ctx_mut_ptr();

            // Store a raw pointer to prev_task in PREV_TASK.
            // Safety: prev_task is alive (Arc held on caller's stack) and will
            // remain so through clear_prev_task_on_cpu() below.
            #[cfg(feature = "smp")]
            {
                *PREV_TASK.current_ref_mut_raw() =
                    Some(NonNull::new(Arc::as_ptr(&prev_task) as *mut _).unwrap());
            }

            // The strong reference count of `prev_task` will be decremented by 1,
            // but won't be dropped until `gc_entry()` is called.
            assert!(Arc::strong_count(&prev_task) > 1);
            assert!(Arc::strong_count(&next_task) >= 1);

            CurrentTask::set_current(prev_task, next_task);

            (*prev_ctx_ptr).switch_to(&*next_ctx_ptr);

            // The current task is now **next_task** on this CPU, so clear `prev_task.on_cpu`
            // to indicate that it has finished its scheduling process and no longer running on this CPU.
            #[cfg(feature = "smp")]
            clear_prev_task_on_cpu();
        }
    }
}

fn gc_entry() {
    loop {
        // Drop all exited tasks and recycle resources.
        let n = EXITED_TASKS.with_current(|exited_tasks| exited_tasks.len());
        for _ in 0..n {
            // Do not do the slow drops in the critical section.
            let task = EXITED_TASKS.with_current(|exited_tasks| exited_tasks.pop_front());
            if let Some(task) = task {
                if Arc::strong_count(&task) == 1 {
                    // If I'm the last holder of the task, drop it immediately.
                    drop(task);
                } else {
                    // Otherwise (e.g, `switch_to` is not completed, held by the
                    // joiner, etc), push it back and wait for them to drop first.
                    EXITED_TASKS.with_current(|exited_tasks| exited_tasks.push_back(task));
                }
            }
        }
        // Always wait with a timeout to:
        // 1. Yield CPU to allow other tasks to complete `switch_to` and drop references
        // 2. Handle the race condition where `notify_one` is called before the GC task enters wait,
        //    causing the notification to be lost.
        // Note: we cannot block current task with preemption disabled,
        // use `current_ref_raw` to get the `WAIT_FOR_EXIT`'s reference here to avoid the use of `NoPreemptGuard`.
        // Since gc task is pinned to the current CPU, there is no effect if the gc task is preempted during the process.
        #[cfg(feature = "irq")]
        unsafe {
            let _timeout = WAIT_FOR_EXIT
                .current_ref_raw()
                .wait_timeout(core::time::Duration::from_millis(100));
        }
        #[cfg(not(feature = "irq"))]
        unsafe {
            WAIT_FOR_EXIT.current_ref_raw().wait();
        }
    }
}

/// The task routine for migrating the current task to the correct CPU.
///
/// It calls `select_run_queue` to get the correct run queue for the task, and
/// then puts the task to the scheduler of target run queue.
#[cfg(feature = "smp")]
pub(crate) fn migrate_entry(migrated_task: AxTaskRef) {
    let rq = select_run_queue::<ax_kernel_guard::NoPreemptIrqSave>(&migrated_task);
    let cpu_id = rq.inner.cpu_id;
    migrated_task.set_cpu_id(cpu_id as _);
    rq.inner
        .scheduler
        .lock()
        .put_prev_task(migrated_task, false);
    #[cfg(all(feature = "smp", feature = "ipi"))]
    // Current-task migration cannot make progress until the target CPU runs
    // the migrated task, so do not let a stale coalescing bit suppress this IPI.
    force_kick_remote_cpu(cpu_id);
}

/// Clear the `on_cpu` field of previous task running on this CPU.
#[cfg(feature = "smp")]
pub(crate) unsafe fn clear_prev_task_on_cpu() {
    let prev = unsafe { PREV_TASK.current_ref_mut_raw() }
        .take()
        .expect("PREV_TASK should have been set by switch_to");
    // Safety: prev_task's Arc is still alive on the caller's stack at this point
    // (switch_to has not yet returned), so the pointer is valid.
    unsafe { prev.as_ref() }.set_on_cpu(false);
}
pub(crate) fn init() {
    let cpu_id = this_cpu_id();

    // Create the `idle` task (not current task).
    // The idle task will run when there is no other runnable task.
    #[cfg(feature = "lockdep")]
    let idle_task_stack_size = crate::default_task_stack_size();
    // TODO: Consider unifying the non-lockdep idle stack size with the task stack configuration.
    #[cfg(not(feature = "lockdep"))]
    let idle_task_stack_size = 16384;
    let idle_task = TaskInner::new(|| crate::run_idle(), "idle".into(), idle_task_stack_size);
    // idle task should be pinned to the current CPU.
    idle_task.set_cpumask(AxCpuMask::one_shot(cpu_id));
    IDLE_TASK.with_current(|i| {
        i.init_once(idle_task.into_arc());
    });

    // Put the subsequent execution into the `main` task.
    let main_task = TaskInner::new_init("main".into(), main_task_stack()).into_arc();
    main_task.set_state(TaskState::Running);
    unsafe { CurrentTask::init_current(main_task) }

    RUN_QUEUE.with_current(|rq| {
        rq.init_once(AxRunQueue::new(cpu_id));
    });
    unsafe {
        RUN_QUEUES[cpu_id].write(RUN_QUEUE.current_ref_mut_raw());
    }
}

pub(crate) fn init_secondary(stack_ptr: VirtAddr, stack_size: usize) {
    let cpu_id = this_cpu_id();

    // Put the subsequent execution into the `idle` task.
    let idle_task = TaskInner::new_init(
        "idle".into(),
        TaskStack::borrowed(stack_ptr, stack_size, TASK_STACK_ALIGN),
    )
    .into_arc();
    idle_task.set_state(TaskState::Running);
    IDLE_TASK.with_current(|i| {
        i.init_once(idle_task.clone());
    });
    unsafe { CurrentTask::init_current(idle_task) }

    RUN_QUEUE.with_current(|rq| {
        rq.init_once(AxRunQueue::new(cpu_id));
    });
    unsafe {
        RUN_QUEUES[cpu_id].write(RUN_QUEUE.current_ref_mut_raw());
    }
}
