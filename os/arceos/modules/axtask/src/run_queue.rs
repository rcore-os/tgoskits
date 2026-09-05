use alloc::{collections::VecDeque, sync::Arc};
#[cfg(all(feature = "smp", feature = "ipi"))]
use core::sync::atomic::{AtomicBool, Ordering};
use core::{mem::MaybeUninit, ops::Deref, ptr::NonNull};

use ax_hal::percpu::{PreviousContextBinding, this_cpu_id};
use ax_lazyinit::LazyInit;
use ax_memory_addr::VirtAddr;
use ax_sched::BaseScheduler;

#[cfg(all(
    feature = "smp",
    feature = "ipi",
    feature = "preempt",
    not(feature = "host-test")
))]
use crate::sync::RawState;
#[cfg(all(feature = "task-ext", feature = "uspace"))]
use crate::task::SchedulerAddressSpaceActivation;
#[cfg(feature = "task-ext")]
use crate::task::TaskExt;
use crate::{
    AxCpuMask, AxTaskRef, Scheduler, TaskInner, WaitQueue,
    sync::{GuardState, SpinLock, SpinLockIrqSaveGuard},
    task::{CurrentTask, TASK_STACK_ALIGN, TaskStack, TaskState},
    wait_queue::WaitQueueGuard,
};

struct PreviousTask {
    task: NonNull<crate::AxTask>,
    binding: PreviousContextBinding,
}

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
    /// Stores the previous task and the exact CPU-binding epoch withdrawn by
    /// the incoming switch tail. The raw pointer is valid only between
    /// `switch_to` and `clear_prev_task_on_cpu`: the scheduler retains an Arc
    /// throughout that non-preemptible handoff.
    PREV_TASK: Option<PreviousTask> = None,
    /// The unique OS-owned activation whose userspace root may remain in this
    /// CPU's TLB. Kernel tasks retain this token and borrow its root exactly as
    /// Linux kernel threads retain `active_mm` in lazy-TLB mode.
    #[cfg(all(feature = "task-ext", feature = "uspace"))]
    ACTIVE_ADDRESS_SPACE: Option<SchedulerAddressSpaceActivation> = None,
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
static mut RUN_QUEUES: [MaybeUninit<NonNull<AxRunQueue>>; crate::build_info::CPU_CAPACITY] =
    [ARRAY_REPEAT_VALUE; crate::build_info::CPU_CAPACITY];
#[allow(clippy::declare_interior_mutable_const)] // It's ok because it's used only for initialization `RUN_QUEUES`.
const ARRAY_REPEAT_VALUE: MaybeUninit<NonNull<AxRunQueue>> = MaybeUninit::uninit();

/// Per-CPU count of scheduler ticks during which a non-idle task was running, for
/// the ondemand cpufreq governor's load metric. Bumped once per timer tick in
/// [`AxRunQueue::scheduler_timer_tick`] when the current task is not the idle task
/// (that path already runs with IRQ + preempt disabled). Read cross-CPU by the
/// governor via [`crate::cpu_busy_ticks`]; `Relaxed` atomics keep the bump a single
/// instruction on the owning CPU while avoiding a data race on the read.
pub(crate) static BUSY_TICKS: [core::sync::atomic::AtomicU64; crate::build_info::CPU_CAPACITY] =
    [const { core::sync::atomic::AtomicU64::new(0) }; crate::build_info::CPU_CAPACITY];

#[cfg(not(feature = "host-test"))]
fn main_task_stack() -> TaskStack {
    let (stack_ptr, stack_size) = ax_hal::mem::boot_stack_bounds(this_cpu_id());
    TaskStack::borrowed(stack_ptr, stack_size, TASK_STACK_ALIGN)
}

#[cfg(feature = "host-test")]
fn main_task_stack() -> TaskStack {
    TaskStack::alloc(crate::default_task_stack_size())
}

/// Acquires guarded access to the current run queue.
#[inline(always)]
pub(crate) fn current_run_queue<G: GuardState>() -> CurrentRunQueueRef<G> {
    let irq_state = G::acquire();
    CurrentRunQueueRef {
        // SAFETY: the acquired guard supplies the scheduler's exclusive local
        // CPU access for the complete CurrentRunQueueRef lifetime.
        inner: unsafe { RunQueueAccess::new(current_run_queue_pointer()) },
        current_task: crate::current(),
        state: irq_state,
        _phantom: core::marker::PhantomData,
    }
}

/// Publishes a newly-installed userspace activation as the sole per-CPU
/// scheduler owner, then retires the previously installed activation.
#[cfg(all(feature = "task-ext", feature = "uspace"))]
pub(crate) fn replace_current_address_space_activation(
    activation: SchedulerAddressSpaceActivation,
    proof: crate::AddressSpaceSwitchProof,
) {
    assert_eq!(proof.cpu(), this_cpu_id());
    // SAFETY: the caller holds a preempt/IRQ guard across the hardware root
    // write and this per-CPU ownership publication.
    let previous = unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| {
            ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                ACTIVE_ADDRESS_SPACE.with_current_mut(exclusive, |slot| slot.replace(activation))
            })
        })
    }
    .expect("address-space publication requires an installed CPU-local area");
    if let Some(previous) = previous {
        previous.release_after_root_switch(proof);
    }
}

/// Releases the current lazy activation after CPU offline installed the stable
/// kernel context and completed a local full TLB flush.
#[cfg(all(feature = "task-ext", feature = "uspace"))]
pub(crate) fn release_current_address_space_after_kernel_switch(
    proof: crate::CpuOfflineRootSwitchProof,
) {
    assert_eq!(proof.cpu(), this_cpu_id());
    // SAFETY: CPU offline excludes migration, interrupts and scheduler entry.
    let activation = unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| {
            ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                ACTIVE_ADDRESS_SPACE.with_current_mut(exclusive, Option::take)
            })
        })
    }
    .expect("address-space withdrawal requires an installed CPU-local area");
    if let Some(activation) = activation {
        activation.release_after_kernel_switch(proof);
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

/// Retrieves the permanent pointer to a run queue by logical CPU index.
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
/// ## Panics
///
/// This function will panic if the index is out of bounds.
#[cfg(feature = "smp")]
#[inline]
fn get_run_queue(index: usize) -> RunQueueAccess {
    // SAFETY: scheduler initialization publishes one permanent pointer for
    // every online CPU before remote scheduling can select it. Callers retain
    // their guard and serialize scheduler state through the embedded lock.
    unsafe { RunQueueAccess::new(RUN_QUEUES[index].assume_init()) }
}

/// Resolves the initialized local run-queue pointer under a scheduler guard.
///
/// # Safety
///
/// The caller must keep migration, IRQ/re-entry, and conflicting local access
/// excluded while the returned pointer is dereferenced.
unsafe fn current_run_queue_pointer() -> NonNull<AxRunQueue> {
    // SAFETY: the caller provides the guard required by both scoped tokens.
    unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| {
            ax_hal::percpu::with_exclusive_cpu(pin, |_exclusive| {
                let mut slot = RUN_QUEUE.current_ptr(pin);
                // SAFETY: scheduler bootstrap initialized this LazyInit before
                // any CurrentRunQueueRef can be constructed.
                NonNull::from(slot.as_mut().get_mut_unchecked())
            })
        })
    }
    .expect("run queue access requires an installed CPU-local area")
}

#[cfg(all(feature = "smp", feature = "ipi"))]
/// Consumes the current CPU's scheduler-owned IPI publication and requests a
/// forced local reschedule when work was pending.
pub fn handle_ipi_reschedule() {
    if !take_remote_reschedule_pending_for_current_cpu() {
        return;
    }
    #[cfg(all(feature = "preempt", feature = "host-test"))]
    if let Some(curr) = crate::current_may_uninit() {
        curr.set_force_resched_pending(true);
    }
    #[cfg(all(feature = "preempt", not(feature = "host-test")))]
    if crate::current_may_uninit().is_some() {
        CurrentRunQueueRef::<RawState>::force_resched_from_irq();
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
fn take_remote_reschedule_pending_for_current_cpu() -> bool {
    #[cfg(not(all(test, feature = "host-test")))]
    let pending = REMOTE_RESCHEDULE_PENDING[this_cpu_id()].swap(false, Ordering::AcqRel);
    #[cfg(all(test, feature = "host-test"))]
    let pending = REMOTE_RESCHEDULE_PENDING.swap(false, Ordering::AcqRel);
    pending
}

#[cfg(all(feature = "smp", feature = "ipi"))]
fn request_remote_reschedule_if_not_pending<F>(
    pending: &AtomicBool,
    request: F,
) -> Result<ax_ipi::IpiNotification, ax_hal::irq::IrqError>
where
    F: FnOnce() -> Result<ax_ipi::IpiNotification, ax_hal::irq::IrqError>,
{
    if pending.swap(true, Ordering::AcqRel) {
        Ok(ax_ipi::IpiNotification::Coalesced)
    } else {
        request()
    }
}

#[cfg(all(feature = "smp", feature = "ipi"))]
fn force_remote_reschedule_request<F>(
    pending: &AtomicBool,
    request: F,
) -> Result<ax_ipi::IpiNotification, ax_hal::irq::IrqError>
where
    F: FnOnce() -> Result<ax_ipi::IpiNotification, ax_hal::irq::IrqError>,
{
    pending.store(true, Ordering::Release);
    request()
}

#[cfg(all(
    feature = "smp",
    feature = "ipi",
    not(all(test, feature = "host-test"))
))]
fn request_remote_reschedule(
    cpu_id: usize,
) -> Result<ax_ipi::IpiNotification, ax_hal::irq::IrqError> {
    request_remote_reschedule_if_not_pending(&REMOTE_RESCHEDULE_PENDING[cpu_id], || {
        ax_ipi::notify_cpu(ax_hal::irq::CpuId(cpu_id))
    })
}

#[cfg(all(
    feature = "smp",
    feature = "ipi",
    not(all(test, feature = "host-test"))
))]
fn force_remote_reschedule(
    cpu_id: usize,
) -> Result<ax_ipi::IpiNotification, ax_hal::irq::IrqError> {
    force_remote_reschedule_request(&REMOTE_RESCHEDULE_PENDING[cpu_id], || {
        ax_ipi::notify_cpu(ax_hal::irq::CpuId(cpu_id))
    })
}

#[cfg(all(test, feature = "smp", feature = "ipi", feature = "host-test"))]
fn request_remote_reschedule(
    cpu_id: usize,
) -> Result<ax_ipi::IpiNotification, ax_hal::irq::IrqError> {
    let _ = cpu_id;
    request_remote_reschedule_if_not_pending(&REMOTE_RESCHEDULE_PENDING, || {
        REMOTE_RESCHEDULE_REQUESTS.fetch_add(1, Ordering::Release);
        Ok(ax_ipi::IpiNotification::Sent)
    })
}

#[cfg(all(test, feature = "smp", feature = "ipi", feature = "host-test"))]
fn force_remote_reschedule(
    cpu_id: usize,
) -> Result<ax_ipi::IpiNotification, ax_hal::irq::IrqError> {
    let _ = cpu_id;
    force_remote_reschedule_request(&REMOTE_RESCHEDULE_PENDING, || {
        REMOTE_RESCHEDULE_REQUESTS.fetch_add(1, Ordering::Release);
        Ok(ax_ipi::IpiNotification::Sent)
    })
}

#[cfg(all(feature = "smp", feature = "ipi"))]
fn kick_remote_cpu(cpu_id: usize) {
    if is_remote_cpu(cpu_id) {
        request_remote_reschedule(cpu_id).unwrap_or_else(|error| {
            panic!("failed to deliver reschedule IPI to CPU {cpu_id}: {error:?}")
        });
    }
}

#[cfg(all(feature = "smp", feature = "ipi"))]
fn force_kick_remote_cpu(cpu_id: usize) {
    if is_remote_cpu(cpu_id) {
        force_remote_reschedule(cpu_id).unwrap_or_else(|error| {
            panic!("failed to deliver forced reschedule IPI to CPU {cpu_id}: {error:?}")
        });
    }
}

#[cfg(all(
    feature = "smp",
    feature = "ipi",
    not(all(test, feature = "host-test"))
))]
fn is_remote_cpu(cpu_id: usize) -> bool {
    cpu_id != this_cpu_id()
}

#[cfg(all(test, feature = "smp", feature = "ipi", feature = "host-test"))]
fn is_remote_cpu(cpu_id: usize) -> bool {
    // The host scheduler models CPU zero. Avoid installing that same modeled
    // CPU on this independent test-harness thread while the scheduler worker
    // is actively publishing its current task.
    cpu_id != 0
}

#[cfg(all(test, feature = "smp", feature = "ipi", feature = "host-test"))]
mod tests {
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // This test covers only the atomic publication edge. Scheduler handling
    // and forced rotation require the real ArceOS IPI path; the task-ipi QEMU
    // case is the runtime evidence boundary rather than a host fake scheduler.
    #[test]
    fn remote_reschedule_request_is_coalesced() {
        let pending = AtomicBool::new(false);
        let requests = AtomicUsize::new(0);

        assert_eq!(
            super::request_remote_reschedule_if_not_pending(&pending, || {
                requests.fetch_add(1, Ordering::Release);
                Ok(ax_ipi::IpiNotification::Sent)
            })
            .unwrap(),
            ax_ipi::IpiNotification::Sent,
        );
        assert_eq!(
            super::request_remote_reschedule_if_not_pending(&pending, || {
                requests.fetch_add(1, Ordering::Release);
                Ok(ax_ipi::IpiNotification::Sent)
            })
            .unwrap(),
            ax_ipi::IpiNotification::Coalesced,
        );
        assert_eq!(
            requests.load(Ordering::Acquire),
            1,
            "remote reschedule requests should coalesce while pending",
        );

        pending.store(false, Ordering::Release);
        assert_eq!(
            super::request_remote_reschedule_if_not_pending(&pending, || {
                requests.fetch_add(1, Ordering::Release);
                Ok(ax_ipi::IpiNotification::Sent)
            })
            .unwrap(),
            ax_ipi::IpiNotification::Sent,
        );
        assert_eq!(
            requests.load(Ordering::Acquire),
            2,
            "a cleared pending bit should permit a fresh request",
        );
    }

    #[test]
    fn forced_remote_reschedule_bypasses_stale_pending() {
        let pending = AtomicBool::new(true);
        let requests = AtomicUsize::new(0);

        super::force_remote_reschedule_request(&pending, || {
            requests.fetch_add(1, Ordering::Release);
            Ok(ax_ipi::IpiNotification::Sent)
        })
        .unwrap();

        assert_eq!(
            requests.load(Ordering::Acquire),
            1,
            "forced remote kicks must bypass stale pending coalescing",
        );

        super::request_remote_reschedule_if_not_pending(&pending, || {
            requests.fetch_add(1, Ordering::Release);
            Ok(ax_ipi::IpiNotification::Sent)
        })
        .unwrap();

        assert_eq!(
            requests.load(Ordering::Acquire),
            1,
            "ordinary remote kicks should still coalesce stale pending requests",
        );

        super::force_remote_reschedule_request(&pending, || {
            requests.fetch_add(1, Ordering::Release);
            Ok(ax_ipi::IpiNotification::Sent)
        })
        .unwrap();

        assert_eq!(
            requests.load(Ordering::Acquire),
            2,
            "forced remote kicks must not coalesce required migration reschedules",
        );
    }

    #[test]
    fn remote_reschedule_send_failure_is_reported_and_keeps_pending() {
        let pending = AtomicBool::new(false);

        let result = super::request_remote_reschedule_if_not_pending(&pending, || {
            Err(ax_hal::irq::IrqError::Controller)
        });

        assert_eq!(result, Err(ax_hal::irq::IrqError::Controller));
        assert!(
            pending.load(Ordering::Acquire),
            "the scheduler publication must survive a physical IPI delivery failure",
        );
    }
}

#[cfg(all(test, feature = "sched-rr", feature = "host-test"))]
mod rr_tests {
    use alloc::{string::String, sync::Arc};
    use core::{marker::PhantomData, ptr::NonNull};

    use ax_sched::BaseScheduler;

    use super::{AxRunQueue, AxRunQueueRef, RunQueueAccess, Scheduler, SpinLock, TaskInner};
    use crate::{sync::RawState, task::TaskState};

    fn new_test_task(name: &str, state: TaskState) -> crate::AxTaskRef {
        let task =
            TaskInner::new(|| {}, String::from(name), crate::default_task_stack_size()).into_arc();
        task.set_state(state);
        task
    }

    #[test]
    fn unblock_resched_does_not_front_insert_rr_task() {
        ax_hal::percpu::initialize_host_test_cpu();
        let mut run_queue = AxRunQueue {
            cpu_id: 1,
            scheduler: SpinLock::new(Scheduler::new()),
        };
        let queued = new_test_task("queued", TaskState::Ready);
        let blocked = new_test_task("blocked", TaskState::Blocked);

        // SAFETY: this host-side fixture is single-threaded and cannot re-enter
        // the scheduler while the guard is alive.
        unsafe { run_queue.scheduler.lock_raw() }.add_task(queued.clone());
        {
            let mut run_queue_ref = AxRunQueueRef::<RawState> {
                // SAFETY: the stack run queue outlives this guarded test handle.
                inner: unsafe { RunQueueAccess::new(NonNull::from(&mut run_queue)) },
                state: (),
                _phantom: PhantomData,
            };
            run_queue_ref.unblock_task(blocked, true);
        }

        // SAFETY: this host-side fixture is single-threaded and non-reentrant.
        let next = unsafe { run_queue.scheduler.lock_raw() }
            .pick_next_task()
            .unwrap();
        assert!(
            Arc::ptr_eq(&next, &queued),
            "waking a blocked task with resched=true must not move it ahead of already queued RR \
             tasks",
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
/// * [`AxRunQueueRef`] - guarded access to the selected current or remote run queue.
///
/// ## TODO
///
/// 1. Implement better load balancing across CPUs for more efficient task distribution.
/// 2. Use a more generic load balancing algorithm that can be customized or replaced.
#[inline]
pub(crate) fn select_run_queue<G: GuardState>(task: &AxTaskRef) -> AxRunQueueRef<G> {
    let irq_state = G::acquire();
    #[cfg(not(feature = "smp"))]
    {
        let _ = task;
        // When SMP is disabled, all tasks are scheduled on the same global run queue.
        AxRunQueueRef {
            // SAFETY: `irq_state` retains G's exclusive scheduler guard.
            inner: unsafe { RunQueueAccess::new(current_run_queue_pointer()) },
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
pub(crate) fn select_wake_run_queue<G: GuardState>(task: &AxTaskRef) -> AxRunQueueRef<G> {
    let irq_state = G::acquire();
    #[cfg(not(feature = "smp"))]
    {
        let _ = task;
        AxRunQueueRef {
            // SAFETY: `irq_state` retains G's exclusive scheduler guard.
            inner: unsafe { RunQueueAccess::new(current_run_queue_pointer()) },
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
    scheduler: SpinLock<Scheduler>,
}

/// Permanent run-queue pointer whose references remain scoped to this handle.
struct RunQueueAccess(NonNull<AxRunQueue>);

impl RunQueueAccess {
    /// Creates guarded access from an initialized permanent pointer.
    ///
    /// # Safety
    ///
    /// The pointer must remain live while this handle exists. The surrounding
    /// scheduler guard and the embedded scheduler lock must serialize access.
    unsafe fn new(pointer: NonNull<AxRunQueue>) -> Self {
        Self(pointer)
    }
}

impl Deref for RunQueueAccess {
    type Target = AxRunQueue;

    fn deref(&self) -> &Self::Target {
        // SAFETY: construction requires a live permanent pointer; the returned
        // shared borrow cannot outlive this guarded handle.
        unsafe { self.0.as_ref() }
    }
}

/// A reference to the run queue with specific guard.
///
/// Note:
/// [`AxRunQueueRef`] is used to get a reference to the run queue on current CPU
/// or a remote CPU, which is used to add tasks to the run queue or unblock tasks.
/// If you want to perform scheduling operations on the current run queue,
/// see [`CurrentRunQueueRef`].
pub(crate) struct AxRunQueueRef<G: GuardState> {
    inner: RunQueueAccess,
    state: G::State,
    _phantom: core::marker::PhantomData<G>,
}

impl<G: GuardState> Drop for AxRunQueueRef<G> {
    fn drop(&mut self) {
        G::release(self.state);
    }
}

/// A reference to the current run queue with specific guard.
///
/// Note:
/// [`CurrentRunQueueRef`] is used to get a reference to the run queue on current CPU,
/// in which scheduling operations can be performed.
pub(crate) struct CurrentRunQueueRef<G: GuardState> {
    inner: RunQueueAccess,
    current_task: CurrentTask,
    state: G::State,
    _phantom: core::marker::PhantomData<G>,
}

impl<G: GuardState> Drop for CurrentRunQueueRef<G> {
    fn drop(&mut self) {
        G::release(self.state);
    }
}

/// Management operations for run queue, including adding tasks, unblocking tasks, etc.
impl<G: GuardState> AxRunQueueRef<G> {
    /// Adds a task to the scheduler.
    ///
    /// This function is used to add a new task to the scheduler.
    pub fn add_task(&mut self, task: AxTaskRef) {
        let cpu_id = self.inner.cpu_id;
        debug!("task add: {} on run_queue {}", task.id_name(), cpu_id);
        assert!(task.is_ready());
        #[cfg(feature = "smp")]
        task.set_cpu_id(cpu_id as _);
        // SAFETY: `AxRunQueueRef<G>` has already entered the run-queue
        // critical section represented by `G`.
        unsafe { self.inner.scheduler.lock_raw() }.add_task(task);
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
            // A wakeup is not a time-slice preemption of the woken task.
            .put_task_with_state(task, TaskState::Blocked, false)
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
impl<G: GuardState> CurrentRunQueueRef<G> {
    /// Unblock one task by inserting it into the current CPU's run queue.
    ///
    /// See [`AxRunQueueRef::unblock_task`] for the state-transition details.
    pub(crate) fn unblock_task(&mut self, task: AxTaskRef, resched: bool) {
        let task_id_name = if log::log_enabled!(log::Level::Debug) {
            Some(task.id_name())
        } else {
            None
        };
        if self
            .inner
            // A wakeup is not a time-slice preemption of the woken task.
            .put_task_with_state(task, TaskState::Blocked, false)
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

    pub fn scheduler_timer_tick(&mut self) {
        let curr = &self.current_task;
        if !curr.is_idle() {
            // Ondemand-governor load accounting: this CPU ran a real (non-idle)
            // task this tick. Already IRQ + preempt off here; a single relaxed
            // fetch_add is essentially free.
            if let Some(t) = BUSY_TICKS.get(this_cpu_id()) {
                t.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            }
            // SAFETY: `CurrentRunQueueRef<G>` owns the run-queue critical
            // section for this operation.
            if unsafe { self.inner.scheduler.lock_raw() }.task_tick(curr) {
                #[cfg(feature = "preempt")]
                curr.set_preempt_pending(true);
            }
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

        // Switch to the migration task through the same scheduler-frame
        // transaction used by ordinary rescheduling.
        let mut scheduler_frame = crate::runtime_preempt::SchedulerFrame::enter();
        let result = self
            .inner
            .switch_to(crate::current(), migration_task, &mut scheduler_frame);
        scheduler_frame.finish(result);
    }

    /// Preempts the current task and reschedules.
    /// This function is used to preempt the current task and reschedule
    /// to next task on current run queue.
    ///
    /// This function is called by `current_check_preempt_pending` with IRQs and preemption disabled.
    ///
    /// Note:
    /// preemption may happened in `enable_preempt`, which is called
    /// each time a preemption guard is dropped.
    #[cfg(feature = "preempt")]
    pub fn preempt_resched(&mut self) {
        // There is no need to disable IRQ and preemption here, because
        // they both have been disabled in `current_check_preempt_pending`.
        let curr = &self.current_task;
        assert!(curr.is_running());

        // When we call `preempt_resched()`, both IRQs and preemption must
        // have been disabled by `crate::sync::PreemptIrqSaveState`. So we need
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
        let mut rq = current_run_queue::<RawState>();
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
            // SAFETY: CurrentRunQueueRef's guard disables migration and local
            // IRQ/re-entry for this complete mutation.
            unsafe {
                ax_hal::percpu::with_cpu_pin(|pin| {
                    ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                        EXITED_TASKS.with_current_mut(exclusive, VecDeque::clear)
                    })
                })
            }
            .expect("task exit requires an installed CPU-local area");
            ax_hal::power::system_off();
        } else {
            curr.set_state(TaskState::Exited);

            #[cfg(feature = "task-ext")]
            if let Some(ext) = curr.task_ext() {
                // The task is no longer eligible for any run queue, while its
                // architectural context and activation are still installed.
                // This is the only safe point for an extension to release
                // task-local lifetime pins without making a retiring context
                // schedulable again.
                ext.on_exit();
            }

            // Notify the joiner task.
            curr.notify_exit(exit_code);

            // SAFETY: CurrentRunQueueRef's guard disables migration and local
            // IRQ/re-entry for both mutations.
            unsafe {
                ax_hal::percpu::with_cpu_pin(|pin| {
                    ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                        // Push current task to the list consumed by the GC task.
                        EXITED_TASKS
                            .with_current_mut(exclusive, |tasks| tasks.push_back(curr.clone()));
                        WAIT_FOR_EXIT.with_current_mut(exclusive, |wait| wait.notify_one(false));
                    })
                })
            }
            .expect("task exit requires an installed CPU-local area");

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
    pub fn future_blocked_resched(&mut self, mut woke: SpinLockIrqSaveGuard<'_, bool>) {
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
        // SAFETY: `CurrentRunQueueRef<G>` owns the run-queue critical section.
        unsafe { self.inner.scheduler.lock_raw() }.set_priority(&self.current_task, prio)
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
            scheduler: SpinLock::new(scheduler),
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
        &self,
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
            // A blocked task woken here may still be finishing its context
            // switch-out on its owning CPU: `on_cpu == true` means its registers
            // are not yet fully saved. It must NOT be made runnable (enqueued)
            // until `on_cpu` is false, or another CPU could resume it with stale
            // registers. Pairs with `clear_prev_task_on_cpu()`.
            //
            // We must NOT busy-spin on `on_cpu` for a task owned by a *remote*
            // CPU (as the old code did): two CPUs each spinning with IRQs off,
            // each waiting for the other to reach `clear_prev_task_on_cpu()`, is
            // a mutual deadlock (whole-board freeze). Instead, hand the enqueue
            // to the owning CPU via a lock-free stash it drains once its context
            // is saved. `waking_current_task` (self-wake on this CPU, mid-switch)
            // keeps the old inline behavior: this CPU finishes the switch in
            // program order when it returns.
            #[cfg(feature = "smp")]
            if current_state == TaskState::Blocked && !waking_current_task && task.on_cpu() {
                // Record where the task must land, then stash a reference for the
                // owning CPU to enqueue from `clear_prev_task_on_cpu()`.
                task.set_cpu_id(self.cpu_id as _);
                task.stash_wake(task.clone());
                // Re-check under the SeqCst handshake. If still on its owning CPU,
                // that CPU drains the stash after its switch completes — done.
                if task.on_cpu() {
                    return false;
                }
                // `on_cpu` cleared meanwhile: the owning CPU may already have
                // passed its drain point. Whichever side wins `take_wake` does
                // the enqueue exactly once.
                if task.take_wake().is_none() {
                    // Owner won the swap; it enqueues + kicks the target.
                    return false;
                }
                // We won: the reclaimed reference is dropped here; fall through
                // and enqueue our own `task` (its context is now saved).
            }
            // TODO: priority
            #[cfg(feature = "smp")]
            task.set_cpu_id(self.cpu_id as _);
            // SAFETY: the caller holds the run-queue context guard.
            unsafe { self.scheduler.lock_raw() }.put_prev_task(task, preempt);
            true
        } else {
            false
        }
    }

    /// Core reschedule subroutine.
    /// Pick the next task to run and switch to it.
    fn resched(&self) {
        let mut scheduler_frame = crate::runtime_preempt::SchedulerFrame::enter();
        // SAFETY: the caller holds the run-queue context guard.
        let next = unsafe { self.scheduler.lock_raw() }
            .pick_next_task()
            .unwrap_or_else(|| {
                // SAFETY: the current run-queue guard prevents migration while
                // resolving this CPU's initialized idle task.
                unsafe {
                    ax_hal::percpu::with_cpu_pin(|pin| {
                        IDLE_TASK.with_current(pin, |idle| {
                            idle.get().expect("idle task must be initialized").clone()
                        })
                    })
                }
                .expect("reschedule requires an installed CPU-local area")
            });
        assert!(
            next.is_ready(),
            "next {} is not ready: {:?}",
            next.id_name(),
            next.state()
        );
        let result = self.switch_to(crate::current(), next, &mut scheduler_frame);
        scheduler_frame.finish(result);
    }

    fn switch_to(
        &self,
        prev_task: CurrentTask,
        next_task: AxTaskRef,
        scheduler_frame: &mut crate::runtime_preempt::SchedulerFrame,
    ) -> crate::runtime_preempt::SchedulerFrameResult {
        use crate::runtime_preempt::SchedulerFrameResult;

        // Make sure that IRQs are disabled by kernel guard or other means.
        #[cfg(not(feature = "host-test"))]
        assert!(
            !ax_hal::asm::irqs_enabled(),
            "IRQs must be disabled during scheduling"
        );
        trace!(
            "context switch: {} -> {}",
            prev_task.id_name(),
            next_task.id_name()
        );
        prev_task.check_stack_canary();
        #[cfg(feature = "preempt")]
        next_task.set_preempt_pending(false);
        next_task.set_state(TaskState::Running);
        if prev_task.ptr_eq(&next_task) {
            return SchedulerFrameResult::Stayed;
        }

        // Claim the task as running, we do this before switching to it
        // such that any running task will have this set.
        #[cfg(feature = "smp")]
        next_task.set_on_cpu(true);

        #[cfg(feature = "task-ext")]
        {
            if let Some(ext) = prev_task.task_ext() {
                ext.on_leave()
            }
            if let Some(ext) = next_task.task_ext() {
                ext.on_enter()
            }
        }

        #[cfg(all(feature = "task-ext", feature = "uspace"))]
        let mut next_activation = next_task
            .task_ext()
            .and_then(|ext| ext.acquire_address_space_activation(this_cpu_id()));
        #[cfg(all(feature = "task-ext", feature = "uspace"))]
        if let Some(activation) = &next_activation {
            assert!(
                activation.installed().is_user(),
                "scheduler requires a userspace identity"
            );
        }

        // `prev_task.state()` must be sampled before the architectural switch:
        // callers like `exit_current` already set it to `Exited`/`Blocked`,
        // and that pre-switch state is what `sched:sched_switch` reports.
        #[cfg(feature = "tracepoint-hooks")]
        ax_crate_interface::call_interface!(
            crate::sched_tracepoint::SchedTracepoint::on_sched_switch(
                prev_task.id(),
                next_task.id(),
                prev_task.state() as u32,
            )
        );

        unsafe {
            let prev_ctx_ptr = prev_task.ctx_mut_ptr();
            let next_ctx_ptr = next_task.ctx_mut_ptr();

            // The enclosing run-queue guard has already disabled migration and
            // local IRQs for the complete switch lifetime.
            let prev_header_pointer = prev_task.context_header().as_non_null();
            let next_header_pointer = next_task.context_header().as_non_null();
            ax_hal::percpu::with_cpu_pin(|pin| {
                // SAFETY: both Arc allocations remain alive across the raw
                // switch; the header fields are permanently pinned within them.
                let prev_header = core::pin::Pin::new_unchecked(prev_header_pointer.as_ref());
                let next_header = core::pin::Pin::new_unchecked(next_header_pointer.as_ref());
                let (prepared, previous_binding) =
                    ax_hal::percpu::prepare_context_switch(pin, prev_header, next_header)
                        .expect("scheduler thread switch must validate before publication");

                // FP, address-space, Arc, and PREV_TASK work all remain before
                // the prepared token's one-way commit.
                #[cfg(all(feature = "task-ext", feature = "uspace"))]
                let next_address_space = next_activation
                    .as_ref()
                    .map(|activation| activation.installed())
                    .or_else(|| {
                        ACTIVE_ADDRESS_SPACE.with_current(pin, |activation| {
                            activation.as_ref().map(|activation| activation.installed())
                        })
                    });
                #[cfg(all(feature = "task-ext", feature = "uspace"))]
                if let Some(installed) = next_address_space {
                    // A userspace task installs its newly acquired token.
                    // A kernel task inherits the existing per-CPU token and
                    // therefore leaves the lazy hardware context active.
                    (*next_ctx_ptr).set_address_space(installed);
                }
                (*prev_ctx_ptr).prepare_switch_to(&*next_ctx_ptr);
                #[cfg(all(feature = "task-ext", feature = "uspace"))]
                if let Some(activation) = next_activation.take() {
                    let proof = crate::AddressSpaceSwitchProof::new(this_cpu_id());
                    let previous = ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                        ACTIVE_ADDRESS_SPACE
                            .with_current_mut(exclusive, |slot| slot.replace(activation))
                    });
                    if let Some(previous) = previous {
                        // `prepare_switch_to` has installed the incoming root.
                        // Only now may the old activation stop naming this CPU
                        // as a possible TLB owner.
                        previous.release_after_root_switch(proof);
                    }
                }
                ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                    PREV_TASK.with_current_mut(exclusive, |slot| {
                        *slot = Some(PreviousTask {
                            task: NonNull::new(Arc::as_ptr(&prev_task) as *mut _).unwrap(),
                            binding: previous_binding,
                        });
                    });
                });

                assert!(Arc::strong_count(&prev_task) > 1);
                assert!(Arc::strong_count(&next_task) >= 1);
                scheduler_frame.transfer();
                CurrentTask::set_current(prev_task, next_task);

                // switch_to_prepared consumes the only commit capability and
                // enters naked assembly immediately after anchor publication.
                (*prev_ctx_ptr).switch_to_prepared(&*next_ctx_ptr, prepared);
            })
            .expect("scheduler switch requires an installed CPU-local area");

            // Execution resumes here when this task is scheduled again. The
            // per-CPU MM activation was either replaced by an incoming user MM
            // or retained across a lazy kernel-task interval; this tail only
            // withdraws the generic previous-task binding and wake hand-off.
            clear_prev_task_on_cpu();
        }
        SchedulerFrameResult::Resumed
    }
}

fn gc_entry() {
    loop {
        // Drop all exited tasks and recycle resources.
        let n = {
            let _guard = crate::sync::PreemptIrqSaveGuard::new();
            // SAFETY: the guard prevents migration and IRQ re-entry, and the
            // closure does not let the per-CPU borrow escape.
            unsafe {
                ax_hal::percpu::with_cpu_pin(|pin| {
                    ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                        EXITED_TASKS.with_current_mut(exclusive, |tasks| tasks.len())
                    })
                })
            }
            .expect("GC requires an installed CPU-local area")
        };
        for _ in 0..n {
            // Do not do the slow drops in the critical section.
            let task = {
                let _guard = crate::sync::PreemptIrqSaveGuard::new();
                // SAFETY: the guard prevents migration and IRQ re-entry.
                unsafe {
                    ax_hal::percpu::with_cpu_pin(|pin| {
                        ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                            EXITED_TASKS.with_current_mut(exclusive, |tasks| tasks.pop_front())
                        })
                    })
                }
                .expect("GC requires an installed CPU-local area")
            };
            if let Some(task) = task {
                if Arc::strong_count(&task) == 1 {
                    // If I'm the last holder of the task, drop it immediately.
                    drop(task);
                } else {
                    // Otherwise (e.g, `switch_to` is not completed, held by the
                    // joiner, etc), push it back and wait for them to drop first.
                    let _guard = crate::sync::PreemptIrqSaveGuard::new();
                    // SAFETY: the guard prevents migration and IRQ re-entry.
                    unsafe {
                        ax_hal::percpu::with_cpu_pin(|pin| {
                            ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                                EXITED_TASKS
                                    .with_current_mut(exclusive, |tasks| tasks.push_back(task))
                            })
                        })
                    }
                    .expect("GC requires an installed CPU-local area");
                }
            }
        }
        #[cfg(feature = "vmap-task-stack")]
        {
            // TaskStack::drop only publishes `Live -> Retiring`. Run the
            // fallible PTE/TLB/frame retire protocol here in the sleepable GC
            // task, after all EXITED_TASKS locks and CPU-local borrows ended.
            let report = ax_mm::retry_kernel_virtual_quarantines(16);
            if report.failed() != 0 {
                warn!(
                    "task-stack retire pass kept {}/{} range(s) queued: {:?}",
                    report.failed(),
                    report.attempted(),
                    report.first_error(),
                );
            }
        }
        // Always wait with a timeout to:
        // 1. Yield CPU to allow other tasks to complete `switch_to` and drop references
        // 2. Handle the race condition where `notify_one` is called before the GC task enters wait,
        //    causing the notification to be lost.
        // The GC task's affinity pins it to this CPU across the blocking wait;
        // WaitQueue is internally synchronized, so IRQ and other tasks may use
        // shared access while this callback is suspended.
        unsafe {
            ax_hal::percpu::with_cpu_pin(|pin| {
                WAIT_FOR_EXIT.with_current(pin, |wait| {
                    let _timeout = wait.wait_timeout(core::time::Duration::from_millis(100));
                })
            })
        }
        .expect("GC wait requires an installed CPU-local area");
    }
}

/// The task routine for migrating the current task to the correct CPU.
///
/// It calls `select_run_queue` to get the correct run queue for the task, and
/// then puts the task to the scheduler of target run queue.
#[cfg(feature = "smp")]
pub(crate) fn migrate_entry(migrated_task: AxTaskRef) {
    let rq = select_run_queue::<crate::sync::PreemptIrqSaveState>(&migrated_task);
    let cpu_id = rq.inner.cpu_id;
    migrated_task.set_cpu_id(cpu_id as _);
    // SAFETY: `rq` owns the target run-queue critical section.
    unsafe { rq.inner.scheduler.lock_raw() }.put_prev_task(migrated_task, false);
    #[cfg(all(feature = "smp", feature = "ipi"))]
    // Current-task migration cannot make progress until the target CPU runs
    // the migrated task, so do not let a stale coalescing bit suppress this IPI.
    force_kick_remote_cpu(cpu_id);
}

/// Clear the `on_cpu` field of the previous task running on this CPU, then
/// complete any cross-core wake that was deferred while it was still `on_cpu`.
pub(crate) unsafe fn clear_prev_task_on_cpu() {
    let previous = unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| {
            ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                PREV_TASK.with_current_mut(exclusive, Option::take)
            })
        })
    }
    .expect("incoming switch tail requires an installed CPU-local area")
    .expect("PREV_TASK should have been set by switch_to");
    // Safety: prev_task's Arc is still alive on the caller's stack at this point
    // (switch_to has not yet returned), so the pointer is valid.
    let prev = unsafe { previous.task.as_ref() };
    // SAFETY: current publication and architecture registers already identify
    // the incoming task, and this is the sole owner of the recorded epoch.
    unsafe { previous.binding.finish(prev.context_header()) }
        .expect("incoming switch tail must withdraw prev_task CPU binding");
    // Publish that the context is fully saved. The SeqCst store pairs with the
    // waker's `on_cpu()`/`take_wake()` handshake in `put_task_with_state`.
    #[cfg(feature = "smp")]
    prev.set_on_cpu(false);
    // Drain a wake that raced our switch-out. `take_wake` is the single arbiter:
    // if the waker did not reclaim it (it saw `on_cpu` still true), we get the
    // owned reference and enqueue it now that the context is saved.
    #[cfg(feature = "smp")]
    if let Some(task) = prev.take_wake() {
        let target = task.cpu_id() as usize;
        // Leaf lock: `resched()` already dropped this CPU's scheduler lock before
        // `switch_to`, so this takes only the target run queue's lock.
        let target_run_queue = get_run_queue(target);
        // The task may have completed its exit after a remote wake changed
        // `Blocked -> Ready`, but before this CPU finished switching it out.
        // Reuse the run-queue state transition as the enqueue ownership gate:
        // `Ready -> Ready` deliberately validates that the deferred wake still
        // owns a runnable task. Once `on_cpu` is false, only a successful gate
        // may publish the task to a scheduler; an `Exited` task is dropped here.
        let enqueued = target_run_queue.put_task_with_state(task, TaskState::Ready, false);
        if enqueued && target != this_cpu_id() {
            // Remote target: ask that CPU to reschedule so it picks the task up
            // (and wakes if it is idle in `wait_for_irqs`).
            #[cfg(feature = "ipi")]
            kick_remote_cpu(target);
        } else if enqueued {
            // Local target: `kick_remote_cpu(self)` is a no-op, so the reschedule
            // the remote waker's IPI used to deliver here would be lost — the
            // task could sit un-run until the next tick, or indefinitely if this
            // CPU just switched to `idle` and is about to `wait_for_irqs()`.
            // `target == this_cpu_id()` arises when `select_wake_run_queue()`
            // falls back to the task's `last_cpu`, which is this owning CPU.
            // Request a reschedule on THIS CPU instead: the current task
            // (`next_task`, possibly `idle`) is forced to reschedule when the
            // switch chain unwinds and its preempt guard is released
            // (`current_check_preempt_pending` consumes the flag), mirroring the
            // reschedule the runtime IPI handler performed.
            #[cfg(feature = "preempt")]
            crate::current().set_force_resched_pending(true);
        }
    }
}
pub(crate) fn init() {
    let cpu_id = this_cpu_id();

    #[cfg(feature = "uspace")]
    crate::task::initialize_kernel_task_address_space();

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
    // SAFETY: scheduler bootstrap runs before this CPU can schedule or accept
    // interrupts, and each callback keeps its mutable borrow local.
    unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| {
            ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                IDLE_TASK.with_current_mut(exclusive, |idle| {
                    idle.init_once(idle_task.into_arc());
                })
            })
        })
    }
    .expect("scheduler bootstrap requires an installed CPU-local area");

    // Put the subsequent execution into the `main` task.
    let main_task = TaskInner::new_init("main".into(), main_task_stack()).into_arc();
    main_task.set_state(TaskState::Running);
    unsafe { CurrentTask::init_current(main_task) }

    let run_queue = unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| {
            ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                RUN_QUEUE.with_current_mut(exclusive, |run_queue| {
                    run_queue.init_once(AxRunQueue::new(cpu_id));
                    NonNull::from(
                        run_queue
                            .get_mut()
                            .expect("run queue must be initialized during bootstrap"),
                    )
                })
            })
        })
    }
    .expect("scheduler bootstrap requires an installed CPU-local area");
    unsafe {
        RUN_QUEUES[cpu_id].write(run_queue);
    }
}

pub(crate) fn init_secondary(stack_ptr: VirtAddr, stack_size: usize) {
    let cpu_id = this_cpu_id();

    #[cfg(feature = "uspace")]
    crate::task::initialize_kernel_task_address_space();

    // Put the subsequent execution into the `idle` task.
    let idle_task = TaskInner::new_init(
        "idle".into(),
        TaskStack::borrowed(stack_ptr, stack_size, TASK_STACK_ALIGN),
    )
    .into_arc();
    idle_task.set_state(TaskState::Running);
    // SAFETY: the secondary CPU remains offline and IRQ-disabled throughout
    // its scheduler initialization.
    unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| {
            ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                IDLE_TASK.with_current_mut(exclusive, |idle| {
                    idle.init_once(idle_task.clone());
                })
            })
        })
    }
    .expect("secondary scheduler bootstrap requires an installed CPU-local area");
    unsafe { CurrentTask::init_current(idle_task) }

    let run_queue = unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| {
            ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                RUN_QUEUE.with_current_mut(exclusive, |run_queue| {
                    run_queue.init_once(AxRunQueue::new(cpu_id));
                    NonNull::from(
                        run_queue
                            .get_mut()
                            .expect("secondary run queue must be initialized"),
                    )
                })
            })
        })
    }
    .expect("secondary scheduler bootstrap requires an installed CPU-local area");
    unsafe {
        RUN_QUEUES[cpu_id].write(run_queue);
    }
}
