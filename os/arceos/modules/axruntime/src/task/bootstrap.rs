use super::*;

static TASK_SYSTEM: LazyInit<Pin<Box<TaskSystem>>> = LazyInit::new();

/// The already-running primary context is the unikernel's process owner.
///
/// Unlike a spawned runtime thread, it has no join record: returning from it
/// terminates the whole system. Retaining its generation-checked identity
/// keeps that role explicit instead of inferring it from a missing extension.
static PRIMARY_BOOTSTRAP_THREAD: LazyInit<PrimaryBootstrapThread> = LazyInit::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PrimaryBootstrapThread(ThreadId);

#[ax_percpu::def_percpu]
static CPU_LOCAL: LazyInit<Pin<Box<CpuLocal>>> = LazyInit::new();

/// Arc-backed scheduler endpoint cached before this CPU becomes online.
///
/// The endpoint is immutable and shutdown-live. Scheduler-adjacent current-CPU
/// reads reach it through the architecture current-thread register instead of
/// resolving a logical CPU through the global task-system registry.
#[ax_percpu::def_percpu]
static CPU_REMOTE_HANDLE: LazyInit<usize> = LazyInit::new();

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

/// Runs one CPU-local operation under the caller's existing migration guard.
///
/// # Safety
///
/// The caller must prevent migration for the complete callback. Runtime callers
/// use this only during offline CPU bring-up, hard IRQ handling, or while a
/// scheduler/IRQ guard owns the current CPU.
pub(super) unsafe fn with_current_cpu_pin<R>(
    operation: impl for<'scope> FnOnce(&CpuPin<'scope>) -> R,
) -> R {
    unsafe { ax_hal::percpu::with_cpu_pin(operation) }
        .unwrap_or_else(|error| panic!("task runtime CPU-local state is invalid: {error}"))
}

fn with_irq_cpu_pin<R>(operation: impl for<'scope> FnOnce(&CpuPin<'scope>) -> R) -> R {
    let _irq = IrqSave::new();
    // SAFETY: IrqSave excludes scheduler migration for the complete callback.
    unsafe { with_current_cpu_pin(operation) }
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
pub(super) enum IdleEntryAction {
    RetireBootstrap,
    RunIdle,
}

pub(super) fn idle_entry_action(
    current: Option<ThreadId>,
    idle: Option<ThreadId>,
) -> Result<IdleEntryAction, TaskError> {
    match (current, idle) {
        (Some(current), Some(idle)) if current == idle => Ok(IdleEntryAction::RunIdle),
        (Some(_), Some(_)) => Ok(IdleEntryAction::RetireBootstrap),
        _ => Err(TaskError::InvalidConfiguration),
    }
}

fn initialize_current_cpu(cpu_id: usize) -> Result<ThreadId, TaskError> {
    let system = task_system().ok_or(TaskError::NotInitialized)?;
    let cpu_id = u32::try_from(cpu_id).map_err(|_| TaskError::InvalidCpu(u32::MAX))?;
    let owner = CpuId::new(cpu_id);
    let remote_handle = system.runtime_cpu_remote_handle(owner).into_raw();
    if remote_handle == 0 {
        return Err(TaskError::InvalidCpu(cpu_id));
    }
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
                CPU_REMOTE_HANDLE.with_current_mut(exclusive, |slot| {
                    slot.init_once(remote_handle);
                });
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

pub(super) unsafe extern "C" fn idle_context_entry() -> ! {
    finish_initial_scheduler_switch();
    run_idle()
}

pub(super) fn task_system() -> Option<&'static TaskSystem> {
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

pub(super) fn with_current_cpu_local_mut_owner<R>(
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

pub(super) fn cpu_remote(cpu: RuntimeCpuId) -> Option<&'static CpuRemote> {
    task_system()?.cpu_remote(CpuId::new(cpu.as_u32()))
}

pub(super) fn current_cpu_local_owner_handle(cpu_pin: &CpuPin) -> usize {
    CPU_LOCAL_OWNER_HANDLE.read_current(cpu_pin)
}

/// Reads the current CPU's cached remote endpoint without constructing a pin.
///
/// # Safety
///
/// The caller must keep the scheduler-owned current thread alive and prevent
/// context switches and local IRQ re-entry for the complete observation.
pub(super) unsafe fn scheduler_current_cpu_remote_handle() -> usize {
    unsafe { CPU_REMOTE_HANDLE.with_scheduler_current(|slot| slot.get().copied().unwrap_or(0)) }
        .unwrap_or(0)
}

pub(super) fn primary_bootstrap_thread() -> Option<ThreadId> {
    PRIMARY_BOOTSTRAP_THREAD.get().map(|thread| thread.0)
}

#[cfg(feature = "uspace")]
pub(super) fn kernel_address_space_root(cpu_pin: &CpuPin) -> usize {
    KERNEL_ADDRESS_SPACE_ROOT.read_current(cpu_pin)
}
