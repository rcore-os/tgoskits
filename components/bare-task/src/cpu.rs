//! Per-CPU task runtime core.

use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

use crate::{
    BaseScheduler, CFSTask, FifoTask, IrqWakeQueueCore, RRTask, RunQueueCore, ScheduledTask,
    TaskCore, TaskOps, TaskRef, TaskState, WakeResult,
    wake::{HardIrqWaker, WakeBits},
};

/// Runtime IPI event kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpiEvent {
    /// Re-evaluate scheduling on the target CPU.
    Reschedule,
    /// Drain the target CPU hard-IRQ wake queue.
    IrqWakeDrain,
    /// Run timer service work on the target CPU.
    TimerService,
}

impl IpiEvent {
    const fn bit(self) -> usize {
        match self {
            Self::Reschedule => 1 << 0,
            Self::IrqWakeDrain => 1 << 1,
            Self::TimerService => 1 << 2,
        }
    }
}

/// Coalesced runtime IPI event set.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IpiEvents(usize);

impl IpiEvents {
    /// Creates an empty event set.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Creates an event set from explicit events.
    pub fn from_events(events: &[IpiEvent]) -> Self {
        let mut bits = 0;
        for event in events {
            bits |= event.bit();
        }
        Self(bits)
    }

    /// Returns whether the set is empty.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns whether `event` is present.
    pub const fn contains(self, event: IpiEvent) -> bool {
        self.0 & event.bit() != 0
    }

    /// Returns raw event bits.
    pub const fn bits(self) -> usize {
        self.0
    }

    /// Returns the number of pending events.
    pub const fn count(self) -> usize {
        self.0.count_ones() as usize
    }
}

/// Scheduler item that can be created from a bare [`TaskRef`].
pub trait ReadyTaskFromCore: ScheduledTask {
    /// Creates a scheduler item for `task`.
    fn from_task_ref(task: TaskRef) -> Self;
}

impl ReadyTaskFromCore for Arc<FifoTask<TaskRef>> {
    fn from_task_ref(task: TaskRef) -> Self {
        Arc::new(FifoTask::new(task))
    }
}

impl<const S: usize> ReadyTaskFromCore for Arc<RRTask<TaskRef, S>> {
    fn from_task_ref(task: TaskRef) -> Self {
        Arc::new(RRTask::new(task))
    }
}

impl ReadyTaskFromCore for Arc<CFSTask<TaskRef>> {
    fn from_task_ref(task: TaskRef) -> Self {
        Arc::new(CFSTask::new(task))
    }
}

struct TimerEntry {
    deadline_nanos: u64,
    task: TaskRef,
    active: bool,
}

/// Per-CPU task runtime core.
pub struct BareCpuCore<S: BaseScheduler>
where
    S::SchedItem: ScheduledTask,
{
    cpu_id: crate::CpuId,
    run_queue: crate::sync::SpinMutex<RunQueueCore<S>>,
    irq_wake_queue: IrqWakeQueueCore,
    ipi_pending: AtomicUsize,
    timer_service_pending: AtomicBool,
    ready_count: AtomicUsize,
    current_task: AtomicPtr<()>,
    idle_task: AtomicPtr<()>,
    init_task: AtomicPtr<()>,
    prev_task: AtomicPtr<()>,
    timers: crate::sync::SpinMutex<Vec<TimerEntry>>,
}

impl<S> BareCpuCore<S>
where
    S: BaseScheduler,
    S::SchedItem: ScheduledTask,
{
    /// Creates a CPU core with an empty run queue.
    pub fn new(cpu_id: crate::CpuId, scheduler: S) -> Self {
        Self {
            cpu_id,
            run_queue: crate::sync::SpinMutex::new(RunQueueCore::new(scheduler)),
            irq_wake_queue: IrqWakeQueueCore::new(),
            ipi_pending: AtomicUsize::new(0),
            timer_service_pending: AtomicBool::new(false),
            ready_count: AtomicUsize::new(0),
            current_task: AtomicPtr::new(core::ptr::null_mut()),
            idle_task: AtomicPtr::new(core::ptr::null_mut()),
            init_task: AtomicPtr::new(core::ptr::null_mut()),
            prev_task: AtomicPtr::new(core::ptr::null_mut()),
            timers: crate::sync::SpinMutex::new(Vec::new()),
        }
    }

    /// Returns this CPU id.
    pub const fn cpu_id(&self) -> crate::CpuId {
        self.cpu_id
    }

    /// Runs `f` with mutable access to this CPU run queue core.
    pub fn with_run_queue<R>(&self, f: impl FnOnce(&mut RunQueueCore<S>) -> R) -> R {
        f(&mut self.run_queue.lock())
    }

    /// Returns the number of tasks inserted through this runtime core.
    pub fn run_queue_len(&self) -> usize {
        self.ready_count.load(Ordering::Acquire)
    }

    /// Requests an IPI event, returning true if it newly set a pending bit.
    pub fn request_ipi(&self, event: IpiEvent) -> bool {
        let bit = event.bit();
        self.ipi_pending.fetch_or(bit, Ordering::AcqRel) & bit == 0
    }

    /// Takes coalesced pending IPI events.
    pub fn take_pending_ipis(&self) -> IpiEvents {
        IpiEvents(self.ipi_pending.swap(0, Ordering::AcqRel))
    }

    /// Clears one pending IPI event.
    pub fn clear_ipi(&self, event: IpiEvent) {
        self.ipi_pending.fetch_and(!event.bit(), Ordering::AcqRel);
    }

    /// Returns whether timer service is pending.
    pub fn timer_service_pending(&self) -> bool {
        self.timer_service_pending.load(Ordering::Acquire)
    }

    /// Marks timer service pending.
    pub fn mark_timer_service_pending(&self) -> bool {
        !self.timer_service_pending.swap(true, Ordering::AcqRel)
    }

    /// Clears timer service pending.
    pub fn clear_timer_service_pending(&self) {
        self.timer_service_pending.store(false, Ordering::Release);
    }

    /// Installs the current task raw pointer.
    pub fn set_current_task_ptr(&self, ptr: *mut ()) {
        self.current_task.store(ptr, Ordering::Release);
    }

    /// Returns the current task raw pointer.
    pub fn current_task_ptr(&self) -> *mut () {
        self.current_task.load(Ordering::Acquire)
    }

    /// Installs the idle task raw pointer.
    pub fn set_idle_task_ptr(&self, ptr: *mut ()) {
        self.idle_task.store(ptr, Ordering::Release);
    }

    /// Installs the init task raw pointer.
    pub fn set_init_task_ptr(&self, ptr: *mut ()) {
        self.init_task.store(ptr, Ordering::Release);
    }

    /// Stores the previous task raw pointer.
    pub fn set_prev_task_ptr(&self, ptr: *mut ()) {
        self.prev_task.store(ptr, Ordering::Release);
    }

    /// Takes the previous task raw pointer.
    pub fn take_prev_task_ptr(&self) -> *mut () {
        self.prev_task.swap(core::ptr::null_mut(), Ordering::AcqRel)
    }

    fn push_irq_wake_task(&self, task: &TaskRef) {
        let task_ptr = Arc::as_ptr(task).cast_mut();
        // Keep the task alive until the drain path reconstructs the Arc.
        unsafe { Arc::increment_strong_count(task_ptr) };
        self.irq_wake_queue.push(task_ptr.cast::<()>(), |next| {
            task.set_wake_next(next.cast::<TaskCore>());
        });
    }

    fn pop_irq_wake_task(&self) -> Option<TaskRef> {
        let head = self.irq_wake_queue.pop(|node| {
            let task = unsafe { &*node.cast::<TaskCore>() };
            task.wake_next::<TaskCore>().cast::<()>()
        })?;
        Some(unsafe { Arc::from_raw(head.cast::<TaskCore>()) })
    }

    fn add_timer(&self, deadline_nanos: u64, task: TaskRef) {
        self.timers.lock().push(TimerEntry {
            deadline_nanos,
            task,
            active: true,
        });
    }

    fn take_expired_timer_tasks(&self, now_nanos: u64) -> Vec<TaskRef> {
        let mut expired = Vec::new();
        for timer in self.timers.lock().iter_mut() {
            if timer.active && timer.deadline_nanos <= now_nanos {
                timer.active = false;
                expired.push(timer.task.clone());
            }
        }
        expired
    }
}

impl<S> BareCpuCore<S>
where
    S: BaseScheduler,
    S::SchedItem: ReadyTaskFromCore,
{
    fn enqueue_task_ref(&self, task: TaskRef) {
        self.run_queue
            .lock()
            .add_task(S::SchedItem::from_task_ref(task));
        self.ready_count.fetch_add(1, Ordering::AcqRel);
    }
}

/// Multi-CPU task runtime core.
pub struct BareTaskRuntime<S: BaseScheduler>
where
    S::SchedItem: ScheduledTask,
{
    cpus: crate::sync::SpinMutex<Vec<Arc<BareCpuCore<S>>>>,
}

impl<S> BareTaskRuntime<S>
where
    S: BaseScheduler,
    S::SchedItem: ScheduledTask,
{
    /// Creates an empty runtime.
    pub const fn new() -> Self {
        Self {
            cpus: crate::sync::SpinMutex::new(Vec::new()),
        }
    }

    /// Adds a CPU runtime core.
    pub fn add_cpu(&self, cpu_id: crate::CpuId, scheduler: S) -> Arc<BareCpuCore<S>> {
        let cpu = Arc::new(BareCpuCore::new(cpu_id, scheduler));
        let mut cpus = self.cpus.lock();
        if cpus.len() == cpu_id.0 {
            cpus.push(cpu.clone());
        } else if cpus.len() > cpu_id.0 {
            cpus[cpu_id.0] = cpu.clone();
        } else {
            panic!("CPU runtime cores must be installed in cpu-id order");
        }
        cpu
    }

    /// Returns a CPU runtime core.
    pub fn cpu(&self, cpu_id: crate::CpuId) -> Option<Arc<BareCpuCore<S>>> {
        self.cpus.lock().get(cpu_id.0).cloned()
    }

    fn cpu_expect(&self, cpu_id: crate::CpuId) -> Arc<BareCpuCore<S>> {
        self.cpu(cpu_id)
            .expect("bare task CPU core is not installed")
    }
}

impl<S> Default for BareTaskRuntime<S>
where
    S: BaseScheduler,
    S::SchedItem: ScheduledTask,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<S> BareTaskRuntime<S>
where
    S: BaseScheduler,
    S::SchedItem: ReadyTaskFromCore,
{
    /// Publishes a hard IRQ wake and queues the task on its target CPU.
    pub fn wake_from_irq(&self, waker: &HardIrqWaker, bits: WakeBits) -> WakeResult {
        let (task, result) = waker.wake_from_irq(bits);
        let Some(task) = task else {
            return result;
        };
        if result.woke() {
            let cpu = self.cpu_expect(task.cpu_id());
            cpu.push_irq_wake_task(&task);
            cpu.request_ipi(IpiEvent::IrqWakeDrain);
            return WakeResult::new(true, false, true);
        }
        result
    }

    /// Drains one CPU's hard IRQ wake queue into the run queue.
    pub fn drain_irq_wake_queue(&self, cpu_id: crate::CpuId) -> usize {
        let cpu = self.cpu_expect(cpu_id);
        let mut drained = 0;
        while let Some(task) = cpu.pop_irq_wake_task() {
            task.clear_wake_link();
            if !task.take_wake_pending() {
                continue;
            }
            if task.transition_state(TaskState::Blocked, TaskState::Ready) {
                cpu.enqueue_task_ref(task);
                drained += 1;
            }
        }
        drained
    }

    /// Enqueues an already-ready task on the target CPU.
    pub fn add_ready_task(&self, cpu_id: crate::CpuId, task: TaskRef) {
        self.cpu_expect(cpu_id).enqueue_task_ref(task);
    }

    /// Adds a task timer to a CPU runtime.
    pub fn add_task_timer(&self, cpu_id: crate::CpuId, deadline_nanos: u64, task: TaskRef) {
        self.cpu_expect(cpu_id).add_timer(deadline_nanos, task);
    }

    /// Handles a timer interrupt. This only marks deferred timer service work.
    pub fn on_timer_irq(&self, cpu_id: crate::CpuId, _now_nanos: u64) {
        let cpu = self.cpu_expect(cpu_id);
        cpu.mark_timer_service_pending();
    }

    /// Handles a runtime IPI interrupt.
    pub fn on_ipi_irq(&self, cpu_id: crate::CpuId) -> IpiEvents {
        let events = self.cpu_expect(cpu_id).take_pending_ipis();
        if events.contains(IpiEvent::IrqWakeDrain) {
            self.drain_irq_wake_queue(cpu_id);
        }
        events
    }

    /// Drains expired timer tasks in task/deferred context.
    pub fn drain_timer_service(&self, cpu_id: crate::CpuId, now_nanos: u64) -> usize {
        let cpu = self.cpu_expect(cpu_id);
        if !cpu.timer_service_pending() {
            return 0;
        }
        cpu.clear_timer_service_pending();
        let expired = cpu.take_expired_timer_tasks(now_nanos);
        let mut drained = 0;
        for task in expired {
            if task.transition_state(TaskState::Blocked, TaskState::Ready) {
                cpu.enqueue_task_ref(task);
                drained += 1;
            }
        }
        drained
    }
}

impl TaskOps for TaskRef {
    fn core(&self) -> &TaskCore {
        self
    }

    fn id_name(&self) -> alloc::string::String {
        alloc::format!("task#{}", self.id().as_u64())
    }

    fn is_idle(&self) -> bool {
        false
    }

    fn is_init(&self) -> bool {
        false
    }
}
