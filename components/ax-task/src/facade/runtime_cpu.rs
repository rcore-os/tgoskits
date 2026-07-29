use super::*;

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
    let irq = RuntimeIrqGuard::enter();
    let cpu = claim_runtime_current_cpu()?;
    Ok(RuntimeCurrentCpu { cpu, _irq: irq })
}

mod runtime_cpu_pin_sealed {
    pub trait Sealed {}
}

pub(crate) trait RuntimeCpuPin: runtime_cpu_pin_sealed::Sealed {}

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
    _pin: &'pin mut impl RuntimeCpuPin,
) -> Result<RuntimeCpuOwnerBorrow<'pin>, TaskError> {
    Ok(RuntimeCpuOwnerBorrow {
        cpu: claim_runtime_current_cpu()?,
        _pin: PhantomData,
    })
}

fn claim_runtime_current_cpu() -> Result<CpuLocalOwnerBorrow<'static>, TaskError> {
    let runtime_cpu = task_runtime::current_cpu_id();
    let remote = cpu_local_for_wake(crate::CpuId::new(runtime_cpu.as_u32()))
        .ok_or(TaskError::NotInitialized)?;
    // SAFETY: callers acquire an IRQ pin or scheduler-frame baton before this
    // helper and retain it for the complete lifetime of the returned claim.
    let handle = unsafe { task_runtime::current_cpu_local_handle() };
    let raw = handle.into_raw();
    validate_handle::<CpuLocal>(raw)?;
    // SAFETY: TaskRuntime guarantees this address identifies the current CPU's
    // pinned shutdown-lifetime CpuLocal. The separately allocated remote gate
    // is claimed before a reference to that allocation is reconstructed.
    let cpu = unsafe { remote.claim_local(ptr::with_exposed_provenance_mut::<CpuLocal>(raw))? };
    validate_cpu_owner(&cpu)?;
    Ok(cpu)
}

pub(crate) fn cpu_local_for_wake(cpu: crate::CpuId) -> Option<&'static CpuRemote> {
    // SAFETY: the linked runtime guarantees that this typed endpoint is the
    // Arc-backed CpuRemote for `cpu` and keeps it alive until shutdown.
    let handle =
        unsafe { task_runtime::cpu_remote_handle(crate::runtime::RuntimeCpuId::new(cpu.as_u32())) };
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

fn validate_cpu_owner(cpu: &CpuLocal) -> Result<(), TaskError> {
    let actual = task_runtime::current_cpu_id().as_u32();
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

pub(crate) struct RuntimeIrqGuard {
    token: IrqGuardToken,
    _not_send: PhantomData<*mut ()>,
}

impl RuntimeIrqGuard {
    pub(crate) fn enter() -> Self {
        Self {
            token: task_runtime::irq_guard_enter(),
            _not_send: PhantomData,
        }
    }
}

impl runtime_cpu_pin_sealed::Sealed for RuntimeIrqGuard {}
impl RuntimeCpuPin for RuntimeIrqGuard {}

impl Drop for RuntimeIrqGuard {
    fn drop(&mut self) {
        // SAFETY: this guard consumes its same-CPU token exactly once.
        unsafe { task_runtime::irq_guard_exit(self.token) };
    }
}

pub(super) struct RuntimeSchedulerFrameGuard {
    return_to: RuntimeSchedulerReturn,
    _not_send: PhantomData<*mut ()>,
}

impl runtime_cpu_pin_sealed::Sealed for RuntimeSchedulerFrameGuard {}
impl RuntimeCpuPin for RuntimeSchedulerFrameGuard {}

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
            RuntimeSchedulerEntry::Task | RuntimeSchedulerEntry::PreemptExit => {
                RuntimeSchedulerReturn::Task
            }
            RuntimeSchedulerEntry::IrqReturn => RuntimeSchedulerReturn::IrqReturn,
        };
        Ok(Self {
            return_to,
            _not_send: PhantomData,
        })
    }
}

impl Drop for RuntimeSchedulerFrameGuard {
    fn drop(&mut self) {
        let _task_context_safe = task_runtime::scheduler_frame_guard_exit(self.return_to);
    }
}
