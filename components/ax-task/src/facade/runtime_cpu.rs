use core::ptr::NonNull;

use super::*;

pub(crate) fn wake_thread_from_current_cpu(
    core: &Arc<ThreadCore>,
    intent: crate::WakeIntent,
) -> WakeResult {
    let Ok(system) = runtime_task_system() else {
        return WakeResult::Unavailable;
    };
    system.wake_thread_from_current_cpu(core, intent)
}

pub(crate) fn wake_wait_claim_from_task(
    core: &Arc<ThreadCore>,
    claim: &WaitWakeClaim,
    intent: crate::WakeIntent,
) -> WaitWakeDelivery {
    debug_assert!(!task_runtime::in_hard_irq());
    let Ok(system) = runtime_task_system() else {
        claim.cancel_selected();
        return WaitWakeDelivery::Unavailable;
    };
    system.wake_wait_claim_from_current_cpu(core, claim, intent)
}

pub(crate) fn runtime_task_system() -> Result<&'static TaskSystem, TaskError> {
    // SAFETY: the linked TaskRuntime provider is the platform trust root and
    // must publish only the pinned, shutdown-lifetime TaskSystem it owns.
    let handle = unsafe { task_runtime::task_system_handle() };
    task_system_from_handle(handle)
}

fn task_system_from_handle(
    handle: crate::runtime::TaskSystemHandle,
) -> Result<&'static TaskSystem, TaskError> {
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
    cpu_local: NonNull<CpuLocal>,
    cpu_remote: &'static CpuRemote,
}

impl RuntimeCpuHandles {
    fn capture() -> Self {
        // SAFETY: every capture is owned by a live RuntimeIrqGuard or
        // RuntimeSchedulerFrameGuard that prevents migration. The runtime
        // snapshots both paired endpoints from that one pinned CPU.
        let handles = unsafe { task_runtime::current_cpu_owner_handles() };
        // SAFETY: forwarded from the provider's current-CPU capability
        // contract and bounded by the caller's live CPU pin.
        unsafe { Self::from_snapshot(handles) }
    }

    unsafe fn from_snapshot(handles: crate::runtime::CurrentCpuOwnerHandles) -> Self {
        Self {
            // SAFETY: a successful runtime capability snapshot contains the
            // live, aligned owner endpoint for this exact CPU.
            cpu_local: unsafe {
                NonNull::new_unchecked(ptr::with_exposed_provenance_mut::<CpuLocal>(
                    handles.local().into_raw(),
                ))
            },
            // SAFETY: the paired remote endpoint is Arc-backed and remains
            // live until shutdown by the TaskRuntime contract.
            cpu_remote: unsafe {
                &*ptr::with_exposed_provenance::<CpuRemote>(handles.remote().into_raw())
            },
        }
    }

    const fn cpu_id(self) -> RuntimeCpuId {
        RuntimeCpuId::new(self.cpu_remote.owner().as_u32())
    }

    const fn remote(self) -> &'static CpuRemote {
        self.cpu_remote
    }

    fn claim(self) -> Result<CpuLocalOwnerBorrow<'static>, TaskError> {
        // SAFETY: capture ran under this guard's migration pin. The provider
        // guarantees that the pointer is the pinned CpuLocal paired with
        // `remote`; its owner gate excludes every overlapping mutable borrow.
        unsafe { self.cpu_remote.claim_local(self.cpu_local.as_ptr()) }
    }

    unsafe fn borrow_in_scheduler_frame(self) -> CpuLocalOwnerBorrow<'static> {
        // SAFETY: the caller owns the live IRQ-off scheduler frame that
        // captured and validated these paired handles and bounds the returned
        // borrow. A running owner CPU cannot become offline under that baton.
        unsafe {
            self.cpu_remote
                .borrow_local_in_scheduler_frame(self.cpu_local)
        }
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
        let token = crate::runtime::enter_irq_guard(crate::runtime::IrqGuardSource::RuntimeCpu);
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
    system: &'static TaskSystem,
    current: crate::runtime::CurrentThreadPublication,
    _not_send: PhantomData<*mut ()>,
}

impl runtime_cpu_pin_sealed::Sealed for RuntimeSchedulerFrameGuard {}
impl RuntimeCpuPin for RuntimeSchedulerFrameGuard {
    fn claim_current_cpu(&mut self) -> Result<CpuLocalOwnerBorrow<'static>, TaskError> {
        // SAFETY: this object owns the runtime's IRQ-off scheduler baton, and
        // the returned borrow is lifetime-bound to its mutable borrow.
        Ok(unsafe { self.cpu.borrow_in_scheduler_frame() })
    }
}

impl RuntimeSchedulerFrameGuard {
    pub(super) fn enter(
        origin: RuntimeScheduleOrigin,
        entry: RuntimeSchedulerEntry,
    ) -> Result<Self, TaskError> {
        let context = task_runtime::scheduler_frame_guard_enter(origin, entry);
        let status = context.status();
        if status != RuntimeStatus::Success {
            return Err(TaskError::UnsafeContext);
        }
        let system = task_system_from_handle(context.system()).unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x5254_0001, context.system().into_raw())
        });
        let return_to = match entry {
            RuntimeSchedulerEntry::Task
            | RuntimeSchedulerEntry::PreemptExit
            | RuntimeSchedulerEntry::IrqGuardExit => RuntimeSchedulerReturn::Task,
            RuntimeSchedulerEntry::IrqReturn | RuntimeSchedulerEntry::IrqReturnContinuation => {
                RuntimeSchedulerReturn::IrqReturn
            }
        };
        Ok(Self {
            return_to,
            // SAFETY: success from scheduler-frame entry carries the provider's
            // complete current-CPU capability under the acquired baton.
            cpu: unsafe { RuntimeCpuHandles::from_snapshot(context.cpu()) },
            system,
            current: context.current(),
            _not_send: PhantomData,
        })
    }

    pub(super) fn refresh_current_cpu(&mut self) {
        // A saved scheduler continuation normally resumes on the same rq. Like
        // Linux's switch tail retaining its rq pointer, keep using the handles
        // already validated by this scheduler frame unless the task actually
        // migrated while it was off-CPU.
        // SAFETY: the scheduler baton still pins this resumed continuation to
        // the CPU being compared. Both cached handles remain live until runtime
        // shutdown and are only borrowed again after this identity check.
        let current = unsafe { task_runtime::current_cpu_id() };
        if current != self.cpu.cpu_id() {
            self.cpu = RuntimeCpuHandles::capture();
        }
    }

    pub(super) const fn cpu_id(&self) -> RuntimeCpuId {
        self.cpu.cpu_id()
    }

    pub(super) const fn task_system(&self) -> &'static TaskSystem {
        self.system
    }

    pub(super) const fn current_thread_publication(
        &self,
    ) -> crate::runtime::CurrentThreadPublication {
        self.current
    }

    pub(super) fn current_thread_ref(&self) -> Result<CurrentThreadRef, TaskError> {
        // SAFETY: the runtime captured this publication while atomically
        // claiming the scheduler baton. The returned non-Send capability stays
        // within the synchronous lifetime of this frame.
        unsafe { self.current.borrow_current() }
    }

    /// Tests the scheduler request published for this frame's pinned CPU.
    ///
    /// Like Linux `need_resched()`, this reads the remotely publishable atomic
    /// state without reacquiring the owner runqueue. The scheduler frame keeps
    /// the CPU fixed, and `RuntimeCpuHandles::remote` revalidates that the
    /// cached endpoint still names that CPU.
    pub(super) fn scheduler_request_pending(
        &self,
        scope: SchedulerRequestScope,
    ) -> Result<bool, TaskError> {
        Ok(self.cpu.remote().scheduler_request_pending(scope))
    }
}

impl Drop for RuntimeSchedulerFrameGuard {
    fn drop(&mut self) {
        let needs_reschedule = self.cpu.remote().needs_immediate_scheduler_work();
        let _task_context_safe =
            task_runtime::scheduler_frame_guard_exit(self.return_to, needs_reschedule);
    }
}
