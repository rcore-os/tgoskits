//! ArceOS ownership and trait-FFI glue for the OS-independent task system.

use alloc::{boxed::Box, string::String};
use core::{
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering},
};

use ax_hal::percpu::CpuPin;
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

mod context;
mod executor;
mod resources;
mod scheduler_events;
mod thread;
mod thread_resources;

pub use context::{
    TaskAddressSpace, diagnose_current_stack_guard_page_fault, switch_current_page_table,
};
use context::{
    bind_bootstrap_runtime_context, bind_runtime_context_thread, create_bootstrap_context,
    create_runtime_context, create_user_runtime_context, destroy_runtime_context,
    finish_runtime_context_switch_tail, install_runtime_address_space, switch_runtime_context,
};
pub use executor::{BlockOnError, block_on, block_on_timeout};
#[cfg(feature = "tls")]
use resources::runtime_tls_pointer;
use resources::{
    allocate_runtime_stack, allocate_runtime_tls, deallocate_runtime_stack, deallocate_runtime_tls,
};
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
pub use thread::{
    PreparedThread, ThreadOsExtensionBorrow, ThreadOsExtensionLease, current_os_extension,
    exit_current, join_thread, thread_os_extension, wait_thread,
};
use thread::{
    RUNTIME_THREAD_EXTENSION_OPS, RuntimeThreadData, finish_initial_scheduler_switch,
    release_transferred_extension, runtime_thread_entry,
};
#[cfg(test)]
use thread::{
    RuntimeExtensionKind, classify_runtime_extension, extension_data_after_releasing_lease,
};
#[cfg(all(test, feature = "tls"))]
use thread_resources::assemble_bootstrap_resources;
use thread_resources::{
    InitialContextState, create_bootstrap_resources, create_idle_resources, create_thread_resources,
};
#[cfg(test)]
use thread_resources::{
    ThreadResourceBackend, UnreleasedThreadResources, create_thread_resources_with,
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

#[cfg(not(feature = "fs"))]
const DEFAULT_TASK_STACK_SIZE: usize = 256 * 1024;

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
    Ok(PreparedThread::new(system, handle))
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

unsafe extern "C" fn idle_context_entry() -> ! {
    finish_initial_scheduler_switch();
    run_idle()
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

        fn validate_owner_cpu_context() -> RuntimeStatus {
            crate::guard::validate_owner_cpu_context()
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
            bind_runtime_context_thread(binding)
        }

        fn destroy_context(_context: ExecutionContextHandle) -> RuntimeStatus {
            destroy_runtime_context(_context)
        }

        unsafe fn switch_context(
            previous: ExecutionContextHandle,
            next: ExecutionContextHandle,
        ) {
            // SAFETY: the TaskRuntime contract passes the committed previous
            // and next handles under the active scheduler baton.
            unsafe { switch_runtime_context(previous, next) };
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
