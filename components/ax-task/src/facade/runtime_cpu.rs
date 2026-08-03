use super::*;

pub(crate) fn wake_thread_direct(core: &Arc<ThreadCore>, target: crate::CpuId) -> WakeResult {
    let Ok(system) = runtime_task_system() else {
        return WakeResult::Unavailable;
    };
    system.wake_thread_direct(Arc::clone(core), target, task_runtime::monotonic_ns())
}

pub(crate) fn runtime_task_system() -> Result<&'static TaskSystem, TaskError> {
    // SAFETY: the linked TaskRuntime provider is the platform trust root and
    // must publish only the pinned, shutdown-lifetime TaskSystem it owns.
    let handle = unsafe { task_runtime::task_system_handle() };
    let raw = handle.into_raw();
    validate_handle::<TaskSystem>(raw)?;
    // SAFETY: TaskRuntime requires this handle to identify a pinned TaskSystem
    // that remains live until shutdown. The scheduler's mutable state is behind
    // its internal IRQ ticket lock, so creating this shared reference aliases no
    // unprotected mutable access.
    Ok(unsafe { &*ptr::with_exposed_provenance::<TaskSystem>(raw) })
}

pub(super) struct RuntimeCurrentCpu {
    cpu: CpuLocalOwnerBorrow<'static>,
    _irq: RuntimeIrqGuard,
}

impl Deref for RuntimeCurrentCpu {
    type Target = CpuLocal;

    fn deref(&self) -> &Self::Target {
        &self.cpu
    }
}

pub(super) fn runtime_current_cpu() -> Result<RuntimeCurrentCpu, TaskError> {
    let mut irq = RuntimeIrqGuard::enter();
    let cpu = irq.claim_current_cpu()?;
    Ok(RuntimeCurrentCpu { cpu, _irq: irq })
}

mod runtime_cpu_pin_sealed {
    pub trait Sealed {}
}

pub(crate) trait RuntimeCpuPin: runtime_cpu_pin_sealed::Sealed {
    fn claim_current_cpu(&mut self) -> Result<CpuLocalOwnerBorrow<'static>, TaskError>;
}

#[derive(Clone, Copy)]
struct RuntimeCpuHandles {
    runtime_cpu: RuntimeCpuId,
    cpu_local: crate::runtime::CurrentCpuLocalHandle,
    cpu_remote: crate::runtime::CpuRemoteHandle,
}

impl RuntimeCpuHandles {
    fn capture() -> Self {
        // SAFETY: every capture is owned by a live RuntimeIrqGuard or
        // RuntimeSchedulerFrameGuard that prevents migration.
        let runtime_cpu = unsafe { task_runtime::current_cpu_id() };
        // SAFETY: a RuntimeCpuPin is created only after its runtime guard has
        // disabled migration. The provider returns shutdown-lifetime handles
        // for the CPU selected by that same pinned context.
        let cpu_remote = unsafe { task_runtime::cpu_remote_handle(runtime_cpu) };
        // SAFETY: the same guard pins the current CPU while the runtime reads
        // and validates its architecture-owned CPU-local registers.
        let cpu_local = unsafe { task_runtime::current_cpu_local_handle() };
        Self {
            runtime_cpu,
            cpu_local,
            cpu_remote,
        }
    }

    fn claim(self) -> Result<CpuLocalOwnerBorrow<'static>, TaskError> {
        let remote_raw = self.cpu_remote.into_raw();
        validate_handle::<CpuRemote>(remote_raw)?;
        // SAFETY: TaskRuntime guarantees this handle identifies the Arc-backed
        // remote endpoint retained by the task system until shutdown.
        let remote = unsafe { &*ptr::with_exposed_provenance::<CpuRemote>(remote_raw) };
        if !remote.is_online() {
            return Err(TaskError::NotInitialized);
        }

        let local_raw = self.cpu_local.into_raw();
        validate_handle::<CpuLocal>(local_raw)?;
        // SAFETY: capture ran under this guard's migration pin. The provider
        // guarantees that the pointer is the pinned CpuLocal paired with
        // `remote`; its owner gate excludes every overlapping mutable borrow.
        let cpu =
            unsafe { remote.claim_local(ptr::with_exposed_provenance_mut::<CpuLocal>(local_raw))? };
        validate_cpu_owner(&cpu, self.runtime_cpu)?;
        Ok(cpu)
    }
}

pub(crate) struct RuntimeCpuOwnerBorrow<'pin> {
    cpu: CpuLocalOwnerBorrow<'static>,
    _pin: PhantomData<&'pin mut ()>,
}

impl RuntimeCpuOwnerBorrow<'_> {
    pub(crate) fn as_mut(&mut self) -> Pin<&mut CpuLocal> {
        self.cpu.as_pin_mut()
    }
}

impl Deref for RuntimeCpuOwnerBorrow<'_> {
    type Target = CpuLocal;

    fn deref(&self) -> &Self::Target {
        &self.cpu
    }
}

pub(crate) fn runtime_current_cpu_mut<'pin>(
    pin: &'pin mut impl RuntimeCpuPin,
) -> Result<RuntimeCpuOwnerBorrow<'pin>, TaskError> {
    Ok(RuntimeCpuOwnerBorrow {
        cpu: pin.claim_current_cpu()?,
        _pin: PhantomData,
    })
}

pub(crate) fn current_cpu_remote() -> Option<&'static CpuRemote> {
    // SAFETY: callers of the current-CPU facade retain migration exclusion.
    // The linked runtime publishes the current CPU's shutdown-lifetime remote
    // endpoint directly, without resolving it through the global registry.
    let handle = unsafe { task_runtime::current_cpu_remote_handle() };
    cpu_remote_from_handle(handle)
}

fn cpu_remote_from_handle(handle: crate::runtime::CpuRemoteHandle) -> Option<&'static CpuRemote> {
    let raw = handle.into_raw();
    if validate_handle::<CpuRemote>(raw).is_err() {
        return None;
    }
    // SAFETY: TaskRuntime guarantees every remote endpoint is Arc-backed and
    // remains live until shutdown. It contains no owner-only runqueue state.
    let cpu = unsafe { &*ptr::with_exposed_provenance::<CpuRemote>(raw) };
    cpu.is_online().then_some(cpu)
}

fn validate_handle<T>(raw: usize) -> Result<(), TaskError> {
    if raw == 0 {
        Err(TaskError::NotInitialized)
    } else if !raw.is_multiple_of(align_of::<T>()) {
        Err(TaskError::InvalidRuntimeHandle)
    } else {
        Ok(())
    }
}

fn validate_cpu_owner(cpu: &CpuLocal, runtime_cpu: RuntimeCpuId) -> Result<(), TaskError> {
    let actual = runtime_cpu.as_u32();
    let expected = cpu.owner().as_u32();
    if actual == expected {
        Ok(())
    } else {
        Err(TaskError::CpuOwnerMismatch { expected, actual })
    }
}

pub(super) fn validate_schedule_context(origin: RuntimeScheduleOrigin) -> Result<(), TaskError> {
    match task_runtime::validate_schedule_context(origin) {
        RuntimeStatus::Success => Ok(()),
        RuntimeStatus::UnsafeContext => Err(TaskError::UnsafeContext),
        status => Err(TaskError::RuntimeFailure(status as u32)),
    }
}

pub(super) fn validate_task_context() -> Result<(), TaskError> {
    if task_runtime::in_hard_irq() {
        Err(TaskError::UnsafeContext)
    } else {
        Ok(())
    }
}

pub(crate) struct RuntimeIrqGuard {
    token: IrqGuardToken,
    cpu: RuntimeCpuHandles,
    _not_send: PhantomData<*mut ()>,
}

impl RuntimeIrqGuard {
    pub(crate) fn enter() -> Self {
        let token = task_runtime::irq_guard_enter();
        Self {
            token,
            cpu: RuntimeCpuHandles::capture(),
            _not_send: PhantomData,
        }
    }
}

impl runtime_cpu_pin_sealed::Sealed for RuntimeIrqGuard {}
impl RuntimeCpuPin for RuntimeIrqGuard {
    fn claim_current_cpu(&mut self) -> Result<CpuLocalOwnerBorrow<'static>, TaskError> {
        self.cpu.claim()
    }
}

impl Drop for RuntimeIrqGuard {
    fn drop(&mut self) {
        // SAFETY: this guard consumes its same-CPU token exactly once.
        unsafe { task_runtime::irq_guard_exit(self.token) };
    }
}

pub(super) struct RuntimeSchedulerFrameGuard {
    return_to: RuntimeSchedulerReturn,
    cpu: RuntimeCpuHandles,
    _not_send: PhantomData<*mut ()>,
}

impl runtime_cpu_pin_sealed::Sealed for RuntimeSchedulerFrameGuard {}
impl RuntimeCpuPin for RuntimeSchedulerFrameGuard {
    fn claim_current_cpu(&mut self) -> Result<CpuLocalOwnerBorrow<'static>, TaskError> {
        self.cpu.claim()
    }
}

impl RuntimeSchedulerFrameGuard {
    pub(super) fn enter(
        origin: RuntimeScheduleOrigin,
        entry: RuntimeSchedulerEntry,
    ) -> Result<Self, TaskError> {
        let status = task_runtime::scheduler_frame_guard_enter(origin, entry);
        if status != RuntimeStatus::Success {
            return Err(match status {
                RuntimeStatus::UnsafeContext => TaskError::UnsafeContext,
                status => TaskError::RuntimeFailure(status as u32),
            });
        }
        let return_to = match entry {
            RuntimeSchedulerEntry::Task
            | RuntimeSchedulerEntry::PreemptExit
            | RuntimeSchedulerEntry::IrqGuardExit => RuntimeSchedulerReturn::Task,
            RuntimeSchedulerEntry::IrqReturn => RuntimeSchedulerReturn::IrqReturn,
        };
        Ok(Self {
            return_to,
            cpu: RuntimeCpuHandles::capture(),
            _not_send: PhantomData,
        })
    }

    pub(super) fn refresh_current_cpu(&mut self) {
        // A saved scheduler continuation may resume after its task migrated.
        // Capture the target CPU once before switch-tail owner access; all
        // later borrows in this frame reuse that validated identity.
        self.cpu = RuntimeCpuHandles::capture();
    }
}

impl Drop for RuntimeSchedulerFrameGuard {
    fn drop(&mut self) {
        let _task_context_safe = task_runtime::scheduler_frame_guard_exit(self.return_to);
    }
}
