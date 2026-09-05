use alloc::{boxed::Box, string::String, sync::Arc};
#[cfg(not(target_os = "none"))]
use core::alloc::Layout;
#[cfg(feature = "smp")]
use core::sync::atomic::AtomicPtr;
#[cfg(feature = "uspace")]
use core::sync::atomic::AtomicUsize;
use core::{
    cell::{Cell, UnsafeCell},
    fmt,
    mem::{ManuallyDrop, offset_of},
    ops::Deref,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicU64, Ordering},
    task::{Context, Poll},
};

#[cfg(feature = "tls")]
use ax_hal::tls::TlsArea;
use ax_hal::{
    context::{KernelTlsBase, TaskContext},
    percpu::ExecutionContextHeader,
};
use ax_lazyinit::LazyInit;
#[cfg(feature = "uspace")]
use ax_memory_addr::PhysAddr;
use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr, align_up_4k};
use futures_util::task::AtomicWaker;

#[cfg(feature = "lockdep")]
use crate::lockdep::HeldLockStack;
use crate::{
    AxCpuMask, AxTask, AxTaskRef, WaitQueue,
    interrupt::{InterruptSnapshot, InterruptState},
    sync::SpinLock,
};

#[cfg(target_pointer_width = "64")]
const STACK_END_MAGIC: usize = 0x57AC_CE11_57AC_CE11usize;
#[cfg(target_pointer_width = "32")]
const STACK_END_MAGIC: usize = 0x57AC_CE11usize;

/// Required alignment for task kernel stacks. x86_64 task context setup relies
/// on the ABI-mandated 16-byte stack alignment at task entry.
pub(crate) const TASK_STACK_ALIGN: usize = 16;

/// Stable root used by scheduler-owned kernel tasks on architectures where
/// kernel and userspace share one hardware page-table register.
///
/// A task may be created while its creator is running under a process root.
/// Sampling the live CR3/SATP in `TaskContext::new` would then give the kernel
/// task an untracked borrow of that process page table.  Publish the real
/// kernel root once during scheduler bring-up instead, mirroring Linux kernel
/// threads' explicit `active_mm` ownership rather than treating a register
/// snapshot as a lifetime proof.
#[cfg(feature = "uspace")]
static KERNEL_TASK_PAGE_TABLE_ROOT: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "uspace")]
pub(crate) fn initialize_kernel_task_address_space() {
    let root = if cfg!(target_arch = "loongarch64") {
        // LoongArch Linux initializes PGDL with `invalid_pg_dir` and retains
        // the active userspace PGDL across kernel-thread switches. Keep the
        // boot PGDL as our stable no-user fallback; PGDH is a different root
        // and zero is not a valid lazy-TLB context on this architecture.
        ax_hal::asm::read_user_page_table().as_usize()
    } else if ax_hal::mem::user_aspace_needs_kernel_mappings() {
        ax_hal::asm::read_kernel_page_table().as_usize()
    } else {
        // AArch64 retains its kernel root in a separate register and permits
        // an empty userspace root when no lazy activation exists.
        0
    };
    match KERNEL_TASK_PAGE_TABLE_ROOT.compare_exchange(0, root, Ordering::AcqRel, Ordering::Acquire)
    {
        Ok(_) => {}
        Err(published) => assert_eq!(
            published, root,
            "all CPUs must agree on the scheduler kernel page-table root"
        ),
    }
}

#[cfg(feature = "uspace")]
fn kernel_task_address_space() -> ax_hal::context::InstalledAddressSpace {
    let root = KERNEL_TASK_PAGE_TABLE_ROOT.load(Ordering::Acquire);
    assert!(
        (!ax_hal::mem::user_aspace_needs_kernel_mappings() && !cfg!(target_arch = "loongarch64"))
            || root != 0,
        "scheduler kernel page-table root was not initialized"
    );
    ax_hal::context::InstalledAddressSpace::kernel(PhysAddr::from_usize(root))
}

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

/// Failure to construct the owned kernel stack of a task.
///
/// Task-stack allocation is deliberately fallible so user-task creation can
/// unwind and report `ENOMEM` instead of entering the global allocation-error
/// handler. Kernel bootstrap paths may still use [`TaskInner::new`] when stack
/// exhaustion is an unrecoverable initialization failure.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum TaskCreateError {
    /// The requested stack size is zero or overflows page-size alignment.
    #[error("invalid task stack size")]
    InvalidStackSize,
    /// The page allocator could not provide backing for the task stack.
    #[error("task stack allocation failed: {0}")]
    StackAllocation(#[source] ax_alloc::AllocError),
    /// The kernel virtual address space could not publish the stack mapping.
    #[cfg(feature = "vmap-task-stack")]
    #[error("task stack virtual mapping failed: {0}")]
    StackMapping(#[source] ax_mm::MmError),
}

/// Task-owned wrapper around the scheduler-neutral architecture header.
///
/// The header is the first field so the architecture `current` identity can
/// be converted directly to this wrapper without a second publication slot.
#[repr(C)]
struct TaskExecutionContext {
    header: ExecutionContextHeader,
    owner: NonNull<AxTask>,
}

impl TaskExecutionContext {
    fn new(owner: NonNull<AxTask>, bootstrap: bool) -> Self {
        Self {
            header: if bootstrap {
                ExecutionContextHeader::new_bootstrap()
            } else {
                ExecutionContextHeader::new()
            },
            owner,
        }
    }

    /// Reconstructs the task wrapper whose offset-zero header is current.
    ///
    /// # Safety
    ///
    /// `header` must point to the header of a live `TaskExecutionContext`, and
    /// the scheduler must retain the raw current-task reference while used.
    unsafe fn from_header(header: NonNull<ExecutionContextHeader>) -> &'static Self {
        unsafe { &*header.as_ptr().cast::<Self>() }
    }
}

const _: () = assert!(offset_of!(TaskExecutionContext, header) == 0);

#[cfg(feature = "task-ext")]
pub use ax_hal::context::{
    InstalledAddressSpace as TaskAddressSpace, InstalledAddressSpaceMode as TaskAddressSpaceMode,
};

/// Proof that the scheduler completed the architecture address-space switch
/// on one CPU. External task extensions can inspect but cannot construct this
/// token; it is produced only after `TaskContext::prepare_switch_to` returns.
#[derive(Debug, Clone, Copy)]
#[cfg(feature = "task-ext")]
pub struct AddressSpaceSwitchProof {
    cpu: usize,
}

#[cfg(feature = "task-ext")]
impl AddressSpaceSwitchProof {
    #[cfg(feature = "uspace")]
    pub(crate) const fn new(cpu: usize) -> Self {
        Self { cpu }
    }

    pub const fn cpu(&self) -> usize {
        self.cpu
    }
}

/// Proof that an offline path installed the kernel root and completed a local
/// full TLB flush before releasing the current MM activation.
#[derive(Debug, Clone, Copy)]
#[cfg(feature = "task-ext")]
pub struct CpuOfflineRootSwitchProof {
    cpu: usize,
}

#[cfg(feature = "task-ext")]
impl CpuOfflineRootSwitchProof {
    #[cfg(feature = "uspace")]
    const fn new(cpu: usize) -> Self {
        Self { cpu }
    }

    pub const fn cpu(&self) -> usize {
        self.cpu
    }
}

/// OS-owned, already allocated lifetime anchor for CPU activations.
/// Callbacks run with IRQs disabled and must neither allocate nor sleep.
#[cfg(feature = "task-ext")]
pub trait SchedulerAddressSpaceOwner: Send + Sync {
    /// Releases one activation after a verified root switch.
    fn release_after_root_switch(self: Arc<Self>, proof: AddressSpaceSwitchProof);
    /// Releases one activation after the offline kernel-root switch.
    fn release_after_kernel_switch(self: Arc<Self>, proof: CpuOfflineRootSwitchProof);
    /// Retains an activation whose hardware root was not proved inactive.
    fn abandon(self: Arc<Self>, cpu: usize);
}

/// Move-only scheduler activation with inline identity and an existing owner.
/// Constructing or releasing it requires no Box or new Arc allocation. Kernel
/// tasks retain this value per CPU for Linux-style active_mm/lazy-TLB use.
#[cfg(feature = "task-ext")]
pub struct SchedulerAddressSpaceActivation {
    installed: TaskAddressSpace,
    cpu: usize,
    owner: Option<Arc<dyn SchedulerAddressSpaceOwner>>,
}

#[cfg(feature = "task-ext")]
impl SchedulerAddressSpaceActivation {
    /// Transfers one activation already acquired from `owner` into the
    /// scheduler. `owner` must retain the root throughout this token's life.
    pub fn new(
        installed: TaskAddressSpace,
        cpu: usize,
        owner: Arc<dyn SchedulerAddressSpaceOwner>,
    ) -> Self {
        Self {
            installed,
            cpu,
            owner: Some(owner),
        }
    }

    /// Returns the complete root/tag/epoch identity held by this activation.
    pub const fn installed(&self) -> TaskAddressSpace {
        self.installed
    }

    /// Releases this activation only after the matching CPU switched roots.
    pub fn release_after_root_switch(mut self, proof: AddressSpaceSwitchProof) {
        assert_eq!(self.cpu, proof.cpu());
        self.owner
            .take()
            .expect("activation is consumed once")
            .release_after_root_switch(proof);
    }

    /// Releases this activation after CPU offline completed the kernel switch.
    pub fn release_after_kernel_switch(mut self, proof: CpuOfflineRootSwitchProof) {
        assert_eq!(self.cpu, proof.cpu());
        self.owner
            .take()
            .expect("activation is consumed once")
            .release_after_kernel_switch(proof);
    }
}

#[cfg(feature = "task-ext")]
impl Drop for SchedulerAddressSpaceActivation {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            owner.abandon(self.cpu);
        }
    }
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
    /// Called after the current task has become permanently exited, but before
    /// the scheduler switches away from its architectural context.
    ///
    /// The task cannot become runnable again once this hook starts. Extensions
    /// may therefore release task-local lifetime pins here while retaining any
    /// per-CPU activation lease until [`TaskExt::on_switch_complete`]. The hook
    /// runs with local IRQs disabled and must not sleep.
    fn on_exit(&self) {}
    /// Acquires the non-cloneable activation that the scheduler will own for
    /// this CPU. Extensions without a userspace MM return `None` and execute
    /// under the per-CPU lazy address space.
    fn acquire_address_space_activation(
        &self,
        _cpu: usize,
    ) -> Option<SchedulerAddressSpaceActivation> {
        None
    }
}

/// The inner task structure.
pub struct TaskInner {
    id: TaskId,
    name: SpinLock<String>,
    is_idle: bool,
    is_init: bool,

    entry: Cell<Option<Box<dyn FnOnce()>>>,
    state: AtomicU8,

    /// CPU affinity mask.
    cpumask: SpinLock<AxCpuMask>,

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
    timer_ticket_id: AtomicU64,

    #[cfg(feature = "preempt")]
    need_resched: AtomicBool,
    #[cfg(feature = "preempt")]
    force_resched: AtomicBool,

    interrupted: InterruptState,
    interrupt_waker: AtomicWaker,

    exit_code: AtomicI32,
    wait_for_exit: WaitQueue,

    kstack: TaskStack,
    ctx: UnsafeCell<TaskContext>,
    /// Pinned identity and CPU-binding state published by the switch tail.
    execution_context: LazyInit<TaskExecutionContext>,
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
    ///
    /// # Panics
    ///
    /// Panics if the task stack cannot be allocated. User-facing task creation
    /// should use [`Self::try_new`] and propagate the typed failure instead.
    pub fn new<F>(entry: F, name: String, stack_size: usize) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        Self::try_new(entry, name, stack_size).expect("task stack allocation failed")
    }

    /// Tries to create a task with an owned page-backed kernel stack.
    pub fn try_new<F>(entry: F, name: String, stack_size: usize) -> Result<Self, TaskCreateError>
    where
        F: FnOnce() + Send + 'static,
    {
        let kstack = TaskStack::try_alloc(stack_size)?;
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
        Ok(t)
    }

    /// Gets the ID of the task.
    pub const fn id(&self) -> TaskId {
        self.id
    }

    /// Gets the name of the task.
    pub fn name(&self) -> String {
        self.name.lock_irqsave().clone()
    }

    /// Set the name of the task.
    pub fn set_name(&self, name: &str) {
        *self.name.lock_irqsave() = String::from(name);
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

    /// Replaces the scheduler-owned activation of the current task and
    /// switches the hardware address space immediately.
    #[cfg(all(feature = "uspace", feature = "task-ext"))]
    pub fn replace_address_space_activation(
        &self,
        activation: SchedulerAddressSpaceActivation,
    ) -> AddressSpaceSwitchProof {
        let installed = activation.installed();
        assert!(
            installed.is_user(),
            "scheduler requires a userspace identity"
        );
        let _guard = crate::sync::PreemptIrqSaveGuard::new();
        assert!(
            core::ptr::eq(self, &***crate::current()),
            "only the current task may replace the installed address space"
        );
        // SAFETY: we are the current task and no other thread touches our ctx.
        unsafe { (*self.ctx.get()).set_address_space(installed) };
        // SAFETY: the current task owns this CPU context and the caller cannot
        // migrate until the address-space transaction completes.
        unsafe { (*self.ctx.get()).activate_address_space() };
        let proof = AddressSpaceSwitchProof::new(ax_hal::percpu::this_cpu_id());
        crate::run_queue::replace_current_address_space_activation(activation, proof);
        proof
    }

    /// Removes the current task's userspace root from a CPU that is going
    /// offline, then lets its extension release the non-cloneable activation.
    ///
    /// # Safety
    ///
    /// The caller must have disabled local interrupts and migration, and must
    /// not return to userspace on this CPU. On error, the stable kernel
    /// context remains installed but the CPU must not be powered off or reused
    /// until the TLB withdrawal is repaired.
    #[cfg(all(feature = "uspace", feature = "task-ext"))]
    pub unsafe fn deactivate_address_space_for_cpu_offline(
        &self,
    ) -> Result<ax_hal::cache::CurrentCpuTlbOffline, ax_hal::cache::TlbShootdownError> {
        let kernel = kernel_task_address_space();
        // SAFETY: guaranteed by this method's caller contract.
        unsafe { (*self.ctx.get()).set_address_space(kernel) };
        // SAFETY: guaranteed by this method's caller contract.
        unsafe { (*self.ctx.get()).activate_address_space() };
        ax_hal::asm::flush_tlb(None);
        let proof = CpuOfflineRootSwitchProof::new(ax_hal::percpu::this_cpu_id());
        crate::run_queue::release_current_address_space_after_kernel_switch(proof);
        // SAFETY: the kernel root and local full flush precede the extension
        // token release, which withdraws the OS-owned activation lease.
        unsafe { ax_hal::cache::withdraw_current_cpu_tlb_ready() }
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
        *self.cpumask.lock_irqsave()
    }

    /// Sets the cpu affinity mask of the task.
    ///
    /// # Arguments
    /// `cpumask` - The cpu affinity mask to be set in type [`AxCpuMask`].
    #[inline]
    pub fn set_cpumask(&self, cpumask: AxCpuMask) {
        *self.cpumask.lock_irqsave() = cpumask
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
        if self.interrupted.consume() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    /// Acknowledges all interruption publications visible at this call.
    ///
    /// Publications that race after the internal snapshot remain pending.
    #[inline]
    pub fn clear_interrupt(&self) {
        let snapshot = self.interrupt_snapshot();
        self.acknowledge_interrupt(snapshot);
    }

    /// Consumes the interruption publications currently visible to this task.
    ///
    /// Returns `true` if the task was interrupted.
    #[inline]
    pub fn take_interrupt(&self) -> bool {
        self.interrupted.consume()
    }

    /// Checks whether the task has been interrupted without clearing
    /// the flag.
    ///
    /// This is a non-consuming read, unlike [`Self::take_interrupt`]. Use this
    /// when the interrupt flag needs to remain set for subsequent
    /// consumers (e.g., an [`crate::future::interruptible`] future wrapper).
    #[inline]
    pub fn interrupted(&self) -> bool {
        self.interrupted.is_pending()
    }

    /// Interrupts the task.
    #[inline]
    pub fn interrupt(&self) {
        self.interrupted.publish();
        self.interrupt_waker.wake();
    }

    /// Captures the interruption publications visible before a safe-point scan.
    #[inline]
    pub fn interrupt_snapshot(&self) -> InterruptSnapshot {
        self.interrupted.snapshot()
    }

    /// Acknowledges the interruption publications covered by `snapshot`.
    #[inline]
    pub fn acknowledge_interrupt(&self, snapshot: InterruptSnapshot) {
        self.interrupted.acknowledge(snapshot);
    }
}

// private methods
impl TaskInner {
    fn new_common(id: TaskId, name: String, kstack: TaskStack) -> Self {
        #[cfg(feature = "uspace")]
        let mut context = TaskContext::new();
        #[cfg(not(feature = "uspace"))]
        let context = TaskContext::new();
        #[cfg(feature = "uspace")]
        context.set_address_space(kernel_task_address_space());
        Self {
            id,
            name: SpinLock::new(name),
            is_idle: false,
            is_init: false,
            entry: Cell::new(None),
            state: AtomicU8::new(TaskState::Ready as u8),
            // By default, the task is allowed to run on all CPUs.
            cpumask: SpinLock::new(crate::api::cpu_mask_full()),
            sched_policy: AtomicI32::new(0),
            sched_priority: AtomicI32::new(0),
            in_wait_queue: AtomicBool::new(false),
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
            interrupted: InterruptState::new(),
            interrupt_waker: AtomicWaker::new(),
            exit_code: AtomicI32::new(0),
            wait_for_exit: WaitQueue::new(),
            kstack,
            ctx: UnsafeCell::new(context),
            execution_context: LazyInit::new(),
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
        let owner = NonNull::from(Arc::as_ref(&task));
        let execution_context = task
            .execution_context
            .init_once(TaskExecutionContext::new(owner, task.is_init()));
        // SAFETY: the header is stored in the Arc-owned task and never moves
        // after this task becomes visible to a scheduler.
        let header = unsafe { Pin::new_unchecked(&execution_context.header) };
        // SAFETY: the Arc is not visible to any scheduler yet, so this is the
        // only access to its architecture context.
        unsafe { (*task.ctx_mut_ptr()).set_context_header(header.as_non_null()) };
        task
    }

    pub(crate) fn context_header(&self) -> Pin<&ExecutionContextHeader> {
        let execution_context = self
            .execution_context
            .get()
            .expect("task execution context must be initialized after Arc allocation");
        // SAFETY: `into_arc` initializes this field only after the containing
        // scheduler task reaches its permanent Arc allocation.
        unsafe { Pin::new_unchecked(&execution_context.header) }
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
    pub(crate) fn timer_ticket(&self) -> u64 {
        self.timer_ticket_id.load(Ordering::Acquire)
    }

    /// Set the timer ticket ID.
    #[inline]
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
    #[cfg(feature = "preempt")]
    pub(crate) fn preemption_pending(&self) -> bool {
        self.force_resched_pending() || self.need_resched.load(Ordering::Acquire)
    }

    #[inline]
    #[cfg(feature = "preempt")]
    fn take_force_resched_pending(&self) -> bool {
        self.force_resched.swap(false, Ordering::AcqRel)
    }

    #[inline]
    #[cfg(feature = "preempt")]
    pub(crate) fn preempt_count(&self) -> usize {
        #[cfg(feature = "host-test")]
        return 0;
        #[cfg(not(feature = "host-test"))]
        crate::runtime_preempt::depth()
    }

    #[inline]
    #[cfg(feature = "preempt")]
    pub(crate) fn can_preempt(&self, current_disable_count: usize) -> bool {
        #[cfg(feature = "host-test")]
        return crate::sync::host_preempt_depth() == current_disable_count;
        #[cfg(not(feature = "host-test"))]
        {
            crate::runtime_preempt::depth() == current_disable_count
        }
    }

    #[cfg(feature = "preempt")]
    pub(crate) fn current_check_preempt_pending() {
        use crate::sync::PreemptIrqSaveState;
        let curr = crate::current();
        if (curr.force_resched_pending() || curr.need_resched.load(Ordering::Acquire))
            && curr.can_preempt(0)
        {
            // Note: if we want to print log msg during `preempt_resched`, we have to
            // disable preemption here, because the ax-log may cause preemption.
            let mut rq = crate::current_run_queue::<PreemptIrqSaveState>();
            if curr.take_force_resched_pending() {
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
    #[cfg(not(target_os = "none"))]
    align: usize,
    #[cfg(all(target_os = "none", not(feature = "vmap-task-stack")))]
    alloc_pages: usize,
    #[cfg(all(target_os = "none", feature = "vmap-task-stack"))]
    virtual_allocation: Option<ax_mm::KernelVirtualAllocation>,
    kind: TaskStackKind,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum TaskStackKind {
    #[cfg(not(target_os = "none"))]
    HostAlloc,
    #[cfg(all(target_os = "none", not(feature = "vmap-task-stack")))]
    PageAlloc,
    #[cfg(all(target_os = "none", feature = "vmap-task-stack"))]
    VirtualAlloc,
    Borrowed,
}

impl TaskStack {
    #[cfg(any(test, feature = "host-test"))]
    pub fn alloc(size: usize) -> Self {
        Self::try_alloc(size).expect("task stack allocation failed")
    }

    fn try_alloc(size: usize) -> Result<Self, TaskCreateError> {
        let size = checked_stack_size(size)?;
        cfg_if::cfg_if! {
            if #[cfg(all(target_os = "none", feature = "vmap-task-stack"))] {
                Self::alloc_virtual(size)
            } else if #[cfg(target_os = "none")] {
                Self::alloc_pages(size)
            } else {
                Self::alloc_host(size)
            }
        }
    }

    #[cfg(not(target_os = "none"))]
    fn alloc_host(size: usize) -> Result<Self, TaskCreateError> {
        let align = TASK_STACK_ALIGN;
        let layout =
            Layout::from_size_align(size, align).map_err(|_| TaskCreateError::InvalidStackSize)?;
        let ptr = unsafe { alloc::alloc::alloc(layout) as usize };
        if ptr == 0 {
            return Err(TaskCreateError::StackAllocation(
                ax_alloc::AllocError::NoMemory,
            ));
        }
        let stack = Self {
            ptr,
            size,
            align,
            kind: TaskStackKind::HostAlloc,
        };
        unsafe { stack.write_canary() };
        Ok(stack)
    }

    #[cfg(all(target_os = "none", not(feature = "vmap-task-stack")))]
    fn alloc_pages(size: usize) -> Result<Self, TaskCreateError> {
        let pages = size / PAGE_SIZE_4K;
        let ptr = ax_alloc::global_allocator()
            .alloc_pages(pages, PAGE_SIZE_4K, ax_alloc::UsageKind::TaskStack)
            .map_err(TaskCreateError::StackAllocation)?;
        let stack = Self {
            ptr,
            size,
            alloc_pages: pages,
            kind: TaskStackKind::PageAlloc,
        };
        unsafe { stack.write_canary() };
        Ok(stack)
    }

    #[cfg(all(target_os = "none", feature = "vmap-task-stack"))]
    fn alloc_virtual(size: usize) -> Result<Self, TaskCreateError> {
        let layout = ax_mm::KernelVirtualAllocationLayout::new(
            size,
            ax_hal::paging::MappingFlags::READ | ax_hal::paging::MappingFlags::WRITE,
            ax_alloc::UsageKind::TaskStack,
        )
        .map_err(map_virtual_stack_error)?
        .with_leading_guard_pages(usize::from(cfg!(feature = "stack-guard-page")))
        .map_err(map_virtual_stack_error)?;
        let virtual_allocation =
            ax_mm::KernelVirtualAllocation::allocate(layout).map_err(map_virtual_stack_error)?;
        let usable_bottom = virtual_allocation.usable_range().start.as_usize();
        let stack = Self {
            ptr: usable_bottom,
            size,
            virtual_allocation: Some(virtual_allocation),
            kind: TaskStackKind::VirtualAlloc,
        };
        unsafe { stack.write_canary() };
        Ok(stack)
    }

    pub fn borrowed(bottom: VirtAddr, size: usize, align: usize) -> Self {
        assert_ne!(bottom.as_usize(), 0, "static task stack pointer is null");
        #[cfg(target_os = "none")]
        let _ = align;
        let stack = Self {
            ptr: bottom.as_usize(),
            size,
            #[cfg(not(target_os = "none"))]
            align,
            #[cfg(all(target_os = "none", not(feature = "vmap-task-stack")))]
            alloc_pages: 0,
            #[cfg(all(target_os = "none", feature = "vmap-task-stack"))]
            virtual_allocation: None,
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

    #[cfg(all(target_os = "none", feature = "stack-guard-page"))]
    #[inline]
    fn guard_bottom(&self) -> VirtAddr {
        debug_assert_eq!(self.kind, TaskStackKind::VirtualAlloc);
        VirtAddr::from(self.ptr - PAGE_SIZE_4K)
    }

    #[cfg(all(target_os = "none", feature = "stack-guard-page"))]
    #[inline]
    fn guard_top(&self) -> VirtAddr {
        self.guard_bottom() + PAGE_SIZE_4K
    }

    #[cfg(all(target_os = "none", feature = "stack-guard-page"))]
    #[inline]
    fn contains_guard_addr(&self, addr: VirtAddr) -> bool {
        matches!(self.kind, TaskStackKind::VirtualAlloc)
            && self.guard_bottom() <= addr
            && addr < self.guard_top()
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

#[cfg(all(target_os = "none", feature = "stack-guard-page"))]
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

#[cfg(all(not(target_os = "none"), feature = "stack-guard-page"))]
impl TaskInner {
    /// Host tests have no page-table-backed guard page.
    pub fn diagnose_stack_guard_page_fault(&self, _fault_addr: VirtAddr) -> bool {
        false
    }
}

impl Drop for TaskStack {
    fn drop(&mut self) {
        match self.kind {
            #[cfg(not(target_os = "none"))]
            TaskStackKind::HostAlloc => {
                let layout = Layout::from_size_align(self.size, self.align).unwrap();
                unsafe { alloc::alloc::dealloc(self.ptr as *mut u8, layout) }
            }
            #[cfg(all(target_os = "none", not(feature = "vmap-task-stack")))]
            TaskStackKind::PageAlloc => {
                ax_alloc::global_allocator().dealloc_pages(
                    self.ptr,
                    self.alloc_pages,
                    ax_alloc::UsageKind::TaskStack,
                );
            }
            #[cfg(all(target_os = "none", feature = "vmap-task-stack"))]
            TaskStackKind::VirtualAlloc => {
                drop(self.virtual_allocation.take());
            }
            TaskStackKind::Borrowed => {}
        }
    }
}

#[cfg(all(target_os = "none", feature = "vmap-task-stack"))]
fn map_virtual_stack_error(error: ax_mm::MmError) -> TaskCreateError {
    match error {
        ax_mm::MmError::InvalidInput(_) => TaskCreateError::InvalidStackSize,
        ax_mm::MmError::NoMemory => {
            TaskCreateError::StackAllocation(ax_alloc::AllocError::NoMemory)
        }
        other => TaskCreateError::StackMapping(other),
    }
}

fn checked_stack_size(size: usize) -> Result<usize, TaskCreateError> {
    if size == 0 || size.checked_add(PAGE_SIZE_4K - 1).is_none() {
        return Err(TaskCreateError::InvalidStackSize);
    }
    Ok(align_up_4k(size))
}

#[cfg(test)]
mod stack_tests {
    use super::{TASK_STACK_ALIGN, TaskCreateError, TaskStack};

    #[test]
    fn task_stack_rejects_zero_sized_backing() {
        assert!(matches!(
            TaskStack::try_alloc(0),
            Err(TaskCreateError::InvalidStackSize)
        ));
    }

    #[test]
    fn task_stack_rejects_page_alignment_overflow() {
        assert!(matches!(
            TaskStack::try_alloc(usize::MAX),
            Err(TaskCreateError::InvalidStackSize)
        ));
    }

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
        let header = NonNull::new(unsafe { ax_hal::percpu::current_context_raw() }.cast_mut())?;
        if ax_hal::percpu::is_permanent_boot_context(header) {
            return None;
        }
        // SAFETY: scheduler publication accepts only the offset-zero header of
        // a live task wrapper and retains a raw strong reference while current.
        let context = unsafe { TaskExecutionContext::from_header(header) };
        Some(Self(unsafe {
            ManuallyDrop::new(AxTaskRef::from_raw(context.owner.as_ptr()))
        }))
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
        let header = init_task.context_header();
        unsafe {
            ax_hal::percpu::with_cpu_pin(|pin| {
                #[cfg(feature = "tls")]
                ax_hal::percpu::install_bootstrap_kernel_tls(
                    pin,
                    KernelTlsBase::new(init_task.tls.tls_ptr() as usize),
                );
                ax_hal::percpu::install_bootstrap_context(pin, header)
            })
        }
        .expect("CPU-local area must precede task initialization")
        .expect("bootstrap current-context state must install");
        let _ = Arc::into_raw(init_task);
    }

    pub(crate) unsafe fn set_current(prev: Self, next: AxTaskRef) {
        let Self(arc) = prev;
        ManuallyDrop::into_inner(arc); // `call Arc::drop()` to decrease prev task reference count.
        let _ = Arc::into_raw(next);
    }
}

#[cfg(all(test, feature = "host-test"))]
mod current_task_tests {
    #[test]
    fn permanent_boot_context_is_not_a_published_task() {
        std::thread::spawn(|| {
            ax_hal::percpu::initialize_host_test_cpu();
            assert!(super::CurrentTask::try_get().is_none());
        })
        .join()
        .expect("boot-context probe panicked");
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
    // A CPU-owned preemption word carries the switch guard across the raw
    // transfer. Unlike a resumed task, a new task has no suspended caller to
    // finish that guard, so its first-entry tail completes the handoff here.
    crate::runtime_preempt::finish_initial_context_switch();
    // Enable IRQs before running the task entry function.
    #[cfg(not(feature = "host-test"))]
    ax_hal::asm::enable_irqs();
    let task = crate::current();
    if let Some(entry) = task.entry.take() {
        entry()
    }
    crate::exit(0);
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    fn task_id_and_state_hold_for_test() -> bool {
        // Test TaskId
        let id1 = TaskId(1);
        let id2 = TaskId(2);
        assert!(id1 != id2);
        assert_eq!(id1, TaskId(1));

        // Test TaskState variants
        assert!(TaskState::Running as u8 == 1);
        assert!(TaskState::Ready as u8 == 2);

        true
    }

    fn task_constants_hold_for_test() -> bool {
        // Test TASK_STACK_ALIGN constant
        assert_eq!(TASK_STACK_ALIGN, 16);

        // Test STACK_END_MAGIC for 64-bit
        #[cfg(target_pointer_width = "64")]
        const {
            assert!(STACK_END_MAGIC == 0x57AC_CE11_57AC_CE11usize);
        }

        true
    }

    fn task_id_operations_hold_for_test() -> bool {
        // Test TaskId operations
        let id1 = TaskId(100);
        let id2 = TaskId(200);

        // Test equality
        assert_eq!(id1, TaskId(100));
        assert!(id1 != id2);

        // Test copy
        let id4 = id1;
        assert!(id4 == id1);

        true
    }

    fn task_state_all_variants_hold_for_test() -> bool {
        // Test all TaskState variants
        let running = TaskState::Running;
        let ready = TaskState::Ready;
        let blocked = TaskState::Blocked;
        let exited = TaskState::Exited;

        // Verify all are different
        assert!(core::mem::discriminant(&running) != core::mem::discriminant(&ready));
        assert!(core::mem::discriminant(&ready) != core::mem::discriminant(&blocked));
        assert!(core::mem::discriminant(&blocked) != core::mem::discriminant(&exited));

        // Verify ordinal values
        assert!(running as u8 == 1);
        assert!(ready as u8 == 2);
        assert!(blocked as u8 == 3);
        assert!(exited as u8 == 4);

        true
    }

    #[test]
    fn task_id_and_state_hold() {
        assert!(task_id_and_state_hold_for_test());
    }

    #[test]
    fn task_constants_hold() {
        assert!(task_constants_hold_for_test());
    }

    #[test]
    fn task_id_operations_hold() {
        assert!(task_id_operations_hold_for_test());
    }

    #[test]
    fn task_state_all_variants_hold() {
        assert!(task_state_all_variants_hold_for_test());
    }
}
