use alloc::{boxed::Box, string::String, sync::Arc};
#[cfg(not(feature = "stack-guard-page"))]
use core::alloc::Layout;
#[cfg(feature = "smp")]
use core::sync::atomic::AtomicPtr;
#[cfg(any(
    feature = "preempt",
    all(feature = "stack-guard-page", feature = "smp", feature = "ipi")
))]
use core::sync::atomic::AtomicUsize;
use core::{
    cell::{Cell, UnsafeCell},
    fmt,
    mem::ManuallyDrop,
    ops::Deref,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicU64, Ordering},
    task::{Context, Poll},
};

#[cfg(feature = "tls")]
use ax_hal::tls::TlsArea;
use ax_hal::{
    context::{KernelTlsBase, TaskContext},
    percpu::{CurrentContext, CurrentThreadHeader},
};
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
#[cfg(feature = "stack-guard-page")]
use ax_memory_addr::PAGE_SIZE_4K;
use ax_memory_addr::{VirtAddr, align_up_4k};
use futures_util::task::AtomicWaker;

#[cfg(feature = "lockdep")]
use crate::lockdep::HeldLockStack;
use crate::{AxCpuMask, AxTask, AxTaskRef, WaitQueue};

#[cfg(target_pointer_width = "64")]
const STACK_END_MAGIC: usize = 0x57AC_CE11_57AC_CE11usize;
#[cfg(target_pointer_width = "32")]
const STACK_END_MAGIC: usize = 0x57AC_CE11usize;

/// Required alignment for task kernel stacks. x86_64 task context setup relies
/// on the ABI-mandated 16-byte stack alignment at task entry.
pub(crate) const TASK_STACK_ALIGN: usize = 16;

/// A unique identifier for a thread.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TaskId(u64);

/// The possible states of a task.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TaskState {
    /// Task is running on some CPU.
    Running = 1,
    /// Task is ready to run on some scheduler's ready queue.
    Ready   = 2,
    /// Task is blocked (in the wait queue or timer list),
    /// and it has finished its scheduling process, it can be wake up by `notify()` on any run queue safely.
    Blocked = 3,
    /// Task is exited and waiting for being dropped.
    Exited  = 4,
}

/// User-defined task extended data.
#[cfg(feature = "task-ext")]
#[extern_trait::extern_trait(
    /// The impl proxy type for [`TaskExt`].
    pub AxTaskExt
)]
pub trait TaskExt {
    /// Called when the task is switched in.
    fn on_enter(&self) {}
    /// Called when the task is switched out.
    fn on_leave(&self) {}
}

/// The inner task structure.
pub struct TaskInner {
    id: TaskId,
    name: SpinNoIrq<String>,
    is_idle: bool,
    is_init: bool,

    entry: Cell<Option<Box<dyn FnOnce()>>>,
    state: AtomicU8,

    /// CPU affinity mask.
    cpumask: SpinNoIrq<AxCpuMask>,

    /// Scheduling policy of the task.
    sched_policy: AtomicI32,

    /// Scheduling priority of the task.
    sched_priority: AtomicI32,

    /// Mark whether the task is in the wait queue.
    in_wait_queue: AtomicBool,

    /// Used to indicate the CPU ID where the task is running or will run.
    cpu_id: AtomicU32,
    /// Used to indicate whether the task is running on a CPU.
    #[cfg(feature = "smp")]
    on_cpu: AtomicBool,
    /// One-shot cross-core wake handoff.
    ///
    /// When a remote CPU wins the `Blocked -> Ready` transition for this task
    /// while it is still `on_cpu` (its context not yet fully saved on its owning
    /// CPU), the waker must NOT enqueue it — and must not spin on `on_cpu`
    /// either (that is the cross-core mutual-wake deadlock). Instead it records
    /// the target run-queue in `cpu_id` and stashes an owned reference here; the
    /// owning CPU drains it in `clear_prev_task_on_cpu()` once `on_cpu` is false,
    /// then enqueues + kicks the target. Holds a `*const AxTask` produced by
    /// `Arc::into_raw` (null = empty). See `run_queue::put_task_with_state`.
    #[cfg(feature = "smp")]
    wake_handoff: AtomicPtr<AxTask>,

    /// A ticket ID used to identify the timer event.
    /// Set by `set_timer_ticket()` when creating a timer event in `set_alarm_wakeup()`,
    /// expired by setting it as zero in `timer_ticket_expired()`, which is called by `cancel_events()`.
    #[cfg(feature = "irq")]
    timer_ticket_id: AtomicU64,

    #[cfg(feature = "preempt")]
    need_resched: AtomicBool,
    #[cfg(feature = "preempt")]
    force_resched: AtomicBool,
    #[cfg(feature = "preempt")]
    preempt_disable_count: AtomicUsize,

    interrupted: AtomicBool,
    interrupt_waker: AtomicWaker,

    exit_code: AtomicI32,
    wait_for_exit: WaitQueue,

    kstack: TaskStack,
    ctx: UnsafeCell<TaskContext>,
    /// Pinned identity and CPU-binding state published by the switch tail.
    current_header: LazyInit<CurrentThreadHeader>,
    #[cfg(feature = "lockdep")]
    held_locks: UnsafeCell<HeldLockStack>,

    #[cfg(feature = "task-ext")]
    task_ext: Option<AxTaskExt>,

    #[cfg(feature = "tls")]
    tls: TlsArea,
}

impl TaskId {
    fn new() -> Self {
        static ID_COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Convert the task ID to a `u64`.
    pub const fn as_u64(&self) -> u64 {
        self.0
    }
}

impl From<u8> for TaskState {
    #[inline]
    fn from(state: u8) -> Self {
        match state {
            1 => Self::Running,
            2 => Self::Ready,
            3 => Self::Blocked,
            4 => Self::Exited,
            _ => unreachable!(),
        }
    }
}

unsafe impl Send for TaskInner {}
unsafe impl Sync for TaskInner {}

impl TaskInner {
    /// Create a new task with the given entry function and stack size.
    pub fn new<F>(entry: F, name: String, stack_size: usize) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        let kstack = TaskStack::alloc(align_up_4k(stack_size));
        let mut t = Self::new_common(TaskId::new(), name, kstack);
        debug!("new task: {}", t.id_name());

        #[cfg(feature = "tls")]
        let kernel_tls = KernelTlsBase::new(t.tls.tls_ptr() as usize);
        #[cfg(not(feature = "tls"))]
        let kernel_tls = KernelTlsBase::new(0);
        let kstack_top = t.kstack.top();

        t.entry = Cell::new(Some(Box::new(entry)));
        t.ctx_mut()
            .init(task_entry as *const () as usize, kstack_top, kernel_tls);
        if t.name() == "idle" {
            t.is_idle = true;
        }
        t
    }

    /// Gets the ID of the task.
    pub const fn id(&self) -> TaskId {
        self.id
    }

    /// Gets the name of the task.
    pub fn name(&self) -> String {
        self.name.lock().clone()
    }

    /// Set the name of the task.
    pub fn set_name(&self, name: &str) {
        *self.name.lock() = String::from(name);
    }

    /// Get a combined string of the task ID and name.
    pub fn id_name(&self) -> alloc::string::String {
        alloc::format!("Task({}, {:?})", self.id.as_u64(), self.name())
    }

    /// Wait for the task to exit, and return the exit code.
    ///
    /// It will return immediately if the task has already exited (but not dropped).
    #[track_caller]
    pub fn join(&self) -> i32 {
        crate::api::might_sleep();
        self.wait_for_exit
            .wait_until(|| self.state() == TaskState::Exited);
        self.exit_code.load(Ordering::Acquire)
    }

    /// Returns a reference to the task extended data.
    #[cfg(feature = "task-ext")]
    pub fn task_ext(&self) -> Option<&AxTaskExt> {
        self.task_ext.as_ref()
    }

    /// Returns a mutable reference to the task extended data.
    #[cfg(feature = "task-ext")]
    pub fn task_ext_mut(&mut self) -> &mut Option<AxTaskExt> {
        &mut self.task_ext
    }

    /// Returns a mutable reference to the task context.
    #[inline]
    pub const fn ctx_mut(&mut self) -> &mut TaskContext {
        self.ctx.get_mut()
    }

    /// Updates the page table root stored in this task's context and switches
    /// the hardware page table immediately. Only safe to call on the current
    /// running task.
    #[cfg(feature = "uspace")]
    pub fn switch_page_table(&self, root: ax_memory_addr::PhysAddr) {
        // SAFETY: we are the current task and no other thread touches our ctx.
        unsafe { (*self.ctx.get()).set_page_table_root(root) };
        unsafe { ax_hal::asm::write_user_page_table(root) };
        ax_hal::asm::flush_tlb(None);
    }

    #[cfg(feature = "lockdep")]
    pub(crate) fn with_held_locks<R>(&self, f: impl FnOnce(&mut HeldLockStack) -> R) -> R {
        // SAFETY: the held-lock stack belongs to the current task and is only
        // mutated by the current task while lockdep tracking is active.
        f(unsafe { &mut *self.held_locks.get() })
    }

    /// Returns the CPU ID where the task is running or will run.
    ///
    /// Note: the task may not be running on the CPU, it just exists in the run queue.
    #[inline]
    pub fn cpu_id(&self) -> u32 {
        self.cpu_id.load(Ordering::Acquire)
    }

    /// Gets the cpu affinity mask of the task.
    ///
    /// Returns the cpu affinity mask of the task in type [`AxCpuMask`].
    #[inline]
    pub fn cpumask(&self) -> AxCpuMask {
        *self.cpumask.lock()
    }

    /// Sets the cpu affinity mask of the task.
    ///
    /// # Arguments
    /// `cpumask` - The cpu affinity mask to be set in type [`AxCpuMask`].
    #[inline]
    pub fn set_cpumask(&self, cpumask: AxCpuMask) {
        *self.cpumask.lock() = cpumask
    }

    #[inline]
    pub fn sched_policy(&self) -> i32 {
        self.sched_policy.load(Ordering::Acquire)
    }

    #[inline]
    pub fn set_sched_policy(&self, policy: i32) {
        self.sched_policy.store(policy, Ordering::Release)
    }

    #[inline]
    pub fn sched_priority(&self) -> i32 {
        self.sched_priority.load(Ordering::Acquire)
    }

    #[inline]
    pub fn set_sched_priority(&self, prio: i32) {
        self.sched_priority.store(prio, Ordering::Release)
    }

    /// Polls whether the task has been interrupted.
    #[inline]
    pub fn poll_interrupt(&self, cx: &Context) -> Poll<()> {
        // Register the waker BEFORE rechecking the flag. Under preemptive
        // scheduling a timer IRQ between an initial swap and register could
        // allow `interrupt()` to run and call `wake()` on an empty waker
        // slot — the wake is lost. Registering first closes the window.
        self.interrupt_waker.register(cx.waker());
        if self.interrupted.swap(false, Ordering::AcqRel) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    /// Clears the interrupt state of the task.
    #[inline]
    pub fn clear_interrupt(&self) {
        self.interrupted.store(false, Ordering::Release);
    }

    /// Atomically checks and clears the interrupt flag.
    ///
    /// Returns `true` if the task was interrupted.
    #[inline]
    pub fn take_interrupt(&self) -> bool {
        self.interrupted.swap(false, Ordering::AcqRel)
    }

    /// Checks whether the task has been interrupted without clearing
    /// the flag.
    ///
    /// This is a non-consuming read, unlike [`take_interrupt`]. Use this
    /// when the interrupt flag needs to remain set for subsequent
    /// consumers (e.g., an [`interruptible`] future wrapper).
    #[inline]
    pub fn interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Acquire)
    }

    /// Interrupts the task.
    #[inline]
    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Release);
        self.interrupt_waker.wake();
    }
}

// private methods
impl TaskInner {
    fn new_common(id: TaskId, name: String, kstack: TaskStack) -> Self {
        Self {
            id,
            name: SpinNoIrq::new(name),
            is_idle: false,
            is_init: false,
            entry: Cell::new(None),
            state: AtomicU8::new(TaskState::Ready as u8),
            // By default, the task is allowed to run on all CPUs.
            cpumask: SpinNoIrq::new(crate::api::cpu_mask_full()),
            sched_policy: AtomicI32::new(0),
            sched_priority: AtomicI32::new(0),
            in_wait_queue: AtomicBool::new(false),
            #[cfg(feature = "irq")]
            timer_ticket_id: AtomicU64::new(0),
            cpu_id: AtomicU32::new(0),
            #[cfg(feature = "smp")]
            on_cpu: AtomicBool::new(false),
            #[cfg(feature = "smp")]
            wake_handoff: AtomicPtr::new(core::ptr::null_mut()),
            #[cfg(feature = "preempt")]
            need_resched: AtomicBool::new(false),
            #[cfg(feature = "preempt")]
            force_resched: AtomicBool::new(false),
            #[cfg(feature = "preempt")]
            preempt_disable_count: AtomicUsize::new(0),
            interrupted: AtomicBool::new(false),
            interrupt_waker: AtomicWaker::new(),
            exit_code: AtomicI32::new(0),
            wait_for_exit: WaitQueue::new(),
            kstack,
            ctx: UnsafeCell::new(TaskContext::new()),
            current_header: LazyInit::new(),
            #[cfg(feature = "lockdep")]
            held_locks: UnsafeCell::new(HeldLockStack::new()),
            #[cfg(feature = "task-ext")]
            task_ext: None,
            #[cfg(feature = "tls")]
            tls: TlsArea::alloc(),
        }
    }

    /// Creates an "init task" using the current CPU states, to use as the
    /// current task.
    ///
    /// As it is the current task, no other task can switch to it until it
    /// switches out.
    ///
    /// And there is no need to set the `entry`, `kstack` or `tls` fields, as
    /// they will be filled automatically when the task is switches out.
    pub(crate) fn new_init(name: String, kstack: TaskStack) -> Self {
        let mut t = Self::new_common(TaskId::new(), name, kstack);
        t.is_init = true;
        #[cfg(feature = "smp")]
        t.set_on_cpu(true);
        if t.name() == "idle" {
            t.is_idle = true;
        }
        t
    }

    pub(crate) fn into_arc(self) -> AxTaskRef {
        let task = Arc::new(AxTask::new(self));
        let current_context = CurrentContext::from_raw(Arc::as_ptr(&task) as usize)
            .expect("Arc task pointer must be non-null");
        let header = task
            .current_header
            .init_once(CurrentThreadHeader::new(current_context));
        // SAFETY: `header` is stored inside the Arc allocation that owns the
        // task. That allocation is stable until the last task reference drops.
        let header = unsafe { Pin::new_unchecked(header) };
        // SAFETY: the Arc is not visible to any scheduler yet, so this is the
        // only access to its architecture context.
        unsafe { (*task.ctx_mut_ptr()).set_current_header(header.as_non_null()) };
        task
    }

    pub(crate) fn current_header(&self) -> Pin<&CurrentThreadHeader> {
        let header = self
            .current_header
            .get()
            .expect("task header must be initialized after Arc allocation");
        // SAFETY: `into_arc` initializes this field only after the containing
        // scheduler task reaches its permanent Arc allocation.
        unsafe { Pin::new_unchecked(header) }
    }

    /// Returns the current state of the task.
    #[inline]
    pub fn state(&self) -> TaskState {
        self.state.load(Ordering::Acquire).into()
    }

    #[inline]
    pub(crate) fn set_state(&self, state: TaskState) {
        self.state.store(state as u8, Ordering::Release)
    }

    /// Transition the task state from `current_state` to `new_state`,
    /// Returns `true` if the current state is `current_state` and the state is successfully set to `new_state`,
    /// otherwise returns `false`.
    #[inline]
    pub(crate) fn transition_state(&self, current_state: TaskState, new_state: TaskState) -> bool {
        self.state
            .compare_exchange(
                current_state as u8,
                new_state as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    #[inline]
    pub(crate) fn is_running(&self) -> bool {
        matches!(self.state(), TaskState::Running)
    }

    #[inline]
    pub(crate) fn is_ready(&self) -> bool {
        matches!(self.state(), TaskState::Ready)
    }

    #[inline]
    pub(crate) const fn is_init(&self) -> bool {
        self.is_init
    }

    #[inline]
    pub(crate) const fn is_idle(&self) -> bool {
        self.is_idle
    }

    #[inline]
    pub(crate) fn in_wait_queue(&self) -> bool {
        self.in_wait_queue.load(Ordering::Acquire)
    }

    #[inline]
    pub(crate) fn set_in_wait_queue(&self, in_wait_queue: bool) {
        self.in_wait_queue.store(in_wait_queue, Ordering::Release);
    }

    /// Returns task's current timer ticket ID.
    #[inline]
    #[cfg(feature = "irq")]
    pub(crate) fn timer_ticket(&self) -> u64 {
        self.timer_ticket_id.load(Ordering::Acquire)
    }

    /// Set the timer ticket ID.
    #[inline]
    #[cfg(feature = "irq")]
    pub(crate) fn set_timer_ticket(&self, timer_ticket_id: u64) {
        // CAN NOT set timer_ticket_id to 0,
        // because 0 is used to indicate the timer event is expired.
        assert!(timer_ticket_id != 0);
        self.timer_ticket_id
            .store(timer_ticket_id, Ordering::Release);
    }

    /// Expire timer ticket ID by setting it to 0,
    /// it can be used to identify one timer event is triggered or expired.
    #[inline]
    #[cfg(feature = "irq")]
    pub(crate) fn timer_ticket_expired(&self) {
        self.timer_ticket_id.store(0, Ordering::Release);
    }

    #[inline]
    #[cfg(feature = "preempt")]
    pub(crate) fn set_preempt_pending(&self, pending: bool) {
        self.need_resched.store(pending, Ordering::Release)
    }

    #[inline]
    #[cfg(feature = "preempt")]
    pub(crate) fn set_force_resched_pending(&self, pending: bool) {
        self.force_resched.store(pending, Ordering::Release)
    }

    #[inline]
    #[cfg(feature = "preempt")]
    fn force_resched_pending(&self) -> bool {
        self.force_resched.load(Ordering::Acquire)
    }

    #[inline]
    #[cfg(all(test, feature = "preempt"))]
    pub(crate) fn preempt_pending_for_test(&self) -> bool {
        self.need_resched.load(Ordering::Acquire)
    }

    #[inline]
    #[cfg(all(test, feature = "preempt"))]
    pub(crate) fn force_resched_pending_for_test(&self) -> bool {
        self.force_resched_pending()
    }

    #[inline]
    #[cfg(feature = "preempt")]
    fn take_force_resched_pending(&self) -> bool {
        self.force_resched.swap(false, Ordering::AcqRel)
    }

    #[inline]
    #[cfg(feature = "preempt")]
    pub(crate) fn preempt_count(&self) -> usize {
        self.preempt_disable_count.load(Ordering::Acquire)
    }

    #[inline]
    #[cfg(feature = "preempt")]
    pub(crate) fn can_preempt(&self, current_disable_count: usize) -> bool {
        self.preempt_disable_count.load(Ordering::Acquire) == current_disable_count
    }

    #[inline]
    #[cfg(feature = "preempt")]
    pub(crate) fn disable_preempt(&self) {
        self.preempt_disable_count.fetch_add(1, Ordering::Release);
    }

    #[inline]
    #[cfg(feature = "preempt")]
    pub(crate) fn enable_preempt(&self, resched: bool) {
        if self.preempt_disable_count.fetch_sub(1, Ordering::Release) == 1 && resched {
            // If current task is pending to be preempted, do rescheduling.
            Self::current_check_preempt_pending();
        }
    }

    #[cfg(feature = "preempt")]
    fn current_check_preempt_pending() {
        use ax_kernel_guard::NoPreemptIrqSave;
        let curr = crate::current();
        if (curr.force_resched_pending() || curr.need_resched.load(Ordering::Acquire))
            && curr.can_preempt(0)
        {
            // Note: if we want to print log msg during `preempt_resched`, we have to
            // disable preemption here, because the ax-log may cause preemption.
            let mut rq = crate::current_run_queue::<NoPreemptIrqSave>();
            if curr.take_force_resched_pending() {
                #[cfg(all(feature = "smp", feature = "ipi"))]
                crate::run_queue::clear_remote_reschedule_pending_for_current_cpu();
                rq.force_resched()
            } else if curr.need_resched.load(Ordering::Acquire) {
                rq.preempt_resched()
            }
        }
    }

    /// Notify all tasks that join on this task.
    pub(crate) fn notify_exit(&self, exit_code: i32) {
        self.set_state(TaskState::Exited);
        self.exit_code.store(exit_code, Ordering::Release);
        self.wait_for_exit.notify_all(false);
    }

    #[inline]
    pub(crate) const unsafe fn ctx_mut_ptr(&self) -> *mut TaskContext {
        self.ctx.get()
    }

    #[inline]
    pub(crate) fn check_stack_canary(&self) {
        if self.kstack.is_canary_intact() {
            return;
        }

        panic!(
            "stack overflow/corruption detected for {}: stack=[{:#x}..{:#x}), expected magic={:#x}",
            self.id_name(),
            self.kstack.bottom().as_usize(),
            self.kstack.top().as_usize(),
            STACK_END_MAGIC
        );
    }

    /// Set the CPU ID where the task is running or will run.
    #[cfg(feature = "smp")]
    #[inline]
    pub(crate) fn set_cpu_id(&self, cpu_id: u32) {
        self.cpu_id.store(cpu_id, Ordering::Release);
    }

    /// Returns whether the task is running on a CPU.
    ///
    /// It is used to protect the task from being moved to a different run queue
    /// while it has not finished its scheduling process.
    /// The `on_cpu field is set to `true` when the task is preparing to run on a CPU,
    /// and it is set to `false` when the task has finished its scheduling process in `clear_prev_task_on_cpu()`.
    ///
    /// `SeqCst` because it participates in a store-before-load (Dekker) handshake
    /// with [`Self::stash_wake`]/[`Self::take_wake`] across two distinct atomics
    /// (`on_cpu` and `wake_handoff`); Acquire/Release would permit the
    /// "both sides observe the other's stale value" lost-wakeup execution.
    #[cfg(feature = "smp")]
    #[inline]
    pub(crate) fn on_cpu(&self) -> bool {
        self.on_cpu.load(Ordering::SeqCst)
    }

    /// Sets whether the task is running on a CPU. `SeqCst`, see [`Self::on_cpu`].
    #[cfg(feature = "smp")]
    #[inline]
    pub(crate) fn set_on_cpu(&self, on_cpu: bool) {
        self.on_cpu.store(on_cpu, Ordering::SeqCst)
    }

    /// Stash an owned reference for a deferred cross-core wake (see the
    /// `wake_handoff` field). Transfers ownership of `task` into the slot via
    /// `Arc::into_raw`. Must be paired with exactly one [`Self::take_wake`].
    #[cfg(feature = "smp")]
    #[inline]
    pub(crate) fn stash_wake(&self, task: AxTaskRef) {
        let ptr = Arc::into_raw(task) as *mut AxTask;
        // SeqCst: ordered with the `on_cpu` handshake (see `on_cpu`).
        self.wake_handoff.store(ptr, Ordering::SeqCst);
    }

    /// Atomically consume a stashed deferred-wake reference, if any. Returns the
    /// owned `AxTaskRef` to exactly one caller (the swap is the single arbiter);
    /// all other callers get `None`.
    #[cfg(feature = "smp")]
    #[inline]
    pub(crate) fn take_wake(&self) -> Option<AxTaskRef> {
        let ptr = self
            .wake_handoff
            .swap(core::ptr::null_mut(), Ordering::SeqCst);
        if ptr.is_null() {
            None
        } else {
            // Safety: `ptr` came from `Arc::into_raw` in `stash_wake`, and the
            // swap guarantees a single consumer, so this reconstructs the unique
            // owning `Arc` exactly once.
            Some(unsafe { Arc::from_raw(ptr as *const AxTask) })
        }
    }
}

impl fmt::Debug for TaskInner {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("TaskInner")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("state", &self.state())
            .finish()
    }
}

impl Drop for TaskInner {
    fn drop(&mut self) {
        debug!("task drop: {}", self.id_name());
    }
}

pub(crate) struct TaskStack {
    ptr: usize,
    size: usize,
    #[cfg(not(feature = "stack-guard-page"))]
    align: usize,
    #[cfg(feature = "stack-guard-page")]
    alloc_pages: usize,
    kind: TaskStackKind,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum TaskStackKind {
    #[cfg(not(feature = "stack-guard-page"))]
    Alloc,
    #[cfg(feature = "stack-guard-page")]
    GuardedAlloc,
    Borrowed,
}

impl TaskStack {
    pub fn alloc(size: usize) -> Self {
        cfg_if::cfg_if! {
            if #[cfg(feature = "stack-guard-page")] {
                Self::alloc_guarded(size)
            } else {
                Self::alloc_plain(size)
            }
        }
    }

    #[cfg(not(feature = "stack-guard-page"))]
    fn alloc_plain(size: usize) -> Self {
        let align = TASK_STACK_ALIGN;
        let layout = Layout::from_size_align(size, align).unwrap();
        let ptr = unsafe { alloc::alloc::alloc(layout) as usize };
        assert_ne!(ptr, 0, "task stack allocation failed");
        let stack = Self {
            ptr,
            size,
            align,
            kind: TaskStackKind::Alloc,
        };
        unsafe { stack.write_canary() };
        stack
    }

    #[cfg(feature = "stack-guard-page")]
    fn alloc_guarded(size: usize) -> Self {
        let usable_size = align_up_4k(size);
        let guarded_size = usable_size
            .checked_add(PAGE_SIZE_4K)
            .expect("guarded task stack size overflow");
        let pages = guarded_size / PAGE_SIZE_4K;
        let base = ax_alloc::global_allocator()
            .allocate_pages_raw(
                ax_alloc::PageRequest {
                    count: pages,
                    align: PAGE_SIZE_4K,
                    zone: ax_alloc::MemoryZone::Normal,
                },
                ax_alloc::UsageKind::Global,
            )
            .expect("guarded task stack allocation failed");
        let usable_bottom = base + PAGE_SIZE_4K;
        let stack = Self {
            ptr: usable_bottom,
            size: usable_size,
            alloc_pages: pages,
            kind: TaskStackKind::GuardedAlloc,
        };
        stack.unmap_guard_page();
        unsafe { stack.write_canary() };
        stack
    }

    pub fn borrowed(bottom: VirtAddr, size: usize, align: usize) -> Self {
        assert_ne!(bottom.as_usize(), 0, "static task stack pointer is null");
        #[cfg(feature = "stack-guard-page")]
        let _ = align;
        let stack = Self {
            ptr: bottom.as_usize(),
            size,
            #[cfg(not(feature = "stack-guard-page"))]
            align,
            #[cfg(feature = "stack-guard-page")]
            alloc_pages: 0,
            kind: TaskStackKind::Borrowed,
        };
        unsafe { stack.write_canary() };
        stack
    }

    #[inline]
    pub fn bottom(&self) -> VirtAddr {
        VirtAddr::from(self.ptr)
    }

    #[inline]
    pub fn top(&self) -> VirtAddr {
        VirtAddr::from(self.ptr + self.size)
    }

    #[cfg(feature = "stack-guard-page")]
    #[inline]
    fn guard_bottom(&self) -> VirtAddr {
        debug_assert_eq!(self.kind, TaskStackKind::GuardedAlloc);
        VirtAddr::from(self.ptr - PAGE_SIZE_4K)
    }

    #[cfg(feature = "stack-guard-page")]
    #[inline]
    fn guard_top(&self) -> VirtAddr {
        self.guard_bottom() + PAGE_SIZE_4K
    }

    #[cfg(feature = "stack-guard-page")]
    #[inline]
    fn contains_guard_addr(&self, addr: VirtAddr) -> bool {
        matches!(self.kind, TaskStackKind::GuardedAlloc)
            && self.guard_bottom() <= addr
            && addr < self.guard_top()
    }

    #[cfg(feature = "stack-guard-page")]
    fn unmap_guard_page(&self) {
        let guard_bottom = self.guard_bottom();
        ax_mm::kernel_aspace()
            .lock()
            .unmap(guard_bottom, PAGE_SIZE_4K)
            .expect("failed to unmap task stack guard page");
        flush_stack_guard_tlb(guard_bottom);
    }

    #[cfg(feature = "stack-guard-page")]
    fn remap_guard_page(&self) {
        let guard_bottom = self.guard_bottom();
        ax_mm::kernel_aspace()
            .lock()
            .map_linear(
                guard_bottom,
                ax_hal::mem::virt_to_phys(guard_bottom),
                PAGE_SIZE_4K,
                ax_hal::paging::MappingFlags::READ | ax_hal::paging::MappingFlags::WRITE,
            )
            .expect("failed to restore task stack guard page mapping");
        flush_stack_guard_tlb(guard_bottom);
    }

    #[inline]
    fn canary_ptr(&self) -> *mut usize {
        self.ptr as *mut usize
    }

    #[inline]
    unsafe fn write_canary(&self) {
        unsafe { self.canary_ptr().write(STACK_END_MAGIC) };
    }

    #[inline]
    pub fn is_canary_intact(&self) -> bool {
        unsafe { self.canary_ptr().read() == STACK_END_MAGIC }
    }

    #[cfg(all(test, not(feature = "stack-guard-page")))]
    fn corrupt_canary_for_test(&self) {
        unsafe { self.canary_ptr().write(0) };
    }
}

#[cfg(all(
    feature = "stack-guard-page",
    not(all(feature = "smp", feature = "ipi"))
))]
fn flush_stack_guard_tlb(vaddr: VirtAddr) {
    ax_hal::asm::flush_tlb(Some(vaddr));
}

#[cfg(all(feature = "stack-guard-page", feature = "smp", feature = "ipi"))]
fn flush_stack_guard_tlb(vaddr: VirtAddr) {
    let _guard = ax_kernel_guard::NoPreempt::new();
    let current_cpu = ax_hal::percpu::this_cpu_id();
    let ack_count = Arc::new(AtomicUsize::new(0));
    let mut remote_cpu_count = 0;

    core::sync::atomic::fence(Ordering::SeqCst);

    for cpu_id in 0..ax_hal::cpu_num() {
        if cpu_id == current_cpu || !ax_ipi::wait_until_cpu_ready(cpu_id) {
            continue;
        }

        remote_cpu_count += 1;
        let ack_count = ack_count.clone();
        ax_ipi::run_on_cpu(cpu_id, move || {
            ax_hal::asm::flush_tlb(Some(vaddr));
            ack_count.fetch_add(1, Ordering::Release);
        });
    }

    ax_hal::asm::flush_tlb(Some(vaddr));
    if remote_cpu_count == 0 {
        return;
    }

    const MAX_WAIT_NS: u64 = 5 * ax_hal::time::NANOS_PER_SEC;
    let start = ax_hal::time::monotonic_time_nanos();
    while ack_count.load(Ordering::Acquire) != remote_cpu_count {
        core::hint::spin_loop();
        if ax_hal::time::monotonic_time_nanos() - start > MAX_WAIT_NS {
            let acked = ack_count.load(Ordering::Acquire);
            panic!(
                "task stack guard page TLB shootdown timeout: CPU {current_cpu} got \
                 {acked}/{remote_cpu_count} ack(s) for vaddr={vaddr:#x}"
            );
        }
    }
}

#[cfg(feature = "stack-guard-page")]
impl TaskInner {
    /// Reports whether `fault_addr` hits this task's stack guard page.
    pub fn diagnose_stack_guard_page_fault(&self, fault_addr: VirtAddr) -> bool {
        if !self.kstack.contains_guard_addr(fault_addr) {
            return false;
        }

        error!(
            "task stack guard page hit for {}: fault_addr={:#x}, stack=[{:#x}..{:#x}), \
             guard=[{:#x}..{:#x})",
            self.id_name(),
            fault_addr.as_usize(),
            self.kstack.bottom().as_usize(),
            self.kstack.top().as_usize(),
            self.kstack.guard_bottom().as_usize(),
            self.kstack.guard_top().as_usize(),
        );
        true
    }
}

impl Drop for TaskStack {
    fn drop(&mut self) {
        match self.kind {
            #[cfg(not(feature = "stack-guard-page"))]
            TaskStackKind::Alloc => {
                let layout = Layout::from_size_align(self.size, self.align).unwrap();
                unsafe { alloc::alloc::dealloc(self.ptr as *mut u8, layout) }
            }
            #[cfg(feature = "stack-guard-page")]
            TaskStackKind::GuardedAlloc => {
                self.remap_guard_page();
                // SAFETY: TaskStack owns the entire guarded allocation and
                // records its original page count until this single Drop.
                unsafe {
                    ax_alloc::global_allocator().deallocate_pages_raw(
                        self.guard_bottom().as_usize(),
                        ax_alloc::PageRequest {
                            count: self.alloc_pages,
                            align: PAGE_SIZE_4K,
                            zone: ax_alloc::MemoryZone::Normal,
                        },
                        ax_alloc::UsageKind::Global,
                    );
                }
            }
            TaskStackKind::Borrowed => {}
        }
    }
}

#[cfg(test)]
mod stack_tests {
    use super::{TASK_STACK_ALIGN, TaskStack};

    #[cfg(not(feature = "stack-guard-page"))]
    #[test]
    fn task_stack_canary_detects_corruption() {
        let stack = TaskStack::alloc(0x1000);
        assert!(stack.is_canary_intact());

        stack.corrupt_canary_for_test();

        assert!(!stack.is_canary_intact());
    }

    #[cfg(not(feature = "stack-guard-page"))]
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn task_stack_top_stays_16_byte_aligned() {
        // x86_64 TaskContext::init() builds the initial switch frame from
        // kstack_top and assumes the ABI-required 16-byte stack alignment.
        let stack = TaskStack::alloc(0x1000);
        assert_eq!(stack.top().as_usize() % TASK_STACK_ALIGN, 0);
    }

    #[cfg(feature = "stack-guard-page")]
    #[test]
    fn borrowed_task_stack_top_stays_16_byte_aligned_with_guard_feature() {
        let stack = TaskStack::borrowed(0x1000.into(), 0x1000, TASK_STACK_ALIGN);
        assert_eq!(stack.top().as_usize() % TASK_STACK_ALIGN, 0);
    }
}

/// A wrapper of [`AxTaskRef`] as the current task.
///
/// It won't change the reference count of the task when created or dropped.
pub struct CurrentTask(ManuallyDrop<AxTaskRef>);

impl CurrentTask {
    pub(crate) fn try_get() -> Option<Self> {
        // SAFETY: the scheduler keeps one raw strong reference for the current
        // task until `set_current` transfers ownership to the next task. This
        // bootstrap read is also used by the preemption guard implementation,
        // so it cannot require that same guard to have been acquired already.
        let header = unsafe { ax_hal::percpu::current_thread_raw().as_ref()? };
        let ptr = header.current_context()?.as_usize() as *const super::AxTask;
        if !ptr.is_null() {
            Some(Self(unsafe { ManuallyDrop::new(AxTaskRef::from_raw(ptr)) }))
        } else {
            None
        }
    }

    pub(crate) fn get() -> Self {
        Self::try_get().expect("current task is uninitialized")
    }

    /// Clone the inner `AxTaskRef`.
    #[allow(clippy::should_implement_trait)]
    pub fn clone(&self) -> AxTaskRef {
        self.0.deref().clone()
    }

    /// Returns `true` if the current task is the same as `other`.
    pub fn ptr_eq(&self, other: &AxTaskRef) -> bool {
        Arc::ptr_eq(&self.0, other)
    }

    pub(crate) unsafe fn init_current(init_task: AxTaskRef) {
        assert!(init_task.is_init());
        // SAFETY: scheduler initialization runs on an offline CPU before any
        // task switch or migration can occur.
        let header = init_task.current_header();
        unsafe {
            ax_hal::percpu::with_cpu_pin(|pin| {
                #[cfg(feature = "tls")]
                ax_hal::percpu::install_bootstrap_kernel_tls(
                    pin,
                    KernelTlsBase::new(init_task.tls.tls_ptr() as usize),
                );
                ax_hal::percpu::install_bootstrap_thread(pin, header)
            })
        }
        .expect("CPU-local area must precede task initialization")
        .expect("bootstrap current-thread state must install");
        let _ = Arc::into_raw(init_task);
    }

    pub(crate) unsafe fn set_current(prev: Self, next: AxTaskRef) {
        let Self(arc) = prev;
        ManuallyDrop::into_inner(arc); // `call Arc::drop()` to decrease prev task reference count.
        let _ = Arc::into_raw(next);
    }
}

impl Deref for CurrentTask {
    type Target = AxTaskRef;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

extern "C" fn task_entry() -> ! {
    unsafe {
        // Clear the prev task on CPU before running the task entry function.
        crate::run_queue::clear_prev_task_on_cpu();
    }
    // Enable irq (if feature "irq" is enabled) before running the task entry function.
    #[cfg(all(feature = "irq", not(feature = "host-test")))]
    ax_hal::asm::enable_irqs();
    let task = crate::current();
    if let Some(entry) = task.entry.take() {
        entry()
    }
    crate::exit(0);
}
