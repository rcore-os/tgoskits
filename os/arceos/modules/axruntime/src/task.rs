//! ArceOS ownership and trait-FFI glue for the OS-independent task system.

use alloc::{boxed::Box, string::String};
#[cfg(feature = "uspace")]
use core::marker::PhantomData;
use core::{
    alloc::Layout,
    cell::UnsafeCell,
    mem::offset_of,
    pin::Pin,
    ptr::{self, NonNull},
    sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU64, Ordering},
};

use ax_cpu_local::{
    ContextIdentity, CpuAreaPrefixV2, CpuBindingEpoch, CpuPin, CurrentThreadHeader, ThreadIdentity,
};
use ax_kspin::{IrqGuard, SpinNoIrq};
use ax_lazyinit::LazyInit;
pub use ax_task::{
    CpuId, CpuSet, DeadlineFlags, DeadlinePolicy, FairMode, IrqRegisterResult, IrqWaitCell,
    IrqWaitRegistration, IrqWakeHandle, Nice, RtPriority, SchedulePolicy, SwitchReason, TaskError,
    ThreadExtension, ThreadExtensionOps, ThreadHandle, ThreadId, ThreadPolicyApplied, ThreadState,
    ThreadWakeHandle, WaitQueue, WakeResult, current_cpu_needs_resched, current_thread_extension,
    current_thread_handle, current_thread_id, executor::LocalExecutor, exit_current_thread,
    runtime::SchedSwitchRecord, schedule_current_cpu, set_current_thread_affinity,
    set_thread_affinity, set_thread_policy, sleep, sleep_until, thread_affinity, thread_handle,
    thread_policy, thread_round_robin_interval_ns, thread_runtime, yield_current_cpu,
};
use ax_task::{
    CpuLocal, CpuLocalOwnerBorrow, CpuRemote, TaskSystem, TaskSystemConfig, ThreadResources,
    ThreadSpec, impl_trait as impl_task_runtime,
    runtime::{
        AddressSpaceHandle, ContextThreadBinding, CpuRemoteHandle, CurrentCpuLocalHandle,
        ExecutionContextHandle, IrqGuardToken, KernelContextRequest, RuntimeCpuId,
        RuntimeHandleResult, RuntimeStatus, StackHandle, StackRequest, TaskRuntime,
        TaskSystemHandle, TlsHandle, TlsRequest, UserContextRequest,
    },
};

static TASK_SYSTEM: LazyInit<Pin<Box<TaskSystem>>> = LazyInit::new();

/// The already-running primary context is the unikernel's process owner.
///
/// Unlike a spawned runtime thread, it has no join record: returning from it
/// terminates the whole system. Retaining its generation-checked identity
/// keeps that role explicit instead of inferring it from a missing extension.
static PRIMARY_BOOTSTRAP_THREAD: LazyInit<PrimaryBootstrapThread> = LazyInit::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PrimaryBootstrapThread(ThreadId);

impl PrimaryBootstrapThread {
    fn owns(self, thread: ThreadId) -> bool {
        self.0 == thread
    }
}

static TASK_TIMER_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);

static SCHED_SWITCH_TRACE_HOOK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

#[ax_percpu::def_percpu]
static CPU_LOCAL: LazyInit<Pin<Box<CpuLocal>>> = LazyInit::new();

/// Owner-capability address published once before this CPU becomes online.
///
/// The pointer originates from the unique pinned allocation, rather than a
/// shared `CpuLocal` borrow, so the scheduler may later reconstruct a mutable
/// owner borrow while no shared query is live.
#[ax_percpu::def_percpu]
static CPU_LOCAL_OWNER_HANDLE: usize = 0;

#[cfg(feature = "tls")]
#[ax_percpu::def_percpu]
static EARLY_BOOTSTRAP_TLS: usize = 0;

#[cfg(feature = "uspace")]
#[ax_percpu::def_percpu]
static KERNEL_ADDRESS_SPACE_ROOT: usize = 0;

#[cfg(feature = "irq")]
#[ax_percpu::def_percpu]
static NEXT_TASK_TIMER_DEADLINE_NS: u64 = 0;

const PAGE_SIZE: usize = 4096;

/// Opaque runtime token for one user page-table root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskAddressSpace(AddressSpaceHandle);

impl TaskAddressSpace {
    /// Creates a token from a non-zero physical page-table root address.
    pub fn from_page_table_root(root: usize) -> Result<Self, TaskError> {
        if root == 0 {
            Err(TaskError::InvalidRuntimeHandle)
        } else {
            // SAFETY: the non-zero root is the runtime's address-space token;
            // the OS that creates this wrapper owns the corresponding page
            // tables for every scheduler record that retains the token.
            Ok(Self(unsafe { AddressSpaceHandle::from_raw(root) }))
        }
    }
}

/// Allocation-free scheduler-switch diagnostic hook installed by an OS layer.
pub type SchedSwitchTraceHook = fn(SchedSwitchRecord);

/// Execution domain charged by a user-context boundary transition.
#[cfg(feature = "uspace")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserExecutionState {
    /// The thread is about to execute its user register context.
    User,
    /// The thread returned to the kernel through an exception boundary.
    Kernel,
}

/// Runtime-owned notification level checked at the final user-entry boundary.
///
/// Producers first publish the underlying OS work, then call [`Self::publish`]
/// with Release ordering, and finally wake the owning scheduler thread. The
/// owner snapshots an epoch before draining that work and acknowledges only
/// the captured epoch afterwards. Consequently, a concurrent producer always
/// leaves `produced != acknowledged` even when an older drain completes later.
///
/// This object is intentionally concrete: [`run_user_context`] reads its two
/// atomics directly while raw IRQs are masked and never invokes an OS callback.
#[cfg(feature = "uspace")]
#[derive(Debug)]
pub struct UserEntryNotification {
    produced: AtomicU64,
    acknowledged: AtomicU64,
}

#[cfg(feature = "uspace")]
impl UserEntryNotification {
    /// Creates an empty notification level.
    pub const fn new() -> Self {
        Self {
            produced: AtomicU64::new(0),
            acknowledged: AtomicU64::new(0),
        }
    }

    /// Publishes work before the caller wakes the owning scheduler thread.
    ///
    /// This operation is allocation-free, lock-free, and hard-IRQ-safe. Epoch
    /// exhaustion is a fatal runtime invariant violation. The terminal
    /// `u64::MAX` epoch is a permanently pending poison value and is never
    /// acknowledged, so exhaustion cannot wrap into an apparently drained
    /// state.
    pub fn publish(&self) {
        let advanced =
            self.produced
                .fetch_update(Ordering::Release, Ordering::Relaxed, |current| {
                    current.checked_add(1)
                });
        match advanced {
            Ok(previous) if previous + 1 != u64::MAX => {}
            Ok(_) | Err(_) => panic!("user-entry notification epoch exhausted"),
        }
    }

    /// Captures the newest publication that the owner is about to drain.
    pub fn snapshot(&self) -> UserEntryTicket<'_> {
        UserEntryTicket {
            notification: self,
            epoch: self.produced.load(Ordering::Acquire),
            _not_send_or_sync: PhantomData,
        }
    }

    /// Reports whether this notification advanced after `ticket` was issued.
    pub fn changed_since(&self, ticket: &UserEntryTicket<'_>) -> bool {
        assert!(
            core::ptr::eq(self, ticket.notification),
            "user-entry ticket belongs to another notification"
        );
        self.produced.load(Ordering::Acquire) != ticket.epoch
    }

    /// Acknowledges all work through `ticket` without clearing newer work.
    pub fn acknowledge(&self, ticket: UserEntryTicket<'_>) -> UserEntryAck {
        assert!(
            core::ptr::eq(self, ticket.notification),
            "user-entry ticket belongs to another notification"
        );
        self.acknowledge_epoch(ticket.epoch)
    }

    /// Tests the coalesced level without consuming any publication.
    pub fn pending(&self) -> bool {
        self.pending_irqoff()
    }

    fn pending_irqoff(&self) -> bool {
        let acknowledged = self.acknowledged.load(Ordering::Acquire);
        self.produced.load(Ordering::Acquire) != acknowledged
    }

    fn acknowledge_epoch(&self, epoch: u64) -> UserEntryAck {
        if epoch == u64::MAX {
            return UserEntryAck::Pending;
        }
        self.acknowledged.fetch_max(epoch, Ordering::AcqRel);
        if self.pending() {
            UserEntryAck::Pending
        } else {
            UserEntryAck::Stable
        }
    }
}

#[cfg(feature = "uspace")]
impl Default for UserEntryNotification {
    fn default() -> Self {
        Self::new()
    }
}

/// Linear snapshot of one [`UserEntryNotification`] publication epoch.
///
/// The ticket is deliberately neither `Send` nor `Sync`: only the owning
/// kernel thread may decide that its user-return work has been drained. A
/// baseline observer may borrow it repeatedly through
/// [`UserEntryNotification::changed_since`], while
/// [`UserEntryNotification::acknowledge`] consumes it exactly once.
#[cfg(feature = "uspace")]
#[derive(Debug)]
#[must_use = "a user-entry ticket must be observed or acknowledged by its owner"]
pub struct UserEntryTicket<'notification> {
    notification: &'notification UserEntryNotification,
    epoch: u64,
    _not_send_or_sync: PhantomData<*mut ()>,
}

/// Result of acknowledging one user-entry work snapshot.
#[cfg(feature = "uspace")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "pending user-entry work must be retried before architectural entry"]
pub enum UserEntryAck {
    /// No newer producer is visible; the OS drain reached a stable point.
    Stable,
    /// A producer published after the snapshot; the OS drain must retry.
    Pending,
}

/// Result of one runtime-owned attempt to enter a user register context.
#[cfg(feature = "uspace")]
#[derive(Debug)]
#[must_use = "deferred user entry must drain work; an exit must be dispatched"]
pub enum RunUserContextOutcome {
    /// Pending OS work prevented architectural user entry.
    Deferred,
    /// The architecture entered user mode and returned through this exit.
    Exited(ax_hal::cpu::uspace::UserExitReason),
}

/// Bounded CPU-accounting capability used by the raw user-context boundary.
///
/// # Safety
///
/// [`UserContextAccounting::transition_irqoff`] is called with raw local IRQs
/// disabled and before ordinary task context has been restored. Implementors
/// must perform bounded, non-faulting atomic work only. They must not allocate,
/// acquire a lock, block, schedule, invoke an arbitrary callback, enable IRQs,
/// or retain a reference to boundary-owned state.
#[cfg(feature = "uspace")]
pub unsafe trait UserContextAccounting {
    /// Charges execution up to `now_ns` and publishes the new execution state.
    fn transition_irqoff(&self, state: UserExecutionState, now_ns: u64);
}

#[cfg(feature = "uspace")]
fn user_boundary_result(status: RuntimeStatus) -> Result<(), TaskError> {
    match status {
        RuntimeStatus::Success => Ok(()),
        RuntimeStatus::UnsafeContext => Err(TaskError::UnsafeContext),
        status => Err(TaskError::RuntimeFailure(status as u32)),
    }
}

#[cfg(feature = "uspace")]
fn require_masked_user_boundary(boundary: crate::guard::UserContextBoundary) {
    if crate::guard::validate_user_context_boundary(boundary) != RuntimeStatus::Success {
        // The validator already emitted a fixed, allocation-free diagnostic.
        // Reopening IRQs after a guard or baton mismatch would make corrupted
        // CPU-local ownership observable, so every post-mask failure is fatal.
        panic!("IRQ-masked user-context boundary invariant failed");
    }
}

/// Runs one user context and verifies the complete runtime-owned transition.
///
/// The runtime masks raw IRQs before it publishes user accounting. Architecture
/// entry returns with IRQs still masked after restoring the kernel register
/// state, so kernel accounting is published before any timer lock, signal work,
/// or other ordinary task-context operation can run. Each phase proves that no
/// IRQ guard, preemption guard, hard-IRQ marker, or scheduler baton escaped. The
/// checks reject inconsistent state; they never repair unknown nesting.
#[cfg(feature = "uspace")]
pub fn run_user_context(
    context: &mut ax_hal::cpu::uspace::UserContext,
    accounting: &impl UserContextAccounting,
    notification: &UserEntryNotification,
) -> Result<RunUserContextOutcome, TaskError> {
    user_boundary_result(crate::guard::validate_user_context_boundary(
        crate::guard::UserContextBoundary::TaskEntry,
    ))?;

    ax_hal::asm::disable_irqs();
    require_masked_user_boundary(crate::guard::UserContextBoundary::UserEntry);

    // Match Linux's exit-to-user work loop: this is the final level read after
    // raw IRQ masking. Returning Deferred neither changes virtual-time
    // accounting to User nor invokes the architecture entry assembly. A
    // producer racing after this read Release-publishes before its direct wake;
    // the pending IPI is therefore taken immediately when user IRQ state opens.
    if notification.pending_irqoff() {
        require_masked_user_boundary(crate::guard::UserContextBoundary::TaskReturn);
        ax_hal::asm::enable_irqs();
        return Ok(RunUserContextOutcome::Deferred);
    }

    let user_entry_ns = ax_hal::time::monotonic_time_nanos() as u64;
    accounting.transition_irqoff(UserExecutionState::User, user_entry_ns);
    let raw_exit = context.run_raw();

    let user_return_ns = ax_hal::time::monotonic_time_nanos() as u64;
    accounting.transition_irqoff(UserExecutionState::Kernel, user_return_ns);
    require_masked_user_boundary(crate::guard::UserContextBoundary::KernelEntry);

    let reason = match context.decode_raw_exit(raw_exit) {
        ax_hal::cpu::uspace::DecodedUserExit::Interrupt(interrupt) => {
            let _handled = interrupt.dispatch();
            ax_hal::cpu::uspace::UserExitReason::Interrupt
        }
        ax_hal::cpu::uspace::DecodedUserExit::Reason(reason) => reason,
    };
    require_masked_user_boundary(crate::guard::UserContextBoundary::TaskReturn);

    // `run_raw` and IRQ-return scheduling both preserve the runtime-owned raw
    // mask. Reopen IRQs only after accounting, decode/dispatch, and the final
    // guard/baton postcondition have all committed.
    ax_hal::asm::enable_irqs();
    Ok(RunUserContextOutcome::Exited(reason))
}

/// Installs the process-wide scheduler-switch diagnostic consumer.
///
/// Reinstalling the same function is harmless; replacing a live consumer is an
/// invariant violation because switches may concurrently execute the hook.
pub fn install_sched_switch_trace_hook(hook: SchedSwitchTraceHook) {
    let hook = hook as *mut ();
    match SCHED_SWITCH_TRACE_HOOK.compare_exchange(
        core::ptr::null_mut(),
        hook,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => {}
        Err(installed) => assert_eq!(installed, hook, "scheduler trace hook already installed"),
    }
}

/// Reports whether a kernel page fault hit the current runtime stack guard.
pub fn diagnose_current_stack_guard_page_fault(fault: ax_memory_addr::VirtAddr) -> bool {
    #[cfg(feature = "stack-guard-page")]
    {
        // SAFETY: trap execution cannot migrate before returning through its
        // architecture epilogue. The pinned current-thread header is the sole
        // source of the matching runtime context and stack identity.
        let cpu_pin = unsafe { CpuPin::new_unchecked() };
        let Ok((_prefix, context)) = current_runtime_context(&cpu_pin) else {
            return false;
        };
        let stack = context.stack.into_raw();
        if stack == 0 {
            return false;
        }
        // SAFETY: the scheduler owns the stack resource until after its context
        // can no longer run, and the current header keeps this context on-CPU.
        let stack = unsafe { &*ptr::with_exposed_provenance::<RuntimeStack>(stack) };
        let StackBacking::GuardedPages { guard_size, .. } = &stack.backing else {
            return false;
        };
        let guard_end = stack.base.saturating_add(*guard_size);
        if !(stack.base..guard_end).contains(&fault.as_usize()) {
            return false;
        }
        error!(
            "task stack guard page hit: fault_addr={:#x}, stack=[{:#x}..{:#x}), \
             guard=[{:#x}..{:#x})",
            fault.as_usize(),
            guard_end,
            stack.usable_top,
            stack.base,
            guard_end,
        );
        true
    }
    #[cfg(not(feature = "stack-guard-page"))]
    {
        let _ = fault;
        false
    }
}

/// Replaces the current user context's page-table root and installs it now.
///
/// This operation is valid only for the running thread during an `exec`-style
/// address-space replacement.
pub fn switch_current_page_table(root: usize) -> Result<(), TaskError> {
    if root == 0 {
        return Err(TaskError::InvalidRuntimeHandle);
    }
    #[cfg(feature = "uspace")]
    {
        let _irq = IrqGuard::new();
        let root = ax_memory_addr::PhysAddr::from(root);
        // SAFETY: the exec caller transfers a live process page-table root;
        // the scheduler retains only its opaque identity while the process MM
        // remains the allocation owner.
        let address_space = unsafe { AddressSpaceHandle::from_raw(root.as_usize()) };
        // Keep the scheduler endpoint and hardware root coherent across exec.
        // TaskContext deliberately owns no address-space register state.
        let _old_address_space = ax_task::replace_current_address_space(address_space)?;
        let status = install_runtime_address_space(address_space);
        if status == RuntimeStatus::Success {
            Ok(())
        } else {
            Err(runtime_status_error(status))
        }
    }
    #[cfg(not(feature = "uspace"))]
    {
        let _ = root;
        Err(TaskError::RuntimeFailure(RuntimeStatus::Unsupported as u32))
    }
}
#[cfg(not(feature = "fs"))]
const DEFAULT_TASK_STACK_SIZE: usize = 256 * 1024;

struct RuntimeStack {
    #[cfg(feature = "paging")]
    base: usize,
    usable_top: usize,
    backing: StackBacking,
}

impl RuntimeStack {
    fn usable_bounds(&self) -> Option<axbacktrace::StackBounds> {
        let start = match &self.backing {
            StackBacking::Heap { pointer, .. } => pointer.as_ptr().expose_provenance(),
            #[cfg(feature = "paging")]
            StackBacking::GuardedPages { guard_size, .. } => self.base.checked_add(*guard_size)?,
        };
        (start < self.usable_top).then(|| axbacktrace::StackBounds::new(start, self.usable_top))
    }
}

enum StackBacking {
    Heap {
        pointer: NonNull<u8>,
        layout: Layout,
    },
    #[cfg(feature = "paging")]
    GuardedPages { pages: usize, guard_size: usize },
}

#[derive(Clone, Copy)]
struct RuntimeSwitchTail {
    previous: NonNull<CurrentThreadHeader>,
    binding_epoch: CpuBindingEpoch,
}

/// Runtime-owned architecture context and its pinned scheduler identity.
///
/// The header stays at offset zero so a current-thread publication can be
/// checked against its context handle without a second registry or per-CPU
/// pointer. `switch_tail` is written only while this context is off-CPU and is
/// consumed exactly once after it becomes current with local IRQs disabled.
#[repr(C)]
struct RuntimeContext {
    header: CurrentThreadHeader,
    inner: Box<UnsafeCell<ax_hal::context::TaskContext>>,
    stack: StackHandle,
    switch_tail: UnsafeCell<Option<RuntimeSwitchTail>>,
}

const _: () = assert!(offset_of!(RuntimeContext, header) == 0);

impl RuntimeContext {
    fn allocate(inner: ax_hal::context::TaskContext, stack: StackHandle) -> *mut RuntimeContext {
        let inner = Box::new(UnsafeCell::new(inner));
        let identity = ContextIdentity::from_raw(inner.get().expose_provenance())
            .expect("an architecture context allocation must have a non-zero identity");
        Box::into_raw(Box::new(Self {
            header: CurrentThreadHeader::new(identity),
            inner,
            stack,
            switch_tail: UnsafeCell::new(None),
        }))
    }

    fn header(&self) -> Pin<&CurrentThreadHeader> {
        // SAFETY: every RuntimeContext is constructed in a Box and is never
        // moved before destruction after its header is no longer published.
        unsafe { Pin::new_unchecked(&self.header) }
    }

    fn context_identity(&self) -> ContextIdentity {
        self.header.context_identity()
    }

    fn architecture_context_identity(&self) -> usize {
        self.inner.get().expose_provenance()
    }

    fn switch_tail(&self) -> Option<RuntimeSwitchTail> {
        // SAFETY: only the incoming scheduler continuation reads this slot;
        // the context is current and local IRQs serialize scheduler entry.
        unsafe { *self.switch_tail.get() }
    }

    unsafe fn stage_switch_tail(&self, tail: RuntimeSwitchTail) -> Result<(), RuntimeStatus> {
        // SAFETY: the scheduler selected this context while it is off-CPU and
        // holds the only right to prepare its next incoming continuation.
        let slot = unsafe { &mut *self.switch_tail.get() };
        if slot.is_some() {
            return Err(RuntimeStatus::Busy);
        }
        *slot = Some(tail);
        Ok(())
    }

    unsafe fn finish_switch_tail(&self) -> RuntimeStatus {
        // SAFETY: the current incoming continuation owns this slot with local
        // IRQs disabled; copy first so a failed unbind remains retryable.
        let slot = unsafe { &mut *self.switch_tail.get() };
        let Some(tail) = *slot else {
            return RuntimeStatus::InvalidHandle;
        };
        // SAFETY: the outgoing header stays pinned and unreclaimable through
        // the scheduler `on_cpu` handoff; this tail owns its exact epoch.
        let previous = unsafe { Pin::new_unchecked(tail.previous.as_ref()) };
        if unsafe { previous.unbind_cpu(tail.binding_epoch) }.is_err() {
            return RuntimeStatus::Busy;
        }
        *slot = None;
        RuntimeStatus::Success
    }
}

fn current_cpu_prefix(cpu_pin: &CpuPin) -> Result<&CpuAreaPrefixV2, RuntimeStatus> {
    let area_base = ax_hal::percpu::cpu_base(cpu_pin)
        .map_err(|_| RuntimeStatus::NotInitialized)?
        .as_ptr()
        .expose_provenance();
    // SAFETY: the typed CPU-local query verified the complete immutable area
    // identity while `cpu_pin` prevents migration. The prefix lives until
    // shutdown, and this borrow is limited to the supplied pin.
    Ok(unsafe { &*ptr::with_exposed_provenance::<CpuAreaPrefixV2>(area_base) })
}

fn runtime_context(
    handle: ExecutionContextHandle,
) -> Result<&'static RuntimeContext, RuntimeStatus> {
    if handle.is_none() {
        return Err(RuntimeStatus::InvalidHandle);
    }
    let context = ptr::with_exposed_provenance::<RuntimeContext>(handle.into_raw());
    // SAFETY: TaskRuntime receives only live handles created by this provider;
    // the scheduler retains context ownership through every runtime call.
    let context = unsafe { &*context };
    if context.context_identity().as_usize() != context.architecture_context_identity() {
        return Err(RuntimeStatus::InvalidHandle);
    }
    Ok(context)
}

fn current_runtime_context(
    cpu_pin: &CpuPin,
) -> Result<(&CpuAreaPrefixV2, &'static RuntimeContext), RuntimeStatus> {
    let prefix = current_cpu_prefix(cpu_pin)?;
    let current = ax_hal::percpu::current_thread(cpu_pin)
        .map_err(|_| RuntimeStatus::InvalidHandle)?
        .as_ptr()
        .expose_provenance();
    let binding = prefix.header().binding();
    if current == 0 || current == binding.boot_thread {
        return Err(RuntimeStatus::NotInitialized);
    }
    let header = ptr::with_exposed_provenance::<CurrentThreadHeader>(current);
    // SAFETY: the CPU runtime slot may publish only a pinned header whose live
    // CPU binding matches this prefix. The supplied pin covers the load and
    // every validation below.
    let header = unsafe { &*header };
    // RuntimeContext is `repr(C)` and the pinned header is its offset-zero
    // owner identity. The independently allocated architecture context keeps
    // ContextIdentity free of self-referential outer pointers.
    let context = unsafe { &*ptr::from_ref(header).cast::<RuntimeContext>() };
    if !ptr::eq(context.header().get_ref(), header)
        || context.context_identity().as_usize() != context.architecture_context_identity()
    {
        return Err(RuntimeStatus::InvalidHandle);
    }
    let Some(current_binding) = header.cpu_binding() else {
        return Err(RuntimeStatus::InvalidHandle);
    };
    if current_binding.area_base() != binding.area_base
        || current_binding.cpu_index().as_u32() != binding.cpu_index
    {
        return Err(RuntimeStatus::InvalidHandle);
    }
    Ok((prefix, context))
}

/// Returns the exact mapped stack allocation owned by the current scheduler
/// context without consulting the thread registry or a broad kernel VA range.
pub(crate) fn current_kernel_stack_bounds() -> Option<axbacktrace::StackBounds> {
    // Fatal diagnostics may run because LockRuntime itself is inconsistent, so
    // this capability must not recursively enter a context-aware guard. One raw
    // local-IRQ window pins current/header/stack validation without allocation,
    // registry locking, or scheduler callbacks.
    let restore_irqs = ax_hal::asm::irqs_enabled();
    ax_hal::asm::disable_irqs();
    let bounds = (|| {
        // SAFETY: raw local IRQ exclusion prevents a scheduler return or CPU
        // migration until this closure has copied the immutable bounds.
        let cpu_pin = unsafe { CpuPin::new_unchecked() };
        let (_prefix, context) = current_runtime_context(&cpu_pin).ok()?;
        let stack = context.stack;
        if stack.is_none() {
            return None;
        }
        // SAFETY: the current context owns this stack handle until it is
        // off-CPU and reaped; raw IRQ exclusion pins the validation above.
        let stack = unsafe { &*ptr::with_exposed_provenance::<RuntimeStack>(stack.into_raw()) };
        stack.usable_bounds()
    })();
    if restore_irqs {
        ax_hal::asm::enable_irqs();
    }
    bounds
}

unsafe fn prepare_current_runtime_context_publish<'pin>(
    cpu_pin: &'pin CpuPin,
    context: &'pin RuntimeContext,
) -> Result<ax_hal::percpu::PreparedCurrentThreadPublish<'pin>, RuntimeStatus> {
    let header = context.header();
    // SAFETY: the caller already bound this pinned header to the current CPU and
    // excludes traps/scheduler re-entry until the raw context switch.
    unsafe { ax_hal::percpu::prepare_current_thread_publish(cpu_pin, header) }
        .map_err(|_| RuntimeStatus::InvalidHandle)
}

fn bind_bootstrap_runtime_context(
    cpu_pin: &CpuPin,
    handle: ExecutionContextHandle,
    kernel_tls: usize,
) -> Result<(), TaskError> {
    let prefix = current_cpu_prefix(cpu_pin).map_err(runtime_status_error)?;
    let binding = prefix.header().binding();
    let boot_thread =
        ax_hal::percpu::current_thread(cpu_pin).map_err(|_| TaskError::InvalidConfiguration)?;
    if boot_thread.as_ptr().expose_provenance() != binding.boot_thread {
        return Err(TaskError::InvalidConfiguration);
    }
    let context = runtime_context(handle).map_err(runtime_status_error)?;
    // SAFETY: the CPU is offline with IRQs disabled, this context was just
    // installed as its bootstrap scheduler record, and the header stays pinned
    // in the runtime allocation until that record is reaped.
    unsafe { context.header().bind_cpu(binding) }.map_err(|_| TaskError::InvalidConfiguration)?;
    #[cfg(feature = "tls")]
    unsafe {
        // SAFETY: bootstrap runs in the platform's IRQ-disabled CPU-binding
        // window and the scheduler record now owns the matching TLS resource.
        ax_hal::percpu::install_bootstrap_kernel_tls(ax_hal::context::KernelTlsBase::new(
            kernel_tls,
        ))
        .map_err(|_| TaskError::InvalidConfiguration)?;
        let prepared = prepare_current_runtime_context_publish(cpu_pin, context)
            .map_err(runtime_status_error)?;
        ax_hal::percpu::commit_current_thread_publish(prepared);
    }
    #[cfg(not(feature = "tls"))]
    unsafe {
        assert_eq!(
            kernel_tls, 0,
            "TLS-disabled bootstrap must retain a zero TLS identity"
        );
        // SAFETY: the CPU is still offline with IRQs disabled. The typed HAL
        // operation installs the LinuxCurrent register and matching Release
        // slot as one bootstrap-only transition.
        ax_hal::percpu::install_bootstrap_current_thread(cpu_pin, context.header())
            .map_err(|_| TaskError::InvalidConfiguration)?;
    }
    Ok(())
}

fn finish_runtime_context_switch_tail() -> RuntimeStatus {
    // SAFETY: TaskSystem calls this only with its scheduler-frame pin and local
    // IRQs disabled after execution has entered the incoming context.
    let cpu_pin = unsafe { CpuPin::new_unchecked() };
    let Ok((_prefix, current)) = current_runtime_context(&cpu_pin) else {
        return RuntimeStatus::InvalidHandle;
    };
    // SAFETY: the incoming context exclusively owns its staged one-shot tail.
    unsafe { current.finish_switch_tail() }
}

struct InitialContextState {
    address_space: AddressSpaceHandle,
    #[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
    fp_state: Option<ax_hal::cpu::FpState>,
}

impl InitialContextState {
    const fn kernel() -> Self {
        Self {
            address_space: AddressSpaceHandle::NONE,
            #[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
            fp_state: None,
        }
    }

    const fn user(address_space: TaskAddressSpace) -> Self {
        Self {
            address_space: address_space.0,
            #[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
            fp_state: None,
        }
    }
}

#[cfg(feature = "tls")]
struct RuntimeTls {
    area: ax_hal::tls::TlsArea,
}

type KernelThreadEntry = Box<dyn FnOnce() + Send + 'static>;

struct RuntimeThreadData {
    entry: SpinNoIrq<Option<KernelThreadEntry>>,
    exit_code: AtomicI32,
    exit_completed: AtomicBool,
    join_wait: WaitQueue,
    os_extension: Option<ThreadExtension>,
    _name: String,
}

/// OS extension borrowed through the runtime's outer scheduler extension.
#[derive(Debug)]
pub struct ThreadOsExtensionBorrow<'thread> {
    _runtime: ax_task::ThreadExtensionBorrow<'thread>,
    data: usize,
    ops: &'static ThreadExtensionOps,
}

impl ThreadOsExtensionBorrow<'_> {
    /// Returns the OS-owned opaque value.
    pub const fn data(&self) -> usize {
        self.data
    }

    /// Returns the callback table used as the OS extension type identity.
    pub const fn ops(&self) -> &'static ThreadExtensionOps {
        self.ops
    }
}

/// OS extension lease for current-thread lookups without an existing handle.
#[derive(Debug)]
pub struct ThreadOsExtensionLease {
    _runtime: ax_task::ThreadExtensionLease,
    data: usize,
    ops: &'static ThreadExtensionOps,
}

impl ThreadOsExtensionLease {
    /// Returns the OS-owned opaque value.
    pub const fn data(&self) -> usize {
        self.data
    }

    /// Returns the callback table used as the OS extension type identity.
    pub const fn ops(&self) -> &'static ThreadExtensionOps {
        self.ops
    }
}

impl RuntimeThreadData {
    fn new(entry: KernelThreadEntry, name: String, os_extension: Option<ThreadExtension>) -> Self {
        Self {
            entry: SpinNoIrq::new(Some(entry)),
            exit_code: AtomicI32::new(0),
            exit_completed: AtomicBool::new(false),
            join_wait: WaitQueue::new(),
            os_extension,
            _name: name,
        }
    }
}

static RUNTIME_THREAD_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: runtime_thread_switch_in_hook,
    on_switch_out: runtime_thread_switch_out_hook,
    on_policy_applied: runtime_thread_policy_applied_hook,
    on_exit: runtime_thread_exit_hook,
    on_deadline_overrun: runtime_thread_deadline_overrun_hook,
    drop: runtime_thread_drop_hook,
};

unsafe extern "Rust" fn runtime_thread_switch_in_hook(data: usize, thread: ThreadId) {
    let runtime = unsafe { runtime_thread_data_from_raw(data) };
    if let Some(extension) = runtime.os_extension.as_ref() {
        // SAFETY: `spawn_raw_with_extension` retains the OS extension until the
        // outer runtime extension is reaped and forwards the same thread ID.
        unsafe { (extension.ops().on_switch_in)(extension.data(), thread) };
    }
}

unsafe extern "Rust" fn runtime_thread_switch_out_hook(
    data: usize,
    thread: ThreadId,
    reason: SwitchReason,
) {
    let runtime = unsafe { runtime_thread_data_from_raw(data) };
    if let Some(extension) = runtime.os_extension.as_ref() {
        // SAFETY: same composition contract as `runtime_thread_switch_in_hook`.
        unsafe { (extension.ops().on_switch_out)(extension.data(), thread, reason) };
    }
}

unsafe extern "Rust" fn runtime_thread_policy_applied_hook(
    data: usize,
    thread: ThreadId,
    event: ThreadPolicyApplied,
) {
    let runtime = unsafe { runtime_thread_data_from_raw(data) };
    if let Some(extension) = runtime.os_extension.as_ref() {
        // SAFETY: the outer runtime extension retains and serially forwards
        // the value-only owner commit to the installed OS extension.
        unsafe {
            (extension.ops().on_policy_applied)(extension.data(), thread, event);
        }
    }
}

unsafe extern "Rust" fn runtime_thread_exit_hook(data: usize, thread: ThreadId) {
    let runtime = unsafe { runtime_thread_data_from_raw(data) };
    if let Some(extension) = runtime.os_extension.as_ref() {
        // SAFETY: the TaskSystem invokes this in task context after committing exit.
        unsafe { (extension.ops().on_exit)(extension.data(), thread) };
    }
    // Runtime threads normally publish completion before their final schedule,
    // Linux-zombie style. Keep this idempotent fallback for externally marked
    // exits and failed-spawn cleanup paths that never ran the trampoline.
    publish_runtime_exit_completion(runtime);
}

fn publish_runtime_exit_completion(runtime: &RuntimeThreadData) {
    if runtime
        .exit_completed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        runtime.join_wait.notify_all();
    }
}

unsafe extern "Rust" fn runtime_thread_deadline_overrun_hook(data: usize, thread: ThreadId) {
    let runtime = unsafe { runtime_thread_data_from_raw(data) };
    if let Some(extension) = runtime.os_extension.as_ref() {
        // SAFETY: the scheduler defers this callback to an ordinary safe point.
        unsafe { (extension.ops().on_deadline_overrun)(extension.data(), thread) };
    }
}

unsafe extern "Rust" fn runtime_thread_drop_hook(data: usize) {
    // SAFETY: the scheduler reaper invokes this exactly once for the pointer
    // transferred through `RUNTIME_THREAD_EXTENSION_OPS`.
    drop(unsafe { Box::from_raw(ptr::with_exposed_provenance_mut::<RuntimeThreadData>(data)) });
}

unsafe fn runtime_thread_data_from_raw(data: usize) -> &'static RuntimeThreadData {
    // SAFETY: every outer callback receives the Box pointer installed by
    // `spawn_raw_with_extension`, which remains valid until the drop callback.
    unsafe { &*ptr::with_exposed_provenance::<RuntimeThreadData>(data) }
}

/// Creates the global task system and the primary CPU-local scheduler object.
pub(crate) fn initialize_primary(cpu_id: usize) -> Result<(), TaskError> {
    let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(ax_hal::cpu_num()))?);
    TASK_SYSTEM.init_once(system);
    let bootstrap = initialize_current_cpu(cpu_id)?;
    PRIMARY_BOOTSTRAP_THREAD.init_once(PrimaryBootstrapThread(bootstrap));
    Ok(())
}

/// Installs temporary TLS before platform late-init can enter Rust code that
/// uses thread-local storage.
#[cfg(feature = "tls")]
pub(crate) fn initialize_early_bootstrap_tls() -> Result<(), TaskError> {
    let existing = unsafe { EARLY_BOOTSTRAP_TLS.read_current_raw() };
    assert_eq!(existing, 0, "bootstrap TLS initialized twice on one CPU");
    let result = allocate_runtime_tls(TlsRequest {
        template_start: 0,
        initialized_size: 0,
        total_size: 0,
        alignment: 1,
    });
    if result.status != RuntimeStatus::Success {
        return Err(runtime_status_error(result.status));
    }
    if result.handle == 0 {
        return Err(TaskError::InvalidRuntimeHandle);
    }
    // SAFETY: success returned a fresh, non-zero runtime TLS allocation.
    let early_tls = unsafe { TlsHandle::from_raw(result.handle) };
    unsafe {
        // SAFETY: this CPU exclusively initializes its per-CPU bootstrap slot.
        // Publishing the owner before the hardware pointer keeps an early
        // failure from losing the allocation's destruction right.
        EARLY_BOOTSTRAP_TLS.write_current_raw(result.handle);
        if ax_hal::percpu::install_bootstrap_kernel_tls(ax_hal::context::KernelTlsBase::new(
            runtime_tls_pointer(early_tls),
        ))
        .is_err()
        {
            EARLY_BOOTSTRAP_TLS.write_current_raw(0);
            assert_eq!(
                deallocate_runtime_tls(early_tls),
                RuntimeStatus::Success,
                "failed to roll back rejected bootstrap TLS"
            );
            return Err(TaskError::InvalidConfiguration);
        }
    }
    Ok(())
}

/// Creates and publishes the calling secondary CPU's local scheduler object.
#[cfg(feature = "smp")]
pub(crate) fn initialize_secondary(cpu_id: usize) -> Result<(), TaskError> {
    initialize_current_cpu(cpu_id).map(|_| ())
}

/// Publishes a prepared CPU after local timer and scheduler-IPI paths are ready.
pub(crate) fn publish_current_cpu_online() -> Result<(), TaskError> {
    let system = task_system().ok_or(TaskError::NotInitialized)?;
    let mut cpu = current_cpu_local_mut_for_boot().ok_or(TaskError::NotInitialized)?;
    system.bring_cpu_online(cpu.as_mut())
}

/// Starts the single ordinary-context worker for scheduler callbacks/reaping.
pub(crate) fn start_deferred_task_work_service() -> Result<(), TaskError> {
    ax_task::start_deferred_task_work_service()
}

/// Runs the owner CPU's scheduler/idle handshake forever.
pub(crate) fn run_idle() -> ! {
    let guard = IrqGuard::new();
    let (current, idle) = current_cpu_remote(guard.cpu_pin())
        .map(|cpu| (cpu.current_thread(), cpu.idle_thread()))
        .unwrap_or((None, None));
    drop(guard);
    let entry_action = idle_entry_action(current, idle)
        .unwrap_or_else(|error| panic!("idle loop entered without scheduler ownership: {error}"));
    if entry_action == IdleEntryAction::RetireBootstrap {
        match ax_task::exit_current_thread() {
            Err(error) => panic!("failed to retire secondary bootstrap thread: {error}"),
            Ok(()) => panic!("retired secondary bootstrap thread unexpectedly resumed"),
        }
    }
    loop {
        #[cfg(feature = "ipi")]
        {
            ax_ipi::service_callback_ipi_retries(64);
        }
        // A persistently busy callback-IPI transport must not keep the idle
        // owner away from its scheduler. Remote task wakes have their own
        // persistent doorbell and may already have made local work runnable.
        ax_task::schedule_current_cpu()
            .unwrap_or_else(|error| panic!("idle scheduler safe point failed: {error}"));
        ax_task::idle_current_cpu_once()
            .unwrap_or_else(|error| panic!("idle wait handshake failed: {error}"));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IdleEntryAction {
    RetireBootstrap,
    RunIdle,
}

fn idle_entry_action(
    current: Option<ThreadId>,
    idle: Option<ThreadId>,
) -> Result<IdleEntryAction, TaskError> {
    match (current, idle) {
        (Some(current), Some(idle)) if current == idle => Ok(IdleEntryAction::RunIdle),
        (Some(_), Some(_)) => Ok(IdleEntryAction::RetireBootstrap),
        _ => Err(TaskError::InvalidConfiguration),
    }
}

/// Stores the exit code, marks the current thread exited, and switches away.
pub fn exit_current(exit_code: i32) -> ! {
    let current = current_thread_id()
        .unwrap_or_else(|error| panic!("failed to identify exiting runtime thread: {error}"));
    let primary = PRIMARY_BOOTSTRAP_THREAD
        .get()
        .unwrap_or_else(|| panic!("primary bootstrap thread identity is not initialized"));
    if primary.owns(current) {
        debug!("main task exited: exit_code={exit_code}");
        let _irq = IrqGuard::new();
        ax_hal::power::system_off();
    }

    let exit_permit = ax_task::prepare_current_exit()
        .unwrap_or_else(|error| panic!("failed to prepare scheduler thread exit: {error}"));
    publish_current_runtime_exit(exit_code)
        .unwrap_or_else(|error| panic!("failed to publish thread exit: {error}"));
    ax_task::commit_current_exit(exit_permit)
}

/// Returns the aggregate number of scheduler timer interrupts since boot.
pub fn timer_irq_count() -> u64 {
    TASK_TIMER_IRQ_COUNT.load(Ordering::Relaxed)
}

/// Creates a scheduler-owned kernel thread and enqueues it on the current CPU.
pub fn spawn_raw<F>(entry: F, name: String, stack_size: usize) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    // SAFETY: `None` carries no external callback ownership.
    unsafe { spawn_raw_with_extension(entry, name, stack_size, None) }
}

/// Creates one shutdown-lifetime kernel worker with its CPU affinity and fair
/// policy installed before the scheduler publishes it as Ready.
///
/// This narrow runtime-only entry prevents per-CPU services from racing a
/// post-spawn affinity or policy update. The caller remains responsible for
/// retaining a direct wake capability and for making the entry non-returning.
#[cfg(feature = "workqueue")]
pub(crate) fn spawn_kernel_worker<F>(
    entry: F,
    name: String,
    affinity: CpuSet,
    policy: SchedulePolicy,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        // SAFETY: no external extension or address-space capability is
        // transferred. Affinity and policy are embedded in ThreadSpec before
        // the new kernel context can enter a run queue.
        spawn_raw_with_options(
            entry,
            name,
            runtime_task_stack_size(),
            None,
            Some(affinity),
            policy,
            InitialContextState::kernel(),
        )
    }
}

/// Creates a kernel thread while retaining one OS-specific extension.
///
/// The runtime owns an outer extension for the closure and join metadata. It
/// forwards switch, exit, Deadline-overrun and final-drop callbacks to
/// `os_extension`, preserving the inner callback-table address as its type
/// identity for StarryOS or another consuming OS.
///
/// # Safety
///
/// When present, `os_extension` transfers its unique callback-data ownership
/// to this function. The caller must not install another copy or invoke its
/// drop callback, regardless of whether thread creation succeeds.
pub unsafe fn spawn_raw_with_extension<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    // SAFETY: this function forwards the extension's unique ownership without
    // creating another copy or invoking its callback table.
    unsafe { spawn_raw_with_extension_and_affinity(entry, name, stack_size, os_extension, None) }
}

/// Creates a kernel thread with an OS extension and pre-publication affinity.
///
/// Unlike setting affinity on the returned handle, `affinity` is installed in
/// [`ThreadSpec`] before the thread becomes Ready or enters a run queue. This is
/// required by pinned vCPU and per-CPU service threads whose entry point must
/// never execute on a disallowed CPU.
///
/// # Safety
///
/// When present, `os_extension` transfers its unique callback-data ownership
/// to this function. The caller must not install another copy or invoke its
/// drop callback, regardless of whether thread creation succeeds.
pub unsafe fn spawn_raw_with_extension_and_affinity<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    affinity: Option<CpuSet>,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        // SAFETY: this wrapper forwards unique extension ownership once.
        spawn_raw_with_options(
            entry,
            name,
            stack_size,
            os_extension,
            affinity,
            SchedulePolicy::default(),
            InitialContextState::kernel(),
        )
    }
}

/// Creates a scheduler thread whose architecture context retains a user page table.
///
/// # Safety
///
/// `os_extension` transfers unique callback-data ownership. `address_space`
/// must describe the address space retained by the OS extension for the entire
/// thread lifetime.
pub unsafe fn spawn_raw_with_extension_in_address_space<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    address_space: TaskAddressSpace,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        // SAFETY: this wrapper forwards both capabilities without copying the
        // extension or exposing its architecture context.
        spawn_raw_with_options(
            entry,
            name,
            stack_size,
            os_extension,
            None,
            SchedulePolicy::default(),
            InitialContextState::user(address_space),
        )
    }
}

/// Creates a user thread with its policy installed before run-queue publication.
///
/// # Safety
///
/// The extension and address-space ownership rules are identical to
/// [`spawn_raw_with_extension_in_address_space`].
pub unsafe fn spawn_raw_with_extension_in_address_space_and_policy<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    address_space: TaskAddressSpace,
    policy: SchedulePolicy,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        // SAFETY: ownership is forwarded once and the validated policy is
        // embedded in ThreadSpec before scheduler publication.
        spawn_raw_with_options(
            entry,
            name,
            stack_size,
            os_extension,
            None,
            policy,
            InitialContextState::user(address_space),
        )
    }
}

/// Creates a RISC-V user thread while preserving the inherited FP context.
///
/// # Safety
///
/// The extension and address-space contracts are identical to
/// [`spawn_raw_with_extension_in_address_space`].
#[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
pub unsafe fn spawn_raw_with_extension_in_address_space_and_fp_state<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    address_space: TaskAddressSpace,
    fp_state: ax_hal::cpu::FpState,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        // SAFETY: the newly owned FP snapshot is installed before publication;
        // extension ownership is forwarded exactly once.
        spawn_raw_with_options(
            entry,
            name,
            stack_size,
            os_extension,
            None,
            SchedulePolicy::default(),
            InitialContextState {
                address_space: address_space.0,
                fp_state: Some(fp_state),
            },
        )
    }
}

/// Creates a RISC-V user thread with inherited FP state and scheduling policy.
///
/// # Safety
///
/// The ownership rules are identical to
/// [`spawn_raw_with_extension_in_address_space_and_fp_state`].
#[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
pub unsafe fn spawn_raw_with_extension_in_address_space_and_fp_state_and_policy<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    address_space: TaskAddressSpace,
    fp_state: ax_hal::cpu::FpState,
    policy: SchedulePolicy,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        // SAFETY: all owned capabilities are installed before publication and
        // each is transferred exactly once.
        spawn_raw_with_options(
            entry,
            name,
            stack_size,
            os_extension,
            None,
            policy,
            InitialContextState {
                address_space: address_space.0,
                fp_state: Some(fp_state),
            },
        )
    }
}

unsafe fn spawn_raw_with_options<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    affinity: Option<CpuSet>,
    policy: SchedulePolicy,
    context_state: InitialContextState,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    if stack_size == 0 {
        // SAFETY: this function accepted the extension's unique ownership on entry.
        unsafe { release_transferred_extension(os_extension) };
        return Err(TaskError::InvalidConfiguration);
    }
    let Some(system) = task_system() else {
        // SAFETY: no runtime object observed or retained the extension.
        unsafe { release_transferred_extension(os_extension) };
        return Err(TaskError::NotInitialized);
    };
    let resources = match create_thread_resources(stack_size, runtime_thread_entry, context_state) {
        Ok(resources) => resources,
        Err(error) => {
            // SAFETY: resource construction failed before publishing extension data.
            unsafe { release_transferred_extension(os_extension) };
            return Err(error);
        }
    };
    let data = Box::into_raw(Box::new(RuntimeThreadData::new(
        Box::new(entry),
        name,
        os_extension,
    )))
    .expose_provenance();
    // SAFETY: the boxed data remains live until the scheduler reaper invokes
    // `runtime_thread_drop_hook` through this exact ops table.
    let extension = unsafe { ThreadExtension::new(data, &RUNTIME_THREAD_EXTENSION_OPS) };
    let mut spec = unsafe {
        // SAFETY: create_thread_resources returned one live bundle created by
        // this runtime, and this specification is its unique installation.
        ThreadSpec::new(policy)
            .with_extension(extension)
            .with_resources(resources)
    };
    if let Some(affinity) = affinity {
        spec = spec.with_affinity(affinity);
    }
    let handle = system.create_thread(spec)?;

    let mut cpu = match current_cpu_local_mut_owner() {
        Ok(cpu) => cpu,
        Err(error) => {
            cleanup_failed_thread(system, handle);
            return Err(error);
        }
    };
    let result = system.make_ready(handle.id()).and_then(|()| {
        system.place_ready(
            cpu.as_pin_mut(),
            handle.id(),
            ax_hal::time::monotonic_time_nanos(),
        )
    });
    drop(cpu);
    if let Err(error) = result {
        cleanup_failed_thread(system, handle);
        return Err(error);
    }
    Ok(handle)
}

unsafe fn release_transferred_extension(extension: Option<ThreadExtension>) {
    drop(extension);
}

/// Borrows the OS extension composed inside a runtime-owned thread record.
pub fn thread_os_extension(
    thread: &ThreadHandle,
) -> Result<Option<ThreadOsExtensionBorrow<'_>>, TaskError> {
    let runtime = task_system()
        .ok_or(TaskError::NotInitialized)?
        .thread_extension(thread)?;
    let RuntimeExtensionKind::Runtime = classify_runtime_extension(
        runtime.as_ref().map(|extension| extension.ops()),
        runtime.as_ref().map_or(0, |extension| extension.data()),
    )?
    else {
        return Ok(None);
    };
    let Some(runtime) = runtime else {
        unreachable!("classified runtime extension must be present")
    };
    // SAFETY: the checked ops identity belongs exclusively to RuntimeThreadData,
    // and `runtime` borrows the strong caller handle for the whole result.
    let data = unsafe { runtime_thread_data_from_raw(runtime.data()) };
    Ok(data
        .os_extension
        .as_ref()
        .map(|extension| ThreadOsExtensionBorrow {
            data: extension.data(),
            ops: extension.ops(),
            _runtime: runtime,
        }))
}

/// Leases the current thread's composed OS extension.
pub fn current_os_extension() -> Result<Option<ThreadOsExtensionLease>, TaskError> {
    let runtime = current_thread_extension()?;
    let RuntimeExtensionKind::Runtime = classify_runtime_extension(
        runtime.as_ref().map(|extension| extension.ops()),
        runtime.as_ref().map_or(0, |extension| extension.data()),
    )?
    else {
        return Ok(None);
    };
    let Some(runtime) = runtime else {
        unreachable!("classified runtime extension must be present")
    };
    // SAFETY: the checked ops identity belongs exclusively to RuntimeThreadData,
    // and the returned lease retains the outer scheduler extension lease.
    let data = unsafe { runtime_thread_data_from_raw(runtime.data()) };
    Ok(data
        .os_extension
        .as_ref()
        .map(|extension| ThreadOsExtensionLease {
            data: extension.data(),
            ops: extension.ops(),
            _runtime: runtime,
        }))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeExtensionKind {
    Missing,
    Runtime,
}

fn classify_runtime_extension(
    ops: Option<&ThreadExtensionOps>,
    data: usize,
) -> Result<RuntimeExtensionKind, TaskError> {
    let Some(ops) = ops else {
        return Ok(RuntimeExtensionKind::Missing);
    };
    if !core::ptr::eq(ops, &RUNTIME_THREAD_EXTENSION_OPS) {
        return Err(TaskError::InvalidConfiguration);
    }
    if data == 0 || !data.is_multiple_of(core::mem::align_of::<RuntimeThreadData>()) {
        return Err(TaskError::InvalidRuntimeHandle);
    }
    Ok(RuntimeExtensionKind::Runtime)
}

/// Waits for a thread to finish executing without consuming its owning handle.
///
/// This split wait operation lets handle registries keep their raw-pointer or
/// map entry valid while the target still runs. Completion is published by the
/// exiting thread after its entry function and exit code are final, before the
/// non-returning scheduler exit. Physical off-CPU completion and final resource
/// reclamation are separate phases.
pub fn wait_thread(handle: &ThreadHandle) -> Result<i32, TaskError> {
    if current_thread_id()? == handle.id() {
        return Err(TaskError::InvalidConfiguration);
    }
    let data = runtime_thread_data(handle)?;
    data.join_wait
        .try_wait_until(|| data.exit_completed.load(Ordering::Acquire))?;
    Ok(data.exit_code.load(Ordering::Acquire))
}

/// Waits for an exited thread and returns its exit code.
///
/// Resource teardown is attempted synchronously once. A late IRQ wake or other
/// stable header reference may legitimately defer final reclamation, so join
/// releases its owning handle to the bounded task-system reaper instead of
/// spinning until unrelated references disappear.
pub fn join_thread(handle: ThreadHandle) -> Result<i32, TaskError> {
    let exit_code = wait_thread(&handle)?;
    match task_system()
        .ok_or(TaskError::NotInitialized)?
        .reap_thread_handle(handle)
    {
        Ok(()) => {}
        Err(error) => {
            let task_error = error.task_error();
            if !matches!(task_error, TaskError::ThreadBusy | TaskError::NotExited) {
                return Err(task_error);
            }
            drop(
                error
                    .into_retry_handle()
                    .expect("busy owned reap must return its handle"),
            );
        }
    }
    Ok(exit_code)
}

/// Returns the earliest task deadline known to the current CPU.
#[cfg(feature = "irq")]
pub(crate) fn next_timer_deadline_nanos() -> Option<u64> {
    // SAFETY: only the current CPU's IRQ and scheduler paths access this slot.
    let deadline = unsafe { NEXT_TASK_TIMER_DEADLINE_NS.read_current_raw() };
    (deadline != 0).then_some(deadline)
}

/// Publishes typed timer work from hard IRQ.
///
/// Periodic housekeeping, timer expiry, and scheduler-deadline continuation
/// remain owner work. Only slice, RT-quota, or CBS exhaustion may publish the
/// scheduler's distinct preemption reason.
#[cfg(feature = "irq")]
pub(crate) fn on_timer_irq(scheduler_tick: bool) {
    TASK_TIMER_IRQ_COUNT.fetch_add(1, Ordering::Relaxed);
    match ax_task::timer_interrupt_current_cpu(scheduler_tick, 0) {
        Ok(outcome) => {
            // SAFETY: only the current CPU publishes its next task deadline.
            unsafe {
                NEXT_TASK_TIMER_DEADLINE_NS
                    .write_current_raw(outcome.next_deadline_ns().unwrap_or(0))
            };
        }
        Err(TaskError::NotInitialized | TaskError::CpuOffline(_)) => {}
        Err(error) => panic!("task timer accounting failed: {error}"),
    }
}

/// Acknowledges the scheduler transport epoch carried by the shared IPI vector.
///
/// The producer publishes the scheduling reason before ringing the doorbell.
/// Consequently any IPI arrival is sufficient to acknowledge a claimed
/// transport epoch, including a callback IPI that coalesces with scheduler
/// work. The interrupt itself must not manufacture a preemption reason.
#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
pub(crate) fn on_scheduler_ipi() {
    // SAFETY: scheduler IPI handling is a hard-IRQ scope and therefore cannot
    // migrate during the complete CPU-ID/endpoint lookup.
    if let Some(cpu) = unsafe { current_cpu_remote_unchecked() }.filter(|cpu| cpu.is_online()) {
        cpu.acknowledge_scheduler_ipi();
    }
}

#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
fn scheduler_ipi_target(
    current: ax_hal::irq::CpuId,
    destination: ax_hal::irq::CpuId,
) -> ax_hal::irq::CpuIpiTarget {
    if current == destination {
        ax_hal::irq::CpuIpiTarget::Current { cpu: destination }
    } else {
        ax_hal::irq::CpuIpiTarget::Other { cpu: destination }
    }
}

fn initialize_current_cpu(cpu_id: usize) -> Result<ThreadId, TaskError> {
    let system = task_system().ok_or(TaskError::NotInitialized)?;
    let cpu_id = u32::try_from(cpu_id).map_err(|_| TaskError::InvalidCpu(u32::MAX))?;
    let owner = CpuId::new(cpu_id);
    #[cfg(feature = "uspace")]
    {
        let kernel_root = if cfg!(any(target_arch = "x86_64", target_arch = "riscv64")) {
            ax_hal::asm::read_kernel_page_table().as_usize()
        } else {
            0
        };
        // SAFETY: per-CPU storage is initialized and this owner CPU has not
        // entered any scheduler-managed user address space yet.
        unsafe { KERNEL_ADDRESS_SPACE_ROOT.write_current_raw(kernel_root) };
    }
    let mut cpu = system.create_cpu_local(owner)?;
    // Bootstrap and idle contexts use this CPU's architecture-owned boot
    // stack/context. Migrating either record would resume a CPU on another
    // CPU's boot resources and break the bring-up continuation.
    let mut owner_affinity = CpuSet::empty(ax_hal::cpu_num());
    if !owner_affinity.insert(owner) {
        return Err(TaskError::InvalidCpu(cpu_id));
    }
    let bootstrap_resources = create_bootstrap_resources()?;
    let bootstrap_context = bootstrap_resources.context();
    #[cfg(feature = "tls")]
    let bootstrap_tls = bootstrap_resources.tls();
    let bootstrap = system.install_bootstrap_thread(cpu.as_mut(), unsafe {
        // SAFETY: bootstrap_resources is a fresh unique runtime bundle.
        ThreadSpec::new(SchedulePolicy::default())
            .with_affinity(owner_affinity.clone())
            .with_resources(bootstrap_resources)
    })?;
    let bootstrap_thread = bootstrap.id();
    drop(bootstrap);
    #[cfg(feature = "tls")]
    let bootstrap_kernel_tls = runtime_tls_pointer(bootstrap_tls);
    #[cfg(not(feature = "tls"))]
    let bootstrap_kernel_tls = 0;
    // SAFETY: platform entry bound the immutable CPU area, local IRQs remain
    // disabled, and this CPU is not scheduler-visible yet.
    let cpu_pin = unsafe { CpuPin::new_unchecked() };
    // Publish the physical bootstrap resources only after their scheduler
    // record owns them. A failed installation must not leave this CPU using a
    // context or TLS allocation that no scheduler record can release.
    bind_bootstrap_runtime_context(&cpu_pin, bootstrap_context, bootstrap_kernel_tls)
        .unwrap_or_else(|error| panic!("failed to publish bootstrap runtime context: {error}"));
    #[cfg(feature = "tls")]
    unsafe {
        let early_tls = TlsHandle::from_raw(EARLY_BOOTSTRAP_TLS.read_current_raw());
        assert!(
            !early_tls.is_none(),
            "scheduler bootstrap requires early TLS ownership"
        );
        EARLY_BOOTSTRAP_TLS.write_current_raw(0);
        assert_eq!(
            deallocate_runtime_tls(early_tls),
            RuntimeStatus::Success,
            "failed to release early bootstrap TLS"
        );
    }
    let idle_resources = create_idle_resources();
    system.register_idle_thread(cpu.as_mut(), unsafe {
        // SAFETY: create_idle_resources returned a fresh unique bundle.
        ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle))
            .with_affinity(owner_affinity)
            .with_resources(idle_resources)
    })?;
    // SAFETY: platform entry installed the CPU area and this owner has not yet
    // published its scheduler object online.
    let owner_handle =
        (unsafe { Pin::get_unchecked_mut(cpu.as_mut()) } as *mut CpuLocal).expose_provenance();
    let slot = unsafe { current_cpu_slot_for_boot() };
    slot.init_once(cpu);
    // SAFETY: this CPU exclusively initializes its per-CPU runtime state and
    // remains offline until the pinned owner capability has been published.
    unsafe { CPU_LOCAL_OWNER_HANDLE.write_current_raw(owner_handle) };
    #[cfg(feature = "irq")]
    {
        // SAFETY: no task timer is armed during bootstrap object creation.
        unsafe { NEXT_TASK_TIMER_DEADLINE_NS.write_current_raw(0) };
    }
    crate::guard::assert_boot_guards_released();
    Ok(bootstrap_thread)
}

fn create_idle_resources() -> ThreadResources {
    let guard_size = if cfg!(feature = "stack-guard-page") {
        PAGE_SIZE
    } else {
        0
    };
    let stack = allocate_runtime_stack(StackRequest {
        usable_size: runtime_task_stack_size(),
        alignment: 16,
        guard_size,
    })
    .unwrap_or_else(|status| panic!("failed to allocate idle stack: {status:?}"));
    let tls = allocate_runtime_tls(TlsRequest {
        template_start: 0,
        initialized_size: 0,
        total_size: 0,
        alignment: 1,
    });
    let tls = if tls.status == RuntimeStatus::Success {
        assert_ne!(
            tls.handle, 0,
            "successful idle TLS allocation returned NONE"
        );
        // SAFETY: allocate_runtime_tls returned a fresh, non-zero allocation
        // whose ownership moves into the idle thread resources below.
        unsafe { TlsHandle::from_raw(tls.handle) }
    } else if tls.status == RuntimeStatus::Unsupported {
        TlsHandle::NONE
    } else {
        let _ = deallocate_runtime_stack(stack);
        panic!("failed to allocate idle TLS: {:?}", tls.status);
    };
    let context = create_runtime_context(KernelContextRequest {
        stack,
        entry: idle_context_entry,
        tls,
        address_space: AddressSpaceHandle::NONE,
    });
    if context.status != RuntimeStatus::Success {
        let _ = deallocate_runtime_tls(tls);
        let _ = deallocate_runtime_stack(stack);
        panic!("failed to create idle context: {:?}", context.status);
    }
    unsafe {
        // SAFETY: the three fresh handles were created by this runtime and are
        // uniquely transferred into the idle record's resource bundle.
        ThreadResources::new(
            ExecutionContextHandle::from_raw(context.handle),
            stack,
            tls,
            AddressSpaceHandle::NONE,
        )
    }
}

fn create_bootstrap_resources() -> Result<ThreadResources, TaskError> {
    let tls_result = allocate_runtime_tls(TlsRequest {
        template_start: 0,
        initialized_size: 0,
        total_size: 0,
        alignment: 1,
    });
    let tls = match (tls_result.status, tls_result.handle) {
        (RuntimeStatus::Success, 0) => return Err(TaskError::InvalidRuntimeHandle),
        (RuntimeStatus::Success, handle) => {
            // SAFETY: the runtime returned a fresh, non-zero TLS allocation
            // whose unique ownership is transferred into bootstrap resources.
            unsafe { TlsHandle::from_raw(handle) }
        }
        (RuntimeStatus::Unsupported, _) => TlsHandle::NONE,
        (status, _) => return Err(runtime_status_error(status)),
    };
    let context = create_bootstrap_context();
    match assemble_bootstrap_resources(context, tls) {
        Ok(resources) => Ok(resources),
        Err(error) => {
            let _ = destroy_runtime_context(context);
            let _ = deallocate_runtime_tls(tls);
            Err(error)
        }
    }
}

fn assemble_bootstrap_resources(
    context: ExecutionContextHandle,
    tls: TlsHandle,
) -> Result<ThreadResources, TaskError> {
    if context.is_none() {
        return Err(TaskError::InvalidRuntimeHandle);
    }
    #[cfg(feature = "tls")]
    if tls.is_none() {
        return Err(TaskError::InvalidRuntimeHandle);
    }
    Ok(unsafe {
        // SAFETY: the caller transfers the fresh bootstrap context and TLS
        // handles exactly once. Its architecture boot stack is externally
        // owned, so this resource bundle intentionally has no stack handle.
        ThreadResources::new(context, StackHandle::NONE, tls, AddressSpaceHandle::NONE)
    })
}

unsafe extern "C" fn idle_context_entry() -> ! {
    finish_initial_scheduler_switch();
    run_idle()
}

unsafe extern "C" fn runtime_thread_entry() -> ! {
    finish_initial_scheduler_switch();
    let extension = ax_task::current_thread_extension()
        .unwrap_or_else(|error| panic!("kernel thread has no scheduler extension: {error}"))
        .unwrap_or_else(|| panic!("kernel thread entry is missing runtime data"));
    let data_raw = extension_data_after_releasing_lease(extension, &RUNTIME_THREAD_EXTENSION_OPS)
        .unwrap_or_else(|error| panic!("kernel thread extension type is invalid: {error}"));
    // SAFETY: the ops identity above proves the data pointer was created from
    // `Box<RuntimeThreadData>`. The registry record keeps it live through exit;
    // the temporary lease must not survive the non-unwinding exit path.
    let data = unsafe { &*ptr::with_exposed_provenance::<RuntimeThreadData>(data_raw) };
    let entry = data
        .entry
        .lock()
        .take()
        .unwrap_or_else(|| panic!("kernel thread entry was already consumed"));
    entry();
    exit_current(0)
}

fn extension_data_after_releasing_lease(
    extension: ax_task::ThreadExtensionLease,
    expected_ops: &'static ThreadExtensionOps,
) -> Result<usize, TaskError> {
    if !core::ptr::eq(extension.ops(), expected_ops) {
        return Err(TaskError::InvalidConfiguration);
    }
    let extension = unsafe {
        // SAFETY: the runtime calls this only from the leased running thread's
        // entry trampoline, and its registry record remains live through exit.
        extension.release_for_current_thread_entry()
    };
    Ok(extension.data())
}

fn finish_initial_scheduler_switch() {
    // SAFETY: both architecture entry trampolines invoke this exactly once as
    // their first operation after inheriting the scheduler IRQ-guard baton.
    unsafe { ax_task::finish_initial_context_switch() }
        .unwrap_or_else(|error| panic!("failed to complete initial context switch: {error}"));
}

fn create_thread_resources(
    stack_size: usize,
    entry: ax_task::runtime::KernelEntry,
    context_state: InitialContextState,
) -> Result<ThreadResources, TaskError> {
    let guard_size = if cfg!(feature = "stack-guard-page") {
        PAGE_SIZE
    } else {
        0
    };
    let stack = allocate_runtime_stack(StackRequest {
        usable_size: stack_size,
        alignment: 16,
        guard_size,
    })
    .map_err(runtime_status_error)?;
    let tls_result = allocate_runtime_tls(TlsRequest {
        template_start: 0,
        initialized_size: 0,
        total_size: 0,
        alignment: 1,
    });
    let tls = match (tls_result.status, tls_result.handle) {
        (RuntimeStatus::Success, 0) => {
            let _ = deallocate_runtime_stack(stack);
            return Err(TaskError::InvalidRuntimeHandle);
        }
        (RuntimeStatus::Success, handle) => {
            // SAFETY: the runtime returned a fresh, non-zero TLS allocation
            // whose unique ownership moves into this thread's resources.
            unsafe { TlsHandle::from_raw(handle) }
        }
        (RuntimeStatus::Unsupported, _) => TlsHandle::NONE,
        (status, _) => {
            let _ = deallocate_runtime_stack(stack);
            return Err(runtime_status_error(status));
        }
    };
    let context_result = if context_state.address_space.is_none() {
        create_runtime_context(KernelContextRequest {
            stack,
            entry,
            tls,
            address_space: AddressSpaceHandle::NONE,
        })
    } else {
        create_user_runtime_context(UserContextRequest {
            stack,
            entry,
            tls,
            address_space: context_state.address_space,
        })
    };
    if context_result.status != RuntimeStatus::Success {
        let _ = deallocate_runtime_tls(tls);
        let _ = deallocate_runtime_stack(stack);
        return Err(runtime_status_error(context_result.status));
    }
    #[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
    if let Some(fp_state) = context_state.fp_state {
        let context = ptr::with_exposed_provenance_mut::<RuntimeContext>(context_result.handle);
        // SAFETY: the context allocation was just created above and has not
        // been published to the scheduler, so its FP snapshot is exclusively
        // owned by this construction path.
        unsafe { (*(*context).inner.get()).fp_state = fp_state };
    }
    Ok(unsafe {
        // SAFETY: the active runtime created each live handle above and this is
        // the only owning bundle constructed from those scalar identities.
        ThreadResources::new(
            ExecutionContextHandle::from_raw(context_result.handle),
            stack,
            tls,
            context_state.address_space,
        )
    })
}

fn runtime_thread_data(thread: &ThreadHandle) -> Result<&RuntimeThreadData, TaskError> {
    let extension = task_system()
        .ok_or(TaskError::NotInitialized)?
        .thread_extension(thread)?
        .ok_or(TaskError::InvalidConfiguration)?;
    if !core::ptr::eq(extension.ops(), &RUNTIME_THREAD_EXTENSION_OPS) {
        return Err(TaskError::InvalidConfiguration);
    }
    // SAFETY: the checked ops identity belongs exclusively to RuntimeThreadData,
    // and the returned reference is bounded by the strong caller handle.
    Ok(unsafe { &*ptr::with_exposed_provenance::<RuntimeThreadData>(extension.data()) })
}

fn publish_current_runtime_exit(exit_code: i32) -> Result<(), TaskError> {
    let thread = current_thread_handle()?;
    let data = runtime_thread_data(&thread)?;
    data.exit_code.store(exit_code, Ordering::Release);
    publish_runtime_exit_completion(data);
    Ok(())
}

fn cleanup_failed_thread(system: &TaskSystem, handle: ThreadHandle) {
    let thread = handle.id();
    let _ = system.mark_exited(thread);
    drop(handle);
    let _ = system.reap_thread(thread);
}

const fn runtime_status_error(status: RuntimeStatus) -> TaskError {
    TaskError::RuntimeFailure(status as u32)
}

const fn runtime_task_stack_size() -> usize {
    #[cfg(feature = "fs")]
    {
        crate::build_info::TASK_STACK_SIZE
    }
    #[cfg(not(feature = "fs"))]
    {
        DEFAULT_TASK_STACK_SIZE
    }
}

fn task_system() -> Option<&'static TaskSystem> {
    TASK_SYSTEM.get().map(|system| system.as_ref().get_ref())
}

fn current_cpu_local_mut_for_boot() -> Option<Pin<&'static mut CpuLocal>> {
    // SAFETY: this is called exactly once by the owner CPU before it is
    // published online. PerCpuData stores its value in UnsafeCell, and no
    // scheduler or remote wake can hold an aliasing CPU-local reference yet.
    let slot = unsafe { CPU_LOCAL.current_ref_mut_raw() };
    slot.get_mut().map(Pin::as_mut)
}

/// Returns the current CPU's unpublished scheduler slot during bring-up.
///
/// # Safety
///
/// The architecture CPU-area anchor must be installed, and the calling CPU
/// must not yet be online or reachable by scheduler/remote-wake paths.
unsafe fn current_cpu_slot_for_boot() -> &'static LazyInit<Pin<Box<CpuLocal>>> {
    // SAFETY: forwarded caller contract covers current-area validity and the
    // shutdown lifetime of the per-CPU allocation.
    unsafe { CPU_LOCAL.current_ref_raw() }
}

struct RuntimeCpuOwnerBorrow {
    cpu: CpuLocalOwnerBorrow<'static>,
    _guard: IrqGuard,
}

impl RuntimeCpuOwnerBorrow {
    fn as_pin_mut(&mut self) -> Pin<&mut CpuLocal> {
        self.cpu.as_pin_mut()
    }
}

fn current_cpu_local_mut_owner() -> Result<RuntimeCpuOwnerBorrow, TaskError> {
    let guard = IrqGuard::new();
    let remote = current_cpu_remote(guard.cpu_pin()).ok_or(TaskError::NotInitialized)?;
    // SAFETY: the guard pins this lookup to one CPU and the owner address was
    // published from the unique pinned allocation before that CPU came online.
    let raw = unsafe { CPU_LOCAL_OWNER_HANDLE.read_current_raw() };
    if raw == 0 {
        return Err(TaskError::NotInitialized);
    }
    // SAFETY: publication pairs this raw owner capability with `remote`; the
    // separately allocated remote gate rejects every overlapping owner borrow.
    // The claim is stored before the guard so Rust field-drop order releases
    // owner access before IRQ migration protection is removed.
    let cpu = unsafe { remote.claim_local(ptr::with_exposed_provenance_mut::<CpuLocal>(raw))? };
    Ok(RuntimeCpuOwnerBorrow { cpu, _guard: guard })
}

pub(crate) fn current_cpu_remote(cpu_pin: &CpuPin) -> Option<&'static CpuRemote> {
    let cpu = u32::try_from(ax_hal::percpu::this_cpu_id_pinned(cpu_pin)).ok()?;
    task_system()?.cpu_remote(CpuId::new(cpu))
}

/// Returns the current CPU endpoint when migration is excluded externally.
///
/// # Safety
///
/// A valid CPU-local binding must be installed, and the caller must guarantee
/// that execution cannot migrate during this complete lookup. This is intended
/// only for hard-IRQ/trap paths that cannot hold an ordinary guard token.
#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
unsafe fn current_cpu_remote_unchecked() -> Option<&'static CpuRemote> {
    // SAFETY: the caller's no-migration guarantee covers the returned token's
    // complete use inside `current_cpu_remote`.
    let cpu_pin = unsafe { CpuPin::new_unchecked() };
    current_cpu_remote(&cpu_pin)
}

fn cpu_remote(cpu: RuntimeCpuId) -> Option<&'static CpuRemote> {
    task_system()?.cpu_remote(CpuId::new(cpu.as_u32()))
}

fn allocate_runtime_stack(request: StackRequest) -> Result<StackHandle, RuntimeStatus> {
    if request.usable_size == 0 || request.alignment == 0 || !request.alignment.is_power_of_two() {
        return Err(RuntimeStatus::InvalidArgument);
    }

    if request.guard_size == 0 {
        return allocate_heap_stack(request);
    }

    #[cfg(feature = "paging")]
    {
        allocate_guarded_stack(request)
    }
    #[cfg(not(feature = "paging"))]
    {
        Err(RuntimeStatus::Unsupported)
    }
}

fn allocate_heap_stack(request: StackRequest) -> Result<StackHandle, RuntimeStatus> {
    let layout = Layout::from_size_align(request.usable_size, request.alignment)
        .map_err(|_| RuntimeStatus::InvalidArgument)?;
    let pointer = ax_alloc::global_allocator()
        .alloc(layout)
        .map_err(map_alloc_status)?;
    let base = pointer.as_ptr() as usize;
    let usable_top = base
        .checked_add(request.usable_size)
        .ok_or(RuntimeStatus::InvalidArgument)?;
    let stack = Box::new(RuntimeStack {
        #[cfg(feature = "paging")]
        base,
        usable_top,
        backing: StackBacking::Heap { pointer, layout },
    });
    // SAFETY: Box::into_raw yields a non-null uniquely owned RuntimeStack that
    // stays live until deallocate_runtime_stack consumes this exact handle.
    Ok(unsafe { StackHandle::from_raw(Box::into_raw(stack).expose_provenance()) })
}

#[cfg(feature = "paging")]
fn allocate_guarded_stack(request: StackRequest) -> Result<StackHandle, RuntimeStatus> {
    if !request.guard_size.is_multiple_of(PAGE_SIZE) {
        return Err(RuntimeStatus::InvalidArgument);
    }
    let usable_size = request
        .usable_size
        .checked_add(PAGE_SIZE - 1)
        .ok_or(RuntimeStatus::InvalidArgument)?
        / PAGE_SIZE
        * PAGE_SIZE;
    let total_size = request
        .guard_size
        .checked_add(usable_size)
        .ok_or(RuntimeStatus::InvalidArgument)?;
    let pages = total_size / PAGE_SIZE;
    let base = ax_alloc::global_allocator()
        .alloc_pages(
            pages,
            request.alignment.max(PAGE_SIZE),
            ax_alloc::UsageKind::Global,
        )
        .map_err(map_alloc_status)?;
    let guard = ax_memory_addr::VirtAddr::from(base);
    if ax_mm::kernel_aspace()
        .lock()
        .protect(
            guard,
            request.guard_size,
            ax_hal::paging::MappingFlags::empty(),
        )
        .is_err()
    {
        ax_alloc::global_allocator().dealloc_pages(base, pages, ax_alloc::UsageKind::Global);
        return Err(RuntimeStatus::Platform);
    }
    ax_hal::asm::flush_tlb(None);
    let stack = Box::new(RuntimeStack {
        base,
        usable_top: base + total_size,
        backing: StackBacking::GuardedPages {
            pages,
            guard_size: request.guard_size,
        },
    });
    // SAFETY: Box::into_raw yields a non-null uniquely owned RuntimeStack that
    // stays live until deallocate_runtime_stack consumes this exact handle.
    Ok(unsafe { StackHandle::from_raw(Box::into_raw(stack).expose_provenance()) })
}

fn deallocate_runtime_stack(handle: StackHandle) -> RuntimeStatus {
    if handle.is_none() {
        return RuntimeStatus::InvalidHandle;
    }
    // SAFETY: ax-task passes only a live handle returned by
    // `allocate_runtime_stack`, and consumes it exactly once during reaping.
    let stack = unsafe {
        Box::from_raw(ptr::with_exposed_provenance_mut::<RuntimeStack>(
            handle.into_raw(),
        ))
    };
    match stack.backing {
        StackBacking::Heap { pointer, layout } => {
            ax_alloc::global_allocator().dealloc(pointer, layout);
        }
        #[cfg(feature = "paging")]
        StackBacking::GuardedPages { pages, guard_size } => {
            let guard = ax_memory_addr::VirtAddr::from(stack.base);
            let restore = ax_hal::paging::MappingFlags::READ | ax_hal::paging::MappingFlags::WRITE;
            if ax_mm::kernel_aspace()
                .lock()
                .protect(guard, guard_size, restore)
                .is_err()
            {
                core::mem::forget(stack);
                return RuntimeStatus::Platform;
            }
            ax_hal::asm::flush_tlb(None);
            ax_alloc::global_allocator().dealloc_pages(
                stack.base,
                pages,
                ax_alloc::UsageKind::Global,
            );
        }
    }
    RuntimeStatus::Success
}

fn allocate_runtime_tls(_request: TlsRequest) -> RuntimeHandleResult {
    #[cfg(feature = "tls")]
    {
        let tls = Box::new(RuntimeTls {
            area: ax_hal::tls::TlsArea::alloc(),
        });
        RuntimeHandleResult::success(Box::into_raw(tls).expose_provenance())
    }
    #[cfg(not(feature = "tls"))]
    {
        RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
    }
}

fn deallocate_runtime_tls(handle: TlsHandle) -> RuntimeStatus {
    if handle.is_none() {
        return RuntimeStatus::Success;
    }
    #[cfg(feature = "tls")]
    {
        // SAFETY: the scheduler consumes a live runtime TLS handle once.
        drop(unsafe {
            Box::from_raw(ptr::with_exposed_provenance_mut::<RuntimeTls>(
                handle.into_raw(),
            ))
        });
        RuntimeStatus::Success
    }
    #[cfg(not(feature = "tls"))]
    {
        RuntimeStatus::Unsupported
    }
}

fn create_runtime_context(request: KernelContextRequest) -> RuntimeHandleResult {
    create_runtime_context_parts(
        request.stack,
        request.entry,
        request.tls,
        request.address_space,
    )
}

fn create_user_runtime_context(request: UserContextRequest) -> RuntimeHandleResult {
    if request.address_space.is_none() {
        return RuntimeHandleResult::failure(RuntimeStatus::InvalidHandle);
    }
    create_runtime_context_parts(
        request.stack,
        request.entry,
        request.tls,
        request.address_space,
    )
}

fn create_runtime_context_parts(
    stack_handle: StackHandle,
    entry: ax_task::runtime::KernelEntry,
    tls_handle: TlsHandle,
    address_space: AddressSpaceHandle,
) -> RuntimeHandleResult {
    if stack_handle.is_none() {
        return RuntimeHandleResult::failure(RuntimeStatus::InvalidHandle);
    }
    // SAFETY: the scheduler keeps the stack handle live until context destroy.
    let stack = unsafe { &*ptr::with_exposed_provenance::<RuntimeStack>(stack_handle.into_raw()) };
    let tls_pointer = runtime_tls_pointer(tls_handle);
    let mut context = ax_hal::context::TaskContext::new();
    context.init(
        entry as usize,
        ax_memory_addr::VirtAddr::from(stack.usable_top),
        ax_hal::context::KernelTlsBase::new(tls_pointer),
    );
    #[cfg(not(feature = "uspace"))]
    if !address_space.is_none() {
        return RuntimeHandleResult::failure(RuntimeStatus::Unsupported);
    }
    #[cfg(feature = "uspace")]
    let _ = address_space;
    RuntimeHandleResult::success(
        RuntimeContext::allocate(context, stack_handle).expose_provenance(),
    )
}

fn create_bootstrap_context() -> ExecutionContextHandle {
    let context = ax_hal::context::TaskContext::new();
    let context = RuntimeContext::allocate(context, StackHandle::NONE);
    // SAFETY: Box::into_raw yields a non-null uniquely owned RuntimeContext
    // that stays live until destroy_runtime_context consumes the handle.
    unsafe { ExecutionContextHandle::from_raw(context.expose_provenance()) }
}

#[cfg(feature = "uspace")]
fn resolve_address_space_root(address_space: AddressSpaceHandle) -> usize {
    if !address_space.is_none() {
        return address_space.into_raw();
    }
    if cfg!(any(target_arch = "x86_64", target_arch = "riscv64")) {
        // SAFETY: every CPU captures its kernel root before it is published to
        // the scheduler, and the per-CPU slot remains immutable afterwards.
        unsafe { KERNEL_ADDRESS_SPACE_ROOT.read_current_raw() }
    } else {
        // AArch64 and LoongArch have distinct kernel roots; zero disables the
        // lower/user translation root without disturbing kernel mappings.
        0
    }
}

fn install_runtime_address_space(address_space: AddressSpaceHandle) -> RuntimeStatus {
    #[cfg(feature = "uspace")]
    {
        let root = ax_memory_addr::PhysAddr::from(resolve_address_space_root(address_space));
        if ax_hal::asm::read_user_page_table() != root {
            // SAFETY: both scheduler switch and exec replacement invoke this
            // with local IRQs disabled after committing the selected address
            // space to the current scheduler endpoint.
            unsafe { ax_hal::asm::write_user_page_table(root) };
            ax_hal::asm::flush_tlb(None);
        }
        RuntimeStatus::Success
    }
    #[cfg(not(feature = "uspace"))]
    {
        if address_space.is_none() {
            RuntimeStatus::Success
        } else {
            RuntimeStatus::Unsupported
        }
    }
}

fn destroy_runtime_context(handle: ExecutionContextHandle) -> RuntimeStatus {
    if handle.is_none() {
        return RuntimeStatus::InvalidHandle;
    }
    let context = ptr::with_exposed_provenance_mut::<RuntimeContext>(handle.into_raw());
    // SAFETY: the scheduler keeps the runtime handle live while asking whether
    // its physical CPU handoff has completed.
    let context_ref = unsafe { &*context };
    if context_ref.header.cpu_binding().is_some() || context_ref.switch_tail().is_some() {
        return RuntimeStatus::Busy;
    }
    // SAFETY: the scheduler proves this context cannot run again and consumes
    // its runtime handle exactly once.
    drop(unsafe { Box::from_raw(context) });
    RuntimeStatus::Success
}

#[cfg(feature = "tls")]
fn runtime_tls_pointer(handle: TlsHandle) -> usize {
    if handle.is_none() {
        return 0;
    }
    // SAFETY: context creation borrows a live runtime TLS handle.
    unsafe {
        (&*ptr::with_exposed_provenance::<RuntimeTls>(handle.into_raw()))
            .area
            .tls_ptr()
            .addr()
    }
}

#[cfg(not(feature = "tls"))]
fn runtime_tls_pointer(_handle: TlsHandle) -> usize {
    0
}

fn map_alloc_status(error: ax_alloc::AllocError) -> RuntimeStatus {
    match error {
        ax_alloc::AllocError::NoMemory => RuntimeStatus::NoMemory,
        ax_alloc::AllocError::InvalidParam => RuntimeStatus::InvalidArgument,
        _ => RuntimeStatus::Platform,
    }
}

struct ArceOsTaskRuntime;

impl_task_runtime! {
    impl TaskRuntime for ArceOsTaskRuntime {
        unsafe fn task_system_handle() -> TaskSystemHandle {
            task_system().map_or(TaskSystemHandle::NONE, |system| {
                // SAFETY: TASK_SYSTEM owns this pinned allocation through
                // shutdown and exposes it only through shared scheduler APIs.
                unsafe {
                    TaskSystemHandle::from_raw(
                        (system as *const TaskSystem).expose_provenance(),
                    )
                }
            })
        }

        unsafe fn current_cpu_local_handle() -> CurrentCpuLocalHandle {
            // SAFETY: the ax-task caller already owns a CPU pin, and the slot
            // is initialized from the unique pinned CpuLocal allocation before
            // that CPU becomes visible to scheduler entry paths.
            let raw = unsafe { CPU_LOCAL_OWNER_HANDLE.read_current_raw() };
            // SAFETY: zero denotes pre-initialization; every nonzero value is
            // the shutdown-lifetime owner capability installed above.
            unsafe { CurrentCpuLocalHandle::from_raw(raw) }
        }

        unsafe fn cpu_remote_handle(cpu: RuntimeCpuId) -> CpuRemoteHandle {
            cpu_remote(cpu).map_or(CpuRemoteHandle::NONE, |cpu| {
                // SAFETY: TaskSystem owns this Arc-backed CpuRemote endpoint
                // through shutdown and the lookup preserves its CPU identity.
                unsafe {
                    CpuRemoteHandle::from_raw((cpu as *const CpuRemote).expose_provenance())
                }
            })
        }

        fn current_cpu_id() -> RuntimeCpuId {
            let cpu = u32::try_from(ax_hal::percpu::this_cpu_id())
                .expect("logical CPU ID must fit the TaskRuntime ABI");
            RuntimeCpuId::new(cpu)
        }

        fn online_cpu_count() -> u32 {
            task_system()
                .and_then(|system| u32::try_from(system.online_cpu_count()).ok())
                .unwrap_or(0)
        }

        fn irq_guard_enter() -> IrqGuardToken {
            #[cfg(test)]
            {
                // SAFETY: test mode models one balanced runtime IRQ token.
                unsafe { IrqGuardToken::from_raw(1) }
            }
            #[cfg(not(test))]
            {
                crate::guard::enter_irq();
                // SAFETY: enter_irq established the matching live guard state.
                unsafe { IrqGuardToken::from_raw(1) }
            }
        }

        unsafe fn irq_guard_exit(_token: IrqGuardToken) {
            #[cfg(not(test))]
            crate::guard::exit_irq("task runtime");
        }

        fn finish_context_switch_tail() -> RuntimeStatus {
            finish_runtime_context_switch_tail()
        }

        fn finish_initial_context_switch() {
            crate::guard::finish_initial_context_switch();
        }

        fn scheduler_frame_guard_enter(
            origin: ax_task::runtime::RuntimeScheduleOrigin,
            entry: ax_task::runtime::RuntimeSchedulerEntry,
        ) -> RuntimeStatus {
            crate::guard::enter_scheduler_frame_guard(origin, entry)
        }

        fn scheduler_frame_guard_exit(
            return_to: ax_task::runtime::RuntimeSchedulerReturn,
        ) -> bool {
            crate::guard::exit_scheduler_frame_guard(return_to)
        }

        fn in_hard_irq() -> bool {
            #[cfg(test)]
            {
                false
            }
            #[cfg(all(not(test), feature = "irq"))]
            {
                ax_hal::irq::in_irq_context()
            }
            #[cfg(all(not(test), not(feature = "irq")))]
            {
                false
            }
        }

        fn validate_schedule_context(
            origin: ax_task::runtime::RuntimeScheduleOrigin,
        ) -> RuntimeStatus {
            crate::guard::validate_schedule_context(origin)
        }

        fn monotonic_ns() -> u64 {
            ax_hal::time::monotonic_time_nanos()
        }

        fn timer_resolution_ns() -> u64 {
            // The four supported architectures expose different counter
            // frequencies. Deriving one representable tick avoids rounding a
            // nanosecond deadline back to the current hardware tick and
            // repeatedly delivering an early interrupt.
            let frequency_hz =
                ax_hal::time::nanos_to_ticks(ax_hal::time::NANOS_PER_SEC);
            crate::timer_resolution_from_frequency(frequency_hz)
        }

        fn program_oneshot_timer(deadline_ns: u64) -> RuntimeStatus {
            #[cfg(feature = "irq")]
            {
                // SAFETY: TaskRuntime invokes this only for the current CPU while
                // its nested IRQ service serializes task-timer programming.
                unsafe { NEXT_TASK_TIMER_DEADLINE_NS.write_current_raw(deadline_ns) };
                crate::program_next_timer();
                RuntimeStatus::Success
            }
            #[cfg(not(feature = "irq"))]
            {
                let _ = deadline_ns;
                RuntimeStatus::Unsupported
            }
        }

        fn dispatch_expired_timer(
            event: ax_task::runtime::RuntimeTimerEventV1,
        ) -> RuntimeStatus {
            #[cfg(feature = "workqueue")]
            {
                crate::workqueue::dispatch_expired_timer(event)
            }
            #[cfg(not(feature = "workqueue"))]
            {
                let _ = event;
                RuntimeStatus::Unsupported
            }
        }

        fn send_scheduler_ipi(cpu: RuntimeCpuId) -> RuntimeStatus {
            #[cfg(any(feature = "ipi", feature = "wake-ipi"))]
            {
                let irq_guard = IrqGuard::new();
                let current_cpu = ax_hal::irq::CpuId(ax_hal::percpu::this_cpu_id_pinned(
                    irq_guard.cpu_pin(),
                ));
                let Ok(destination) = usize::try_from(cpu.as_u32()) else {
                    return RuntimeStatus::InvalidArgument;
                };
                let destination = ax_hal::irq::CpuId(destination);
                match ax_hal::irq::send_ipi(
                    ax_hal::irq::ipi_irq(),
                    scheduler_ipi_target(current_cpu, destination),
                    &irq_guard,
                ) {
                    ax_hal::irq::IpiSendStatus::Success => RuntimeStatus::Success,
                    ax_hal::irq::IpiSendStatus::Retry => RuntimeStatus::Busy,
                    ax_hal::irq::IpiSendStatus::Invalid => RuntimeStatus::InvalidArgument,
                }
            }
            #[cfg(not(any(feature = "ipi", feature = "wake-ipi")))]
            {
                let _ = cpu;
                RuntimeStatus::Unsupported
            }
        }

        fn wait_for_interrupt() {
            #[cfg(feature = "ipi")]
            {
                ax_ipi::service_callback_ipi_retries(64);
                if ax_ipi::callback_ipi_retry_pending() {
                    return;
                }
            }
            ax_hal::asm::wait_for_irqs();
        }

        fn allocate_stack(_request: StackRequest) -> RuntimeHandleResult {
            match allocate_runtime_stack(_request) {
                Ok(handle) => RuntimeHandleResult::success(handle.into_raw()),
                Err(status) => RuntimeHandleResult::failure(status),
            }
        }

        fn deallocate_stack(_stack: StackHandle) -> RuntimeStatus {
            deallocate_runtime_stack(_stack)
        }

        fn allocate_tls(_request: TlsRequest) -> RuntimeHandleResult {
            allocate_runtime_tls(_request)
        }

        fn deallocate_tls(_tls: TlsHandle) -> RuntimeStatus {
            deallocate_runtime_tls(_tls)
        }

        fn create_kernel_context(_request: KernelContextRequest) -> RuntimeHandleResult {
            create_runtime_context(_request)
        }

        fn create_user_context(_request: UserContextRequest) -> RuntimeHandleResult {
            create_user_runtime_context(_request)
        }

        fn bind_context_thread(binding: ContextThreadBinding) -> RuntimeStatus {
            let Some(thread_identity) = ThreadIdentity::from_parts(
                binding.identity.slot,
                binding.identity.generation,
            ) else {
                return RuntimeStatus::InvalidArgument;
            };
            let Ok(context) = runtime_context(binding.context) else {
                return RuntimeStatus::InvalidHandle;
            };
            let header = context.header();
            if header.bind_thread(thread_identity).is_err() {
                return RuntimeStatus::InvalidArgument;
            }
            // Scheduler construction invokes this exactly once before the
            // context can enter a run queue. The bootstrap placeholder is
            // likewise not consumed by assembly until its first switch-out.
            unsafe { &mut *context.inner.get() }.set_current_header(header.as_non_null());
            RuntimeStatus::Success
        }

        fn destroy_context(_context: ExecutionContextHandle) -> RuntimeStatus {
            destroy_runtime_context(_context)
        }

        unsafe fn switch_context(
            previous: ExecutionContextHandle,
            next: ExecutionContextHandle,
        ) {
            assert!(!previous.is_none(), "previous task context is missing");
            assert!(!next.is_none(), "next task context is missing");
            assert_ne!(previous, next, "raw context switch requires distinct contexts");
            crate::guard::assert_scheduler_switch_baton();
            let previous_raw = previous.into_raw();
            let next_raw = next.into_raw();
            // SAFETY: the active scheduler baton keeps this execution pinned
            // with local IRQs disabled until the raw switch commits.
            let cpu_pin = unsafe { CpuPin::new_unchecked() };
            let (prefix, published_previous) = current_runtime_context(&cpu_pin)
                .unwrap_or_else(|status| panic!("current runtime context is invalid: {status:?}"));
            let previous = ptr::with_exposed_provenance_mut::<RuntimeContext>(previous_raw);
            let next = ptr::with_exposed_provenance_mut::<RuntimeContext>(next_raw);
            assert!(
                ptr::eq(published_previous, previous),
                "scheduler previous context differs from the pinned current header"
            );
            // SAFETY: both handles are live and uniquely owned by the committed
            // scheduler switch plan until this handoff finishes.
            let previous_context = unsafe { &*previous };
            let next_context = unsafe { &*next };
            // SAFETY: the committed scheduler plan owns mutable access to the
            // outgoing architecture context and shared access to the off-CPU
            // incoming context until the raw handoff.
            let previous_arch_context = unsafe { &mut *previous_context.inner.get() };
            let next_arch_context = unsafe { &*next_context.inner.get() };
            assert_eq!(
                previous_arch_context.current_header(),
                Some(previous_context.header().as_non_null()),
                "outgoing architecture context retained a different current header"
            );
            assert_eq!(
                next_arch_context.current_header(),
                Some(next_context.header().as_non_null()),
                "incoming architecture context retained a different current header"
            );
            let previous_binding = previous_context
                .header
                .cpu_binding()
                .unwrap_or_else(|| panic!("scheduler previous context is not CPU-bound"));
            assert!(
                next_context.header.cpu_binding().is_none(),
                "scheduler next context is already CPU-bound"
            );
            assert!(
                next_context.switch_tail().is_none(),
                "scheduler next context retained an unfinished switch tail"
            );
            // FP/SIMD state may execute architecture helpers and therefore must
            // finish before the incoming CPU binding or current slot changes.
            previous_arch_context.prepare_switch_to(next_arch_context);
            // SAFETY: the scheduler selected `next` while it is off-CPU and
            // the current CPU's immutable prefix binding remains pinned.
            let next_epoch = unsafe { next_context.header().bind_cpu(prefix.header().binding()) }
                .unwrap_or_else(|error| panic!("failed to bind next runtime context: {error}"));
            // Validate every fallible publication condition before staging the
            // irreversible scheduler baton transfer.
            let prepared_current = match unsafe {
                prepare_current_runtime_context_publish(&cpu_pin, next_context)
            } {
                Ok(prepared) => prepared,
                Err(status) => {
                    // SAFETY: publication and tail staging have not occurred;
                    // this path still owns the exact binding just installed.
                    let rollback = unsafe { next_context.header().unbind_cpu(next_epoch) };
                    assert!(rollback.is_ok(), "failed to roll back incoming CPU binding");
                    panic!("failed to prepare next runtime context: {status:?}");
                }
            };
            let tail = RuntimeSwitchTail {
                previous: previous_context.header().as_non_null(),
                binding_epoch: previous_binding.epoch(),
            };
            if let Err(status) = unsafe { next_context.stage_switch_tail(tail) } {
                // SAFETY: current-thread publication has not changed, so this
                // path still owns the exact incoming binding it just created.
                let rollback = unsafe { next_context.header().unbind_cpu(next_epoch) };
                assert!(rollback.is_ok(), "failed to roll back incoming CPU binding");
                panic!("failed to stage runtime switch tail: {status:?}");
            }
            crate::guard::transfer_scheduler_switch_baton();
            // SAFETY: the scheduler commits unique ownership of the previous
            // running context and immutable access to the selected next
            // context. The infallible Release publication and naked switch are
            // deliberately adjacent: no old-context Rust may observe the next
            // current slot. TaskSystem completes `on_cpu` only in switch tail.
            unsafe { ax_hal::percpu::commit_current_thread_publish(prepared_current) };
            unsafe { previous_arch_context.switch_to_raw(next_arch_context) };
        }

        fn install_address_space(address_space: AddressSpaceHandle) -> RuntimeStatus {
            install_runtime_address_space(address_space)
        }

        fn flush_tlb_local(_start: usize, _size: usize) {
            ax_hal::asm::flush_tlb(None);
        }

        fn trace_sched_switch(record: SchedSwitchRecord) {
            let hook = SCHED_SWITCH_TRACE_HOOK.load(Ordering::Acquire);
            if hook.is_null() {
                return;
            }
            // SAFETY: installation accepts exactly this function-pointer type,
            // and the process-wide hook is never replaced or removed.
            let hook = unsafe { core::mem::transmute::<*mut (), SchedSwitchTraceHook>(hook) };
            hook(record);
        }

        fn fatal_invariant(code: u32, argument: usize) -> ! {
            panic!("ax-task invariant {code} failed with argument {argument:#x}")
        }
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};
    #[cfg(feature = "uspace")]
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;

    #[cfg(feature = "uspace")]
    #[test]
    fn user_entry_epoch_exhaustion_is_a_permanently_pending_fatal_state() {
        let notification = UserEntryNotification {
            produced: AtomicU64::new(u64::MAX - 1),
            acknowledged: AtomicU64::new(u64::MAX - 1),
        };

        let exhausted = catch_unwind(AssertUnwindSafe(|| notification.publish()));
        assert!(exhausted.is_err());
        assert_eq!(notification.produced.load(Ordering::Acquire), u64::MAX);
        assert!(notification.pending());

        let poison = notification.snapshot();
        assert_eq!(notification.acknowledge(poison), UserEntryAck::Pending);
        assert_eq!(
            notification.acknowledged.load(Ordering::Acquire),
            u64::MAX - 1
        );

        let repeated = catch_unwind(AssertUnwindSafe(|| notification.publish()));
        assert!(repeated.is_err());
        assert_eq!(notification.produced.load(Ordering::Acquire), u64::MAX);
        assert!(notification.pending());
    }

    static TEST_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: ignore_extension_thread_event,
        on_switch_out: ignore_extension_switch_out,
        on_policy_applied: ignore_extension_policy_applied,
        on_exit: ignore_extension_thread_event,
        on_deadline_overrun: ignore_extension_thread_event,
        drop: count_extension_drop,
    };

    #[cfg(any(feature = "ipi", feature = "wake-ipi"))]
    #[test]
    fn scheduler_doorbell_uses_the_typed_current_cpu_route() {
        let cpu0 = ax_hal::irq::CpuId(0);
        let cpu1 = ax_hal::irq::CpuId(1);

        assert_eq!(
            scheduler_ipi_target(cpu0, cpu0),
            ax_hal::irq::CpuIpiTarget::Current { cpu: cpu0 }
        );
        assert_eq!(
            scheduler_ipi_target(cpu0, cpu1),
            ax_hal::irq::CpuIpiTarget::Other { cpu: cpu1 }
        );
    }

    #[test]
    fn missing_outer_runtime_extension_is_not_an_error() {
        assert_eq!(
            classify_runtime_extension(None, 0),
            Ok(RuntimeExtensionKind::Missing)
        );
    }

    #[test]
    fn foreign_outer_runtime_extension_remains_an_error() {
        assert_eq!(
            classify_runtime_extension(Some(&TEST_EXTENSION_OPS), usize::MAX),
            Err(TaskError::InvalidConfiguration)
        );
    }

    #[test]
    fn matching_runtime_ops_reject_malformed_extension_data() {
        assert_eq!(
            classify_runtime_extension(Some(&RUNTIME_THREAD_EXTENSION_OPS), 0),
            Err(TaskError::InvalidRuntimeHandle)
        );
        assert_eq!(
            classify_runtime_extension(Some(&RUNTIME_THREAD_EXTENSION_OPS), 1),
            Err(TaskError::InvalidRuntimeHandle)
        );

        let data = RuntimeThreadData {
            entry: SpinNoIrq::new(None),
            exit_code: AtomicI32::new(0),
            exit_completed: AtomicBool::new(false),
            join_wait: WaitQueue::new(),
            os_extension: None,
            _name: String::new(),
        };
        assert_eq!(
            classify_runtime_extension(
                Some(&RUNTIME_THREAD_EXTENSION_OPS),
                core::ptr::from_ref(&data).expose_provenance(),
            ),
            Ok(RuntimeExtensionKind::Runtime)
        );
    }

    #[test]
    fn invalid_spawn_releases_transferred_extension() {
        let extension_drops = AtomicUsize::new(0);
        // SAFETY: the call fails synchronously and drops the extension before
        // this stack-owned counter leaves scope.
        let extension = unsafe {
            ThreadExtension::new(
                (&extension_drops as *const AtomicUsize).expose_provenance(),
                &TEST_EXTENSION_OPS,
            )
        };

        // SAFETY: this call transfers the test extension's unique logical ownership.
        let result = unsafe {
            spawn_raw_with_extension_and_affinity(
                || {},
                String::from("invalid-stack"),
                0,
                Some(extension),
                None,
            )
        };

        assert!(matches!(result, Err(TaskError::InvalidConfiguration)));
        assert_eq!(extension_drops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn secondary_bootstrap_retires_before_entering_idle_loop() {
        let bootstrap = ThreadId::from_parts(1, 1);
        let idle = ThreadId::from_parts(2, 1);

        assert_eq!(
            idle_entry_action(Some(bootstrap), Some(idle)).unwrap(),
            IdleEntryAction::RetireBootstrap,
        );
        assert_eq!(
            idle_entry_action(Some(idle), Some(idle)).unwrap(),
            IdleEntryAction::RunIdle,
        );
    }

    #[test]
    fn entry_extension_lookup_does_not_pin_exited_thread() {
        let extension_drops = AtomicUsize::new(0);
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let extension_data = (&extension_drops as *const AtomicUsize).expose_provenance();
        // SAFETY: this test reaps the thread and runs the matching drop callback
        // before the stack-owned counter leaves scope.
        let extension = unsafe { ThreadExtension::new(extension_data, &TEST_EXTENSION_OPS) };
        let spec = ThreadSpec::new(SchedulePolicy::default()).with_extension(extension);
        let handle = system.create_thread(spec).unwrap();
        let lease = system
            .thread_extension_lease(handle.clone())
            .unwrap()
            .unwrap();

        assert_eq!(
            extension_data_after_releasing_lease(lease, &TEST_EXTENSION_OPS).unwrap(),
            extension_data
        );
        system.mark_exited(handle.id()).unwrap();
        assert!(
            system
                .service_deferred_task_work(1)
                .unwrap()
                .made_progress(),
            "the exit callback must finish before the test isolates extension-lease ownership"
        );
        system.reap_thread_handle(handle).unwrap();
        assert_eq!(extension_drops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn user_context_rejects_a_missing_address_space() {
        let result = create_user_runtime_context(UserContextRequest {
            stack: StackHandle::NONE,
            entry: unreachable_test_entry,
            tls: TlsHandle::NONE,
            address_space: AddressSpaceHandle::NONE,
        });

        assert_eq!(result.status, RuntimeStatus::InvalidHandle);
        assert_eq!(result.handle, 0);
    }

    #[cfg(feature = "tls")]
    #[test]
    fn bootstrap_thread_rejects_a_missing_tls_resource() {
        // SAFETY: this inert non-zero identity is never dereferenced because
        // validation rejects the missing TLS resource first.
        let context = unsafe { ExecutionContextHandle::from_raw(1) };
        let result = assemble_bootstrap_resources(context, TlsHandle::NONE);

        assert!(matches!(result, Err(TaskError::InvalidRuntimeHandle)));
    }

    unsafe extern "Rust" fn ignore_extension_thread_event(_data: usize, _thread: ThreadId) {}

    unsafe extern "Rust" fn ignore_extension_switch_out(
        _data: usize,
        _thread: ThreadId,
        _reason: SwitchReason,
    ) {
    }

    unsafe extern "Rust" fn ignore_extension_policy_applied(
        _data: usize,
        _thread: ThreadId,
        _event: ThreadPolicyApplied,
    ) {
    }

    unsafe extern "C" fn unreachable_test_entry() -> ! {
        panic!("invalid user context must not invoke its entry")
    }

    unsafe extern "Rust" fn count_extension_drop(data: usize) {
        // SAFETY: each test keeps its stack-owned counter live until it
        // synchronously observes the extension's matching drop callback.
        let drops = unsafe { &*ptr::with_exposed_provenance::<AtomicUsize>(data) };
        drops.fetch_add(1, Ordering::Release);
    }
}
