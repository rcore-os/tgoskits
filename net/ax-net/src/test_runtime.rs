//! Trait-FFI runtime stubs linked only into the ax-net unit-test binary.

use alloc::boxed::Box;
use core::{
    cell::Cell,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_task::{
    CpuId, CpuRemote, SchedulePolicy, TaskSystem, TaskSystemConfig, ThreadSpec,
    impl_trait as impl_task_runtime,
    runtime::{TaskRuntime, *},
};

static NEXT_IRQ_TOKEN: AtomicUsize = AtomicUsize::new(1);
static NEXT_PREEMPT_TOKEN: AtomicUsize = AtomicUsize::new(1);
static TASK_SYSTEM: AtomicUsize = AtomicUsize::new(0);
static CPU_LOCAL: AtomicUsize = AtomicUsize::new(0);
static CPU_REMOTE: AtomicUsize = AtomicUsize::new(0);
static TEST_RUNTIME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

std::thread_local! {
    static ACTIVE_PREEMPT_GUARDS: Cell<usize> = const { Cell::new(0) };
    static LOCAL_IRQ_ENABLED: Cell<bool> = const { Cell::new(true) };
}

struct NetTestTaskRuntime;

impl_task_runtime! {
    impl TaskRuntime for NetTestTaskRuntime {
        unsafe fn task_system_handle() -> TaskSystemHandle {
            // SAFETY: the test guard keeps the pointed-to system alive.
            unsafe { TaskSystemHandle::from_raw(TASK_SYSTEM.load(Ordering::Acquire)) }
        }
        unsafe fn current_cpu_owner_handles() -> CurrentCpuOwnerHandles {
            let local = CPU_LOCAL.load(Ordering::Acquire);
            let remote = CPU_REMOTE.load(Ordering::Acquire);
            // SAFETY: InstalledTestRuntime publishes the paired handles for
            // modeled CPU 0 and keeps their TaskSystem alive.
            unsafe {
                CurrentCpuOwnerHandles::new(
                    RuntimeCpuId::new(0),
                    CurrentCpuLocalHandle::from_raw(local),
                    CpuRemoteHandle::from_raw(remote),
                )
            }
        }
        unsafe fn current_cpu_remote_handle() -> CpuRemoteHandle {
            // SAFETY: the test runtime retains the TaskSystem that owns this
            // cached endpoint until InstalledTestRuntime is dropped.
            unsafe { CpuRemoteHandle::from_raw(CPU_REMOTE.load(Ordering::Acquire)) }
        }
        fn current_thread_publication() -> CurrentThreadPublication {
            let raw = CPU_REMOTE.load(Ordering::Acquire);
            if raw == 0 {
                return CurrentThreadPublication::NONE;
            }
            // SAFETY: InstalledTestRuntime retains the TaskSystem that owns
            // this endpoint until the modeled current identity is no longer
            // observable.
            let remote = unsafe { &*core::ptr::with_exposed_provenance::<CpuRemote>(raw) };
            let Some(id) = remote.current_thread() else {
                return CurrentThreadPublication::NONE;
            };
            let system = unsafe {
                &*core::ptr::with_exposed_provenance::<TaskSystem>(TASK_SYSTEM.load(Ordering::Acquire))
            };
            system
                .thread_handle(id)
                .map_or(CurrentThreadPublication::NONE, |thread| thread.runtime_publication())
        }
        unsafe fn cpu_remote_handle(cpu: RuntimeCpuId) -> CpuRemoteHandle {
            let raw = TASK_SYSTEM.load(Ordering::Acquire);
            if raw == 0 {
                return CpuRemoteHandle::NONE;
            }
            // SAFETY: the installed test guard owns the TaskSystem.
            let system = unsafe { &*core::ptr::with_exposed_provenance::<TaskSystem>(raw) };
            system
                .cpu_remote(CpuId::new(cpu.as_u32()))
                .map_or(CpuRemoteHandle::NONE, |remote| {
                    // SAFETY: the TaskSystem owns this endpoint until clear.
                    unsafe {
                        CpuRemoteHandle::from_raw(
                            (remote as *const CpuRemote).expose_provenance(),
                        )
                    }
                })
        }
        unsafe fn current_cpu_id() -> RuntimeCpuId { RuntimeCpuId::new(0) }
        fn prepare_cpu_online(_cpu: RuntimeCpuId) -> RuntimeStatus { RuntimeStatus::Success }
        fn prepare_cpu_offline(_cpu: RuntimeCpuId) -> RuntimeStatus { RuntimeStatus::Success }
        fn local_irq_save_and_disable() -> LocalIrqState {
            let was_enabled = LOCAL_IRQ_ENABLED.replace(false);
            // SAFETY: the test runtime accepts this encoded boolean in its
            // matching restore operation.
            unsafe { LocalIrqState::from_raw(usize::from(was_enabled)) }
        }
        unsafe fn local_irq_restore(state: LocalIrqState) {
            LOCAL_IRQ_ENABLED.set(state.into_raw() != 0);
        }
        fn irq_guard_enter() -> IrqGuardToken {
            // SAFETY: the monotonically issued token remains live until the
            // matching no-op test exit consumes its modeled guard scope.
            unsafe {
                IrqGuardToken::from_raw(NEXT_IRQ_TOKEN.fetch_add(1, Ordering::Relaxed))
            }
        }
        unsafe fn irq_guard_exit(_token: IrqGuardToken) {}

        fn preempt_guard_enter() -> PreemptGuardToken {
            // SAFETY: the monotonically issued token remains live until the
            // matching no-op test exit consumes its modeled guard scope.
            unsafe {
                PreemptGuardToken::from_raw(NEXT_PREEMPT_TOKEN.fetch_add(1, Ordering::Relaxed))
            }
        }
        unsafe fn preempt_guard_exit(_token: PreemptGuardToken) {}
        unsafe fn preempt_guard_exit_irq_return(_token: PreemptGuardToken) {}
        fn hardirq_enter() {}
        fn hardirq_exit() {}

        fn publish_local_scheduler_work() -> bool {
            false
        }
        fn finish_context_switch_tail() {}
        fn finish_initial_context_switch() {}
        fn scheduler_frame_guard_enter(
            _origin: RuntimeScheduleOrigin,
            _entry: RuntimeSchedulerEntry,
        ) -> RuntimeStatus { RuntimeStatus::Success }
        fn scheduler_frame_guard_exit(_return_to: RuntimeSchedulerReturn) -> bool { true }
        fn in_hard_irq() -> bool { false }
        fn validate_schedule_context(_origin: RuntimeScheduleOrigin) -> RuntimeStatus {
            RuntimeStatus::Success
        }
        fn validate_owner_cpu_context() -> RuntimeStatus { RuntimeStatus::Success }
        fn monotonic_now() -> ax_task::runtime::MonotonicInstant {
            ax_task::runtime::MonotonicInstant::from_nanos(
                ax_hal::time::monotonic_time_nanos(),
            )
            .expect("platform monotonic clock exceeded the ktime domain")
        }
        fn rq_clock_sample(_cpu: RuntimeCpuId) -> RqClockSample {
            RqClockSample::new(
                ax_task::SchedulerTimestamp::from_nanos(ax_hal::time::monotonic_time_nanos()),
                0,
            )
        }
        fn publish_scheduler_deadline(_update: SchedulerDeadlineUpdate) {}
        fn notify_scheduler_cpu(_cpu: RuntimeCpuId) -> RuntimeStatus {
            RuntimeStatus::Success
        }
        fn wait_for_interrupt() {}
        fn allocate_stack(_request: StackRequest) -> RuntimeHandleResult {
            RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
        }
        fn deallocate_stack(_stack: StackHandle) {}
        fn allocate_tls(_request: TlsRequest) -> RuntimeHandleResult {
            RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
        }
        fn deallocate_tls(_tls: TlsHandle) {}
        fn create_kernel_context(_request: KernelContextRequest) -> RuntimeHandleResult {
            RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
        }
        fn create_user_context(_request: UserContextRequest) -> RuntimeHandleResult {
            RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
        }
        fn bind_context_thread(_binding: ContextThreadBinding) -> RuntimeStatus {
            RuntimeStatus::Success
        }
        fn destroy_context(_context: ExecutionContextHandle) {}
        fn destroy_address_space(
            _address_space: AddressSpaceHandle,
        ) -> AddressSpaceDestroyOutcome {
            panic!("ax-net unit tests do not own address-space tokens")
        }
        fn arm_address_space_reclaim(
            _address_space: AddressSpaceHandle,
        ) -> AddressSpaceReclaimArmOutcome {
            panic!("ax-net unit tests do not own address-space tokens")
        }
        fn address_space_membarrier_state(
            _address_space: AddressSpaceHandle,
        ) -> AddressSpaceMembarrierState {
            panic!("ax-net unit tests do not own address-space tokens")
        }
        fn update_address_space_membarrier_state(
            _address_space: AddressSpaceHandle,
            _registration: MembarrierRegistration,
            _phase: MembarrierRegistrationPhase,
        ) -> AddressSpaceMembarrierState {
            panic!("ax-net unit tests do not own address-space tokens")
        }
        fn synchronize_membarrier_cpu(
            _cpu: RuntimeCpuId,
            action: RuntimeMembarrierAction,
        ) -> RuntimeStatus {
            if action == RuntimeMembarrierAction::MemoryBarrier {
                core::sync::atomic::fence(Ordering::SeqCst);
            }
            RuntimeStatus::Success
        }
        unsafe fn switch_context(_switch: ContextSwitch) {
            panic!("ax-net unit tests do not switch scheduler contexts")
        }
        fn activate_address_space(_activation: AddressSpaceActivation) -> RuntimeStatus {
            RuntimeStatus::Unsupported
        }
        fn flush_tlb_local(_start: usize, _size: usize) {}
        fn trace_sched_switch(_record: SchedSwitchRecord) {}
        fn emergency_console_write(_message: &str) {}
        fn fatal_invariant(code: u32, argument: usize) -> ! {
            panic!("ax-net test scheduler invariant {code} failed with {argument:#x}")
        }
    }
}

pub(crate) struct InstalledTestRuntime {
    _lock: std::sync::MutexGuard<'static, ()>,
}

pub(crate) struct OwnedTestRuntime {
    _installed: InstalledTestRuntime,
    _cpu_local: Pin<Box<ax_task::CpuLocal>>,
    _task_system: Box<TaskSystem>,
}

pub(crate) fn install_default() -> OwnedTestRuntime {
    let task_system = Box::new(TaskSystem::new(TaskSystemConfig::new(1)).unwrap());
    let mut cpu_local = task_system.create_cpu_local(CpuId::new(0)).unwrap();
    task_system
        .install_bootstrap_thread(
            cpu_local.as_mut(),
            ThreadSpec::new(SchedulePolicy::default()),
        )
        .unwrap();
    task_system.bring_cpu_online(cpu_local.as_mut()).unwrap();
    let installed = install(&task_system, cpu_local.as_mut());
    OwnedTestRuntime {
        _installed: installed,
        _cpu_local: cpu_local,
        _task_system: task_system,
    }
}

impl Drop for InstalledTestRuntime {
    fn drop(&mut self) {
        CPU_REMOTE.store(0, Ordering::Release);
        CPU_LOCAL.store(0, Ordering::Release);
        TASK_SYSTEM.store(0, Ordering::Release);
    }
}

pub(crate) fn install(
    task_system: &TaskSystem,
    cpu_local: core::pin::Pin<&mut ax_task::CpuLocal>,
) -> InstalledTestRuntime {
    let lock = TEST_RUNTIME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    TASK_SYSTEM.store(
        (task_system as *const TaskSystem).expose_provenance(),
        Ordering::Release,
    );
    CPU_LOCAL.store(
        (cpu_local.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        Ordering::Release,
    );
    CPU_REMOTE.store(
        task_system
            .runtime_cpu_remote_handle(CpuId::new(0))
            .into_raw(),
        Ordering::Release,
    );
    InstalledTestRuntime { _lock: lock }
}

#[test]
fn pure_model_exports_the_context_binding_symbol() {
    assert_eq!(
        ax_task::runtime::task_runtime::bind_context_thread(ContextThreadBinding {
            context: ExecutionContextHandle::NONE,
            publication: CurrentThreadPublication::NONE,
        }),
        RuntimeStatus::Success
    );
}

pub(crate) fn reset_preempt_guards() {
    ACTIVE_PREEMPT_GUARDS.with(|depth| depth.set(0));
}

pub(crate) fn active_preempt_guards() -> usize {
    ACTIVE_PREEMPT_GUARDS.with(Cell::get)
}
