//! ArceOS ownership and trait-FFI glue for the OS-independent task system.

use alloc::{boxed::Box, string::String};
use core::{
    alloc::Layout,
    cell::UnsafeCell,
    mem::offset_of,
    pin::Pin,
    ptr::{self, NonNull},
    sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU64, Ordering},
};

use ax_hal::percpu::{CpuPin, CurrentContext, CurrentThreadHeader, PreviousThreadBinding};
use ax_kernel_guard::IrqSave;
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
pub use ax_task::{
    CpuId, CpuSet, DeadlineFlags, DeadlinePolicy, FairMode, IrqRegisterResult, IrqUnregisterResult,
    IrqWaitCell, IrqWaitRegistration, IrqWaitToken, Nice, RtPriority, SchedulePolicy, SwitchReason,
    TaskError, ThreadExtension, ThreadExtensionOps, ThreadHandle, ThreadId, ThreadState,
    ThreadWakeHandle, WaitQueue, WakeResult, cpu_busy_runtime_ns, current_cpu_needs_resched,
    current_thread_extension, current_thread_handle, current_thread_id, executor::LocalExecutor,
    exit_current_thread, quiesce_irq_wait, runtime::SchedSwitchRecord, schedule_current_cpu,
    set_current_thread_affinity, set_thread_affinity, set_thread_policy, sleep, sleep_until,
    thread_affinity, thread_handle, thread_policy, thread_round_robin_interval_ns, thread_runtime,
    yield_current_cpu,
};
use ax_task::{
    CpuLocal, CpuRemote, TaskSystem, TaskSystemConfig, ThreadResources, ThreadSpec,
    impl_trait as impl_task_runtime,
    runtime::{
        AddressSpaceHandle, ContextThreadBinding, CpuRemoteHandle, CurrentCpuLocalHandle,
        ExecutionContextHandle, IrqGuardToken, KernelContextRequest, RuntimeCpuId,
        RuntimeHandleResult, RuntimeStatus, StackHandle, StackRequest, TaskRuntime,
        TaskSystemHandle, TlsHandle, TlsRequest, UserContextRequest,
    },
};

mod executor;
mod scheduler_events;

pub use executor::{BlockOnError, block_on, block_on_timeout};
#[cfg(test)]
use scheduler_events::clock_event_requests_reschedule;
#[cfg(all(test, not(any(feature = "ipi", feature = "wake-ipi"))))]
use scheduler_events::publish_then_notify_scheduler_ipi;
pub use scheduler_events::timer_irq_count;
#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
pub(crate) use scheduler_events::{consume_scheduler_ipi_doorbell, on_scheduler_ipi};
#[cfg(feature = "irq")]
pub(crate) use scheduler_events::{on_clock_event, recover_clock_event};
#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
use scheduler_events::{publish_scheduler_ipi_doorbell, publish_then_notify_scheduler_ipi};

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

const PAGE_SIZE: usize = 4096;

/// Runs one CPU-local operation under the caller's existing migration guard.
///
/// # Safety
///
/// The caller must prevent migration for the complete callback. Runtime callers
/// use this only during offline CPU bring-up, hard IRQ handling, or while a
/// scheduler/IRQ guard owns the current CPU.
unsafe fn with_current_cpu_pin<R>(operation: impl for<'scope> FnOnce(&CpuPin<'scope>) -> R) -> R {
    unsafe { ax_hal::percpu::with_cpu_pin(operation) }
        .unwrap_or_else(|error| panic!("task runtime CPU-local state is invalid: {error}"))
}

fn with_irq_cpu_pin<R>(operation: impl for<'scope> FnOnce(&CpuPin<'scope>) -> R) -> R {
    let _irq = IrqSave::new();
    // SAFETY: IrqSave excludes scheduler migration for the complete callback.
    unsafe { with_current_cpu_pin(operation) }
}

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

/// Scheduler thread whose resources and registry identity exist but which is
/// not yet runnable.
///
/// OS layers use this move-only transaction boundary to publish their own task,
/// PID, and resource registries before the new context can execute. Dropping an
/// unpublished value removes the scheduler record and releases context, stack,
/// TLS, extension, and address-space resources in task context.
pub struct PreparedThread {
    system: &'static TaskSystem,
    handle: Option<ThreadHandle>,
}

impl PreparedThread {
    /// Returns a strong handle for binding OS-owned identity before publication.
    pub fn thread_handle(&self) -> ThreadHandle {
        self.handle
            .as_ref()
            .expect("prepared thread was already consumed")
            .clone()
    }

    /// Makes the prepared thread ready and places it on a run queue.
    pub fn publish(mut self) -> Result<ThreadHandle, TaskError> {
        let handle = self
            .handle
            .take()
            .expect("prepared thread was already consumed");
        publish_prepared_thread(self.system, handle)
    }
}

impl Drop for PreparedThread {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            cleanup_failed_thread(self.system, handle);
        }
    }
}

/// Allocation-free scheduler-switch diagnostic hook installed by an OS layer.
pub type SchedSwitchTraceHook = fn(SchedSwitchRecord);

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
        // architecture epilogue.
        unsafe {
            with_current_cpu_pin(|cpu_pin| {
                let Ok(context) = current_runtime_context(cpu_pin) else {
                    return false;
                };
                let stack = context.stack.into_raw();
                if stack == 0 {
                    return false;
                }
                // SAFETY: the scheduler owns the stack until this context can
                // no longer run, and the current header keeps it on-CPU.
                let stack = &*ptr::with_exposed_provenance::<RuntimeStack>(stack);
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
            })
        }
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
        let _irq = IrqSave::new();
        let root = ax_memory_addr::PhysAddr::from(root);
        // SAFETY: the exec caller transfers a live process page-table root;
        // the scheduler retains only its opaque identity while the process MM
        // remains the allocation owner.
        let address_space = unsafe { AddressSpaceHandle::from_raw(root.as_usize()) };
        // Keep the scheduler endpoint, architecture context, and hardware root
        // coherent across exec. A later switch must restore this new root
        // instead of the address space that existed when the context was built.
        let _old_address_space = ax_task::replace_current_address_space(address_space)?;
        set_current_context_page_table_root(root)?;
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

enum StackBacking {
    Heap {
        pointer: NonNull<u8>,
        layout: Layout,
    },
    #[cfg(feature = "paging")]
    GuardedPages { pages: usize, guard_size: usize },
}

struct RuntimeSwitchTail {
    previous: NonNull<CurrentThreadHeader>,
    binding: PreviousThreadBinding,
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
    thread_identity: AtomicU64,
    stack: StackHandle,
    switch_tail: UnsafeCell<Option<RuntimeSwitchTail>>,
}

const _: () = assert!(offset_of!(RuntimeContext, header) == 0);

impl RuntimeContext {
    fn allocate(inner: ax_hal::context::TaskContext, stack: StackHandle) -> *mut RuntimeContext {
        let inner = Box::new(UnsafeCell::new(inner));
        let identity = CurrentContext::from_raw(inner.get().expose_provenance())
            .expect("an architecture context allocation must have a non-zero identity");
        Box::into_raw(Box::new(Self {
            header: CurrentThreadHeader::new(identity),
            inner,
            thread_identity: AtomicU64::new(0),
            stack,
            switch_tail: UnsafeCell::new(None),
        }))
    }

    fn header(&self) -> Pin<&CurrentThreadHeader> {
        // SAFETY: every RuntimeContext is constructed in a Box and is never
        // moved before destruction after its header is no longer published.
        unsafe { Pin::new_unchecked(&self.header) }
    }

    fn context_identity(&self) -> CurrentContext {
        self.header
            .current_context()
            .expect("runtime task header must retain its context identity")
    }

    fn architecture_context_identity(&self) -> usize {
        self.inner.get().expose_provenance()
    }

    fn has_switch_tail(&self) -> bool {
        // SAFETY: only the incoming scheduler continuation reads this slot;
        // the context is current and local IRQs serialize scheduler entry.
        unsafe { (*self.switch_tail.get()).is_some() }
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
        // IRQs disabled and consumes the one-shot previous-binding token.
        let slot = unsafe { &mut *self.switch_tail.get() };
        let Some(tail) = slot.take() else {
            return RuntimeStatus::InvalidHandle;
        };
        // SAFETY: the outgoing header stays pinned and unreclaimable through
        // the scheduler `on_cpu` handoff; this tail owns its exact epoch.
        let previous = unsafe { Pin::new_unchecked(tail.previous.as_ref()) };
        unsafe { tail.binding.finish(previous) }
            .unwrap_or_else(|error| panic!("failed to finish CPU thread binding: {error}"));
        RuntimeStatus::Success
    }
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

fn current_runtime_context(cpu_pin: &CpuPin) -> Result<&'static RuntimeContext, RuntimeStatus> {
    let current = ax_hal::percpu::current_thread(cpu_pin)
        .map_err(|_| RuntimeStatus::InvalidHandle)?
        .as_ptr()
        .expose_provenance();
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
    if header.current_context().is_none() {
        return Err(RuntimeStatus::NotInitialized);
    }
    if header.cpu_area() != Some(cpu_pin.area()) {
        return Err(RuntimeStatus::InvalidHandle);
    }
    Ok(context)
}

fn bind_bootstrap_runtime_context(
    cpu_pin: &CpuPin,
    handle: ExecutionContextHandle,
    kernel_tls: usize,
) -> Result<(), TaskError> {
    let boot_thread =
        ax_hal::percpu::current_thread(cpu_pin).map_err(|_| TaskError::InvalidConfiguration)?;
    // The permanent boot header has no runtime execution-context identity.
    if unsafe { boot_thread.as_ref() }.current_context().is_some() {
        return Err(TaskError::InvalidConfiguration);
    }
    let context = runtime_context(handle).map_err(runtime_status_error)?;
    // SAFETY: the CPU is still offline and trap-free, while the scheduler
    // record keeps this pinned header alive until its switch tail withdraws it.
    unsafe { ax_hal::percpu::install_bootstrap_thread(cpu_pin, context.header()) }
        .map_err(|_| TaskError::InvalidConfiguration)?;
    #[cfg(feature = "tls")]
    // SAFETY: the same offline bootstrap boundary owns the task TLS register.
    unsafe {
        ax_hal::percpu::install_bootstrap_kernel_tls(
            cpu_pin,
            ax_hal::context::KernelTlsBase::new(kernel_tls),
        );
    }
    #[cfg(not(feature = "tls"))]
    assert_eq!(
        kernel_tls, 0,
        "TLS-disabled bootstrap must retain a zero TLS identity"
    );
    Ok(())
}

fn finish_runtime_context_switch_tail() -> RuntimeStatus {
    // SAFETY: TaskSystem invokes this with the scheduler baton and local IRQs
    // disabled immediately after entering the incoming context.
    unsafe {
        with_current_cpu_pin(|cpu_pin| {
            let Ok(current) = current_runtime_context(cpu_pin) else {
                return RuntimeStatus::InvalidHandle;
            };
            // SAFETY: the incoming context exclusively owns its staged one-shot tail.
            current.finish_switch_tail()
        })
    }
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
    // SAFETY: early runtime entry owns this offline CPU until publication.
    let existing = unsafe { with_current_cpu_pin(|pin| EARLY_BOOTSTRAP_TLS.read_current(pin)) };
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
    // SAFETY: this CPU remains offline, so the callback exclusively owns its
    // bootstrap slot and task TLS register.
    unsafe {
        with_current_cpu_pin(|pin| {
            // Publish the allocation owner before installing its hardware base.
            EARLY_BOOTSTRAP_TLS.write_current(pin, result.handle);
            ax_hal::percpu::install_bootstrap_kernel_tls(
                pin,
                ax_hal::context::KernelTlsBase::new(runtime_tls_pointer(early_tls)),
            );
        })
    };
    Ok(())
}

/// Creates and publishes the calling secondary CPU's local scheduler object.
#[cfg(feature = "smp")]
pub(crate) fn initialize_secondary(cpu_id: usize) -> Result<(), TaskError> {
    initialize_current_cpu(cpu_id).map(|_| ())
}

/// Publishes a prepared CPU after local timer and scheduler-IPI paths are ready.
#[must_use = "local IRQs may be enabled only after consuming this publication proof"]
pub(crate) struct PublishedCpuOnline(());

/// Publishes a prepared CPU after local timer and scheduler-IPI paths are ready.
pub(crate) fn publish_current_cpu_online() -> Result<PublishedCpuOnline, TaskError> {
    let system = task_system().ok_or(TaskError::NotInitialized)?;
    with_current_cpu_local_mut_for_boot(|cpu| system.bring_cpu_online(cpu))?;
    Ok(PublishedCpuOnline(()))
}

/// Starts the single ordinary-context worker for scheduler callbacks/reaping.
pub(crate) fn start_deferred_task_work_service() -> Result<(), TaskError> {
    ax_task::start_deferred_task_work_service()
}

/// Runs the owner CPU's scheduler/idle handshake forever.
pub(crate) fn run_idle() -> ! {
    let (current, idle) = with_irq_cpu_pin(|pin| {
        current_cpu_remote(pin)
            .map(|cpu| (cpu.current_thread(), cpu.idle_thread()))
            .unwrap_or((None, None))
    });
    let entry_action = idle_entry_action(current, idle)
        .unwrap_or_else(|error| panic!("idle loop entered without scheduler ownership: {error}"));
    if entry_action == IdleEntryAction::RetireBootstrap {
        match ax_task::exit_current_thread() {
            Err(error) => panic!("failed to retire secondary bootstrap thread: {error}"),
            Ok(()) => panic!("retired secondary bootstrap thread unexpectedly resumed"),
        }
    }
    loop {
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
        let _irq = IrqSave::new();
        #[cfg(feature = "irq")]
        crate::take_current_clock_event_offline();
        ax_hal::power::system_off();
    }

    let exit_permit = ax_task::prepare_current_exit()
        .unwrap_or_else(|error| panic!("failed to prepare scheduler thread exit: {error}"));
    publish_current_runtime_exit(exit_code)
        .unwrap_or_else(|error| panic!("failed to publish thread exit: {error}"));
    ax_task::commit_current_exit(exit_permit)
}

/// Creates a scheduler-owned kernel thread and enqueues it on the current CPU.
pub fn spawn_raw<F>(entry: F, name: String, stack_size: usize) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    // SAFETY: `None` carries no external callback ownership.
    unsafe { spawn_raw_with_extension(entry, name, stack_size, None) }
}

/// Creates a scheduler-owned kernel thread with pre-publication affinity.
pub fn spawn_raw_with_affinity<F>(
    entry: F,
    name: String,
    stack_size: usize,
    affinity: CpuSet,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    // SAFETY: `None` carries no external callback ownership, while the affinity
    // is installed before the new thread is published to a run queue.
    unsafe { spawn_raw_with_extension_and_affinity(entry, name, stack_size, None, Some(affinity)) }
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

/// Prepares a user thread without making it runnable.
///
/// This is the transactional form of
/// [`spawn_raw_with_extension_in_address_space_and_policy`]. The caller may
/// publish OS registries through [`PreparedThread::thread_handle`] and must then
/// call [`PreparedThread::publish`]. Dropping the token rolls everything back.
///
/// # Safety
///
/// The extension and address-space ownership rules are identical to
/// [`spawn_raw_with_extension_in_address_space_and_policy`].
pub unsafe fn prepare_raw_with_extension_in_address_space_and_policy<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    address_space: TaskAddressSpace,
    policy: SchedulePolicy,
) -> Result<PreparedThread, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        prepare_raw_with_options(
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

/// Prepares a RISC-V user thread with FP state without making it runnable.
///
/// # Safety
///
/// The ownership rules are identical to
/// [`spawn_raw_with_extension_in_address_space_and_fp_state_and_policy`].
#[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
pub unsafe fn prepare_raw_with_extension_in_address_space_and_fp_state_and_policy<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    address_space: TaskAddressSpace,
    fp_state: ax_hal::cpu::FpState,
    policy: SchedulePolicy,
) -> Result<PreparedThread, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    unsafe {
        prepare_raw_with_options(
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
    unsafe {
        prepare_raw_with_options(
            entry,
            name,
            stack_size,
            os_extension,
            affinity,
            policy,
            context_state,
        )
    }?
    .publish()
}

unsafe fn prepare_raw_with_options<F>(
    entry: F,
    name: String,
    stack_size: usize,
    os_extension: Option<ThreadExtension>,
    affinity: Option<CpuSet>,
    policy: SchedulePolicy,
    context_state: InitialContextState,
) -> Result<PreparedThread, TaskError>
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
    Ok(PreparedThread {
        system,
        handle: Some(handle),
    })
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
        // SAFETY: this owner CPU remains offline and has not entered a
        // scheduler-managed user address space.
        unsafe {
            with_current_cpu_pin(|pin| KERNEL_ADDRESS_SPACE_ROOT.write_current(pin, kernel_root))
        };
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
    // Publish the physical bootstrap resources only after their scheduler
    // record owns them. A failed installation must not leave this CPU using a
    // context or TLS allocation that no scheduler record can release.
    // SAFETY: platform entry installed the final CPU area and this CPU remains
    // offline and trap-free through bootstrap context publication.
    unsafe {
        with_current_cpu_pin(|pin| {
            bind_bootstrap_runtime_context(pin, bootstrap_context, bootstrap_kernel_tls)
        })
    }
    .unwrap_or_else(|error| panic!("failed to publish bootstrap runtime context: {error}"));
    #[cfg(feature = "tls")]
    {
        // SAFETY: bootstrap still owns this offline CPU-local slot.
        let early_tls = unsafe {
            with_current_cpu_pin(|pin| {
                let handle = EARLY_BOOTSTRAP_TLS.read_current(pin);
                EARLY_BOOTSTRAP_TLS.write_current(pin, 0);
                TlsHandle::from_raw(handle)
            })
        };
        assert!(
            !early_tls.is_none(),
            "scheduler bootstrap requires early TLS ownership"
        );
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
    // SAFETY: this CPU remains offline with IRQs disabled, so it exclusively
    // owns every mutable value in its initialized final CPU area.
    unsafe {
        with_current_cpu_pin(|pin| {
            ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                CPU_LOCAL.with_current_mut(exclusive, |slot| {
                    slot.init_once(cpu);
                });
            });
            CPU_LOCAL_OWNER_HANDLE.write_current(pin, owner_handle);
        })
    };
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
    match create_thread_resources_with(
        &mut RuntimeThreadResourceBackend,
        stack_size,
        entry,
        context_state,
    ) {
        Ok(resources) => Ok(resources),
        Err(failure) => {
            let (error, unreleased) = failure.into_parts();
            if let Some(unreleased) = unreleased {
                let resources = unreleased.into_resources();
                let system = task_system().ok_or(TaskError::NotInitialized)?;
                let _release = system.release_unpublished_resources(resources);
            }
            Err(error)
        }
    }
}

trait ThreadResourceBackend {
    fn allocate_stack(&mut self, request: StackRequest) -> Result<StackHandle, RuntimeStatus>;

    fn deallocate_stack(&mut self, stack: StackHandle) -> RuntimeStatus;

    fn allocate_tls(&mut self, request: TlsRequest) -> RuntimeHandleResult;

    fn deallocate_tls(&mut self, tls: TlsHandle) -> RuntimeStatus;

    fn create_kernel_context(&mut self, request: KernelContextRequest) -> RuntimeHandleResult;

    fn create_user_context(&mut self, request: UserContextRequest) -> RuntimeHandleResult;
}

#[derive(Debug)]
struct ThreadResourceCreationFailure {
    error: TaskError,
    unreleased: Option<UnreleasedThreadResources>,
}

impl ThreadResourceCreationFailure {
    const fn new(error: TaskError) -> Self {
        Self {
            error,
            unreleased: None,
        }
    }

    const fn with_unreleased(error: TaskError, unreleased: UnreleasedThreadResources) -> Self {
        Self {
            error,
            unreleased: Some(unreleased),
        }
    }

    fn into_parts(self) -> (TaskError, Option<UnreleasedThreadResources>) {
        (self.error, self.unreleased)
    }
}

#[derive(Debug, Eq, PartialEq)]
struct UnreleasedThreadResources {
    stack: StackHandle,
    tls: TlsHandle,
}

impl UnreleasedThreadResources {
    fn into_resources(self) -> ThreadResources {
        unsafe {
            // SAFETY: the failed construction transaction still owns every
            // non-zero handle retained here and never created a context.
            ThreadResources::new(
                ExecutionContextHandle::NONE,
                self.stack,
                self.tls,
                AddressSpaceHandle::NONE,
            )
        }
    }
}

struct RuntimeThreadResourceBackend;

impl ThreadResourceBackend for RuntimeThreadResourceBackend {
    fn allocate_stack(&mut self, request: StackRequest) -> Result<StackHandle, RuntimeStatus> {
        allocate_runtime_stack(request)
    }

    fn deallocate_stack(&mut self, stack: StackHandle) -> RuntimeStatus {
        deallocate_runtime_stack(stack)
    }

    fn allocate_tls(&mut self, request: TlsRequest) -> RuntimeHandleResult {
        allocate_runtime_tls(request)
    }

    fn deallocate_tls(&mut self, tls: TlsHandle) -> RuntimeStatus {
        deallocate_runtime_tls(tls)
    }

    fn create_kernel_context(&mut self, request: KernelContextRequest) -> RuntimeHandleResult {
        create_runtime_context(request)
    }

    fn create_user_context(&mut self, request: UserContextRequest) -> RuntimeHandleResult {
        create_user_runtime_context(request)
    }
}

fn create_thread_resources_with(
    backend: &mut impl ThreadResourceBackend,
    stack_size: usize,
    entry: ax_task::runtime::KernelEntry,
    context_state: InitialContextState,
) -> Result<ThreadResources, ThreadResourceCreationFailure> {
    let guard_size = if cfg!(feature = "stack-guard-page") {
        PAGE_SIZE
    } else {
        0
    };
    let stack = backend
        .allocate_stack(StackRequest {
            usable_size: stack_size,
            alignment: 16,
            guard_size,
        })
        .map_err(|status| ThreadResourceCreationFailure::new(runtime_status_error(status)))?;
    if stack.is_none() {
        return Err(ThreadResourceCreationFailure::new(
            TaskError::InvalidRuntimeHandle,
        ));
    }
    let tls_result = backend.allocate_tls(TlsRequest {
        template_start: 0,
        initialized_size: 0,
        total_size: 0,
        alignment: 1,
    });
    let tls = match (tls_result.status, tls_result.handle) {
        (RuntimeStatus::Success, 0) => {
            return Err(rollback_thread_resource_creation(
                backend,
                stack,
                TlsHandle::NONE,
                TaskError::InvalidRuntimeHandle,
            ));
        }
        (RuntimeStatus::Success, handle) => {
            // SAFETY: the runtime returned a fresh, non-zero TLS allocation
            // whose unique ownership moves into this thread's resources.
            unsafe { TlsHandle::from_raw(handle) }
        }
        (RuntimeStatus::Unsupported, _) => TlsHandle::NONE,
        (status, _) => {
            return Err(rollback_thread_resource_creation(
                backend,
                stack,
                TlsHandle::NONE,
                runtime_status_error(status),
            ));
        }
    };
    let context_result = if context_state.address_space.is_none() {
        backend.create_kernel_context(KernelContextRequest {
            stack,
            entry,
            tls,
            address_space: AddressSpaceHandle::NONE,
        })
    } else {
        backend.create_user_context(UserContextRequest {
            stack,
            entry,
            tls,
            address_space: context_state.address_space,
        })
    };
    if context_result.status != RuntimeStatus::Success {
        return Err(rollback_thread_resource_creation(
            backend,
            stack,
            tls,
            runtime_status_error(context_result.status),
        ));
    }
    if context_result.handle == 0 {
        return Err(rollback_thread_resource_creation(
            backend,
            stack,
            tls,
            TaskError::InvalidRuntimeHandle,
        ));
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

fn rollback_thread_resource_creation(
    backend: &mut impl ThreadResourceBackend,
    stack: StackHandle,
    tls: TlsHandle,
    error: TaskError,
) -> ThreadResourceCreationFailure {
    let retained_tls = if tls.is_none() || backend.deallocate_tls(tls) == RuntimeStatus::Success {
        TlsHandle::NONE
    } else {
        tls
    };
    let retained_stack = if backend.deallocate_stack(stack) == RuntimeStatus::Success {
        StackHandle::NONE
    } else {
        stack
    };
    if retained_tls.is_none() && retained_stack.is_none() {
        ThreadResourceCreationFailure::new(error)
    } else {
        ThreadResourceCreationFailure::with_unreleased(
            error,
            UnreleasedThreadResources {
                stack: retained_stack,
                tls: retained_tls,
            },
        )
    }
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

fn publish_prepared_thread(
    system: &'static TaskSystem,
    handle: ThreadHandle,
) -> Result<ThreadHandle, TaskError> {
    let result = system.make_ready(handle.id()).and_then(|()| {
        with_current_cpu_local_mut_owner(|cpu| {
            system.place_ready(cpu, handle.id(), ax_hal::time::monotonic_time_nanos())
        })
    });
    if let Err(error) = result {
        cleanup_failed_thread(system, handle);
        return Err(error);
    }
    Ok(handle)
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

/// Returns the kernel stack size used by ordinary runtime threads.
pub const fn default_task_stack_size() -> usize {
    runtime_task_stack_size()
}

fn task_system() -> Option<&'static TaskSystem> {
    TASK_SYSTEM.get().map(|system| system.as_ref().get_ref())
}

fn with_current_cpu_local_mut_for_boot<R>(
    operation: impl for<'cpu> FnOnce(Pin<&'cpu mut CpuLocal>) -> Result<R, TaskError>,
) -> Result<R, TaskError> {
    if ax_hal::asm::irqs_enabled() {
        return Err(TaskError::InvalidConfiguration);
    }
    // SAFETY: this CPU has installed its final area but remains offline with
    // local IRQs disabled. No scheduler entry or remote owner claim can overlap
    // the exclusive borrow used to perform the one-way online transition.
    unsafe {
        with_current_cpu_pin(|pin| {
            ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                CPU_LOCAL.with_current_mut(exclusive, |slot| {
                    let cpu = slot.get_mut().ok_or(TaskError::NotInitialized)?;
                    let actual = (cpu.as_ref().get_ref() as *const CpuLocal).expose_provenance();
                    let expected = CPU_LOCAL_OWNER_HANDLE.read_current(pin);
                    if expected == 0 || actual != expected {
                        return Err(TaskError::InvalidRuntimeHandle);
                    }
                    operation(cpu.as_mut())
                })
            })
        })
    }
}

struct RuntimeIrqScope;

impl RuntimeIrqScope {
    fn enter() -> Self {
        crate::guard::enter_irq();
        Self
    }
}

impl Drop for RuntimeIrqScope {
    fn drop(&mut self) {
        crate::guard::exit_irq("runtime CPU owner");
    }
}

fn with_current_cpu_local_mut_owner<R>(
    operation: impl for<'cpu> FnOnce(Pin<&'cpu mut CpuLocal>) -> Result<R, TaskError>,
) -> Result<R, TaskError> {
    let _irq = RuntimeIrqScope::enter();
    // SAFETY: RuntimeIrqScope prevents migration and local re-entry for the
    // complete pin and dynamically gated owner borrow.
    unsafe {
        with_current_cpu_pin(|pin| {
            let remote = current_cpu_remote(pin).ok_or(TaskError::NotInitialized)?;
            let raw = CPU_LOCAL_OWNER_HANDLE.read_current(pin);
            if raw == 0 {
                return Err(TaskError::NotInitialized);
            }
            // SAFETY: publication pairs this owner pointer with `remote`; its
            // gate excludes every overlapping runtime-derived mutable borrow.
            let mut cpu = remote.claim_local(ptr::with_exposed_provenance_mut::<CpuLocal>(raw))?;
            operation(cpu.as_pin_mut())
        })
    }
}

pub(crate) fn current_cpu_remote(cpu_pin: &CpuPin) -> Option<&'static CpuRemote> {
    let cpu = u32::try_from(ax_hal::percpu::this_cpu_id_pinned(cpu_pin)).ok()?;
    task_system()?.cpu_remote(CpuId::new(cpu))
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
    context.set_page_table_root(ax_memory_addr::PhysAddr::from(resolve_address_space_root(
        address_space,
    )));
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
        // SAFETY: callers retain the scheduler baton or an IRQ guard, and every
        // CPU publishes this immutable root before coming online.
        unsafe { with_current_cpu_pin(|pin| KERNEL_ADDRESS_SPACE_ROOT.read_current(pin)) }
    } else {
        // AArch64 and LoongArch have distinct kernel roots; zero disables the
        // lower/user translation root without disturbing kernel mappings.
        0
    }
}

#[cfg(feature = "uspace")]
fn set_current_context_page_table_root(root: ax_memory_addr::PhysAddr) -> Result<(), TaskError> {
    // SAFETY: the exec path holds local IRQ exclusion, so the current context
    // cannot migrate or overlap a scheduler handoff while its saved root is
    // replaced.
    unsafe {
        with_current_cpu_pin(|cpu_pin| {
            let context = current_runtime_context(cpu_pin).map_err(runtime_status_error)?;
            // SAFETY: the published current context exclusively owns its
            // architecture state until the next scheduler switch.
            (&mut *context.inner.get()).set_page_table_root(root);
            Ok(())
        })
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
    if context_ref.header.cpu_area().is_some() || context_ref.has_switch_tail() {
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
            let raw =
                unsafe { with_current_cpu_pin(|pin| CPU_LOCAL_OWNER_HANDLE.read_current(pin)) };
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

        fn publish_task_deadline(
            update: ax_task::runtime::TaskDeadlineUpdate,
        ) -> RuntimeStatus {
            #[cfg(feature = "irq")]
            {
                crate::publish_local_task_deadline(update)
            }
            #[cfg(not(feature = "irq"))]
            {
                let _ = update;
                RuntimeStatus::Unsupported
            }
        }

        fn send_scheduler_ipi(cpu: RuntimeCpuId) -> RuntimeStatus {
            #[cfg(any(feature = "ipi", feature = "wake-ipi"))]
            {
                let cpu_id = cpu.as_u32() as usize;
                if cpu_id >= ax_hal::cpu_num() {
                    return RuntimeStatus::InvalidArgument;
                }
                publish_then_notify_scheduler_ipi(
                    || publish_scheduler_ipi_doorbell(cpu_id),
                    || {
                        ax_hal::irq::send_ipi(
                            ax_hal::irq::ipi_irq(),
                            ax_hal::irq::IpiTarget::Other { cpu_id },
                        );
                    },
                )
            }
            #[cfg(not(any(feature = "ipi", feature = "wake-ipi")))]
            {
                let _ = cpu;
                RuntimeStatus::Unsupported
            }
        }

        fn wait_for_interrupt() {
            ax_hal::asm::disable_irqs();
            let now_ns = ax_hal::time::monotonic_time_nanos();
            let recovered_clockevent = crate::recover_overdue_local_clock_event(now_ns);
            let needs_reschedule = ax_task::current_cpu_needs_resched()
                .expect("idle handoff requires an initialized current CPU");
            if recovered_clockevent
                || needs_reschedule
                || crate::local_clock_event_has_immediate_work(now_ns)
            {
                ax_hal::asm::enable_irqs();
            } else {
                ax_hal::asm::wait_for_irqs_disabled();
            }
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
            if binding.identity.generation == 0 {
                return RuntimeStatus::InvalidArgument;
            }
            let thread_identity =
                ((binding.identity.generation as u64) << 32) | binding.identity.slot as u64;
            let Ok(context) = runtime_context(binding.context) else {
                return RuntimeStatus::InvalidHandle;
            };
            if context
                .thread_identity
                .compare_exchange(0, thread_identity, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return RuntimeStatus::InvalidArgument;
            }
            // Scheduler construction invokes this exactly once before the
            // context can enter a run queue. The bootstrap placeholder is
            // likewise not consumed by assembly until its first switch-out.
            unsafe { &mut *context.inner.get() }.set_current_header(context.header().as_non_null());
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
            let previous = ptr::with_exposed_provenance_mut::<RuntimeContext>(previous_raw);
            let next = ptr::with_exposed_provenance_mut::<RuntimeContext>(next_raw);
            // SAFETY: the active scheduler baton keeps local IRQs disabled for
            // preparation, publication, and the naked switch tail.
            unsafe {
                with_current_cpu_pin(|pin| {
                    let published_previous = current_runtime_context(pin).unwrap_or_else(|status| {
                        panic!("current runtime context is invalid: {status:?}")
                    });
                    assert!(
                        ptr::eq(published_previous, previous),
                        "scheduler previous context differs from the pinned current header"
                    );
                    // SAFETY: both handles stay live and are uniquely owned by
                    // the committed scheduler switch plan.
                    let previous_context = &*previous;
                    let next_context = &*next;
                    let previous_arch_context = &mut *previous_context.inner.get();
                    let next_arch_context = &*next_context.inner.get();
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
                    assert!(
                        previous_context.header.cpu_area() == Some(pin.area()),
                        "scheduler previous context is not bound to this CPU"
                    );
                    assert!(
                        next_context.header.cpu_area().is_none(),
                        "scheduler next context is already CPU-bound"
                    );
                    assert!(
                        !next_context.has_switch_tail(),
                        "scheduler next context retained an unfinished switch tail"
                    );

                    // All fallible CPU binding validation and FP/address-space
                    // preparation precede the irreversible baton transfer.
                    let (prepared, previous_binding) =
                        ax_hal::percpu::prepare_thread_switch(
                            pin,
                            previous_context.header(),
                            next_context.header(),
                        )
                        .unwrap_or_else(|error| {
                            panic!("failed to prepare runtime context switch: {error}")
                        });
                    previous_arch_context.prepare_switch_to(next_arch_context);
                    let tail = RuntimeSwitchTail {
                        previous: previous_context.header().as_non_null(),
                        binding: previous_binding,
                    };
                    next_context.stage_switch_tail(tail).unwrap_or_else(|status| {
                        panic!("failed to stage runtime switch tail: {status:?}")
                    });
                    crate::guard::transfer_scheduler_switch_baton();
                    // SAFETY: switch_to_prepared consumes the sole publication
                    // token and enters naked assembly without another fallible
                    // or ownership-sensitive Rust operation.
                    previous_arch_context.switch_to_prepared(next_arch_context, prepared);
                })
            };
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
mod tests;
