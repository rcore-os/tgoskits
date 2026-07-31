//! Trait-FFI runtime stubs linked only into the ax-net unit-test binary.

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_task::{
    CpuId, CpuRemote, TaskSystem, impl_trait as impl_task_runtime,
    runtime::{TaskRuntime, *},
};

static NEXT_IRQ_TOKEN: AtomicUsize = AtomicUsize::new(1);
static TASK_SYSTEM: AtomicUsize = AtomicUsize::new(0);
static CPU_LOCAL: AtomicUsize = AtomicUsize::new(0);
static CPU_REMOTE: AtomicUsize = AtomicUsize::new(0);
static TEST_RUNTIME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct NetTestTaskRuntime;

impl_task_runtime! {
    impl TaskRuntime for NetTestTaskRuntime {
        unsafe fn task_system_handle() -> TaskSystemHandle {
            // SAFETY: the test guard keeps the pointed-to system alive.
            unsafe { TaskSystemHandle::from_raw(TASK_SYSTEM.load(Ordering::Acquire)) }
        }
        unsafe fn current_cpu_local_handle() -> CurrentCpuLocalHandle {
            // SAFETY: the test guard keeps the pinned CPU-local state alive.
            unsafe { CurrentCpuLocalHandle::from_raw(CPU_LOCAL.load(Ordering::Acquire)) }
        }
        unsafe fn current_cpu_remote_handle() -> CpuRemoteHandle {
            // SAFETY: the test runtime retains the TaskSystem that owns this
            // cached endpoint until InstalledTestRuntime is dropped.
            unsafe { CpuRemoteHandle::from_raw(CPU_REMOTE.load(Ordering::Acquire)) }
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
        fn online_cpu_count() -> u32 { 1 }
        fn prepare_cpu_online(_cpu: RuntimeCpuId) -> RuntimeStatus { RuntimeStatus::Success }
        fn prepare_cpu_offline(_cpu: RuntimeCpuId) -> RuntimeStatus { RuntimeStatus::Success }
        fn irq_guard_enter() -> IrqGuardToken {
            // SAFETY: the monotonically issued token remains live until the
            // matching no-op test exit consumes its modeled guard scope.
            unsafe {
                IrqGuardToken::from_raw(NEXT_IRQ_TOKEN.fetch_add(1, Ordering::Relaxed))
            }
        }
        unsafe fn irq_guard_exit(_token: IrqGuardToken) {}

        fn local_scheduler_work_is_self_serviced() -> bool {
            false
        }
        fn finish_context_switch_tail() -> RuntimeStatus { RuntimeStatus::Success }
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
        fn monotonic_ns() -> u64 { ax_hal::time::monotonic_time_nanos() }
        fn timer_resolution_ns() -> u64 { 1 }
        fn publish_task_deadline(_update: TaskDeadlineUpdate) -> RuntimeStatus {
            RuntimeStatus::Success
        }
        fn send_scheduler_ipi(_cpu: RuntimeCpuId) -> RuntimeStatus { RuntimeStatus::Success }
        fn wait_for_interrupt() {}
        fn allocate_stack(_request: StackRequest) -> RuntimeHandleResult {
            RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
        }
        fn deallocate_stack(_stack: StackHandle) -> RuntimeStatus { RuntimeStatus::Unsupported }
        fn allocate_tls(_request: TlsRequest) -> RuntimeHandleResult {
            RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
        }
        fn deallocate_tls(_tls: TlsHandle) -> RuntimeStatus { RuntimeStatus::Unsupported }
        fn create_kernel_context(_request: KernelContextRequest) -> RuntimeHandleResult {
            RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
        }
        fn create_user_context(_request: UserContextRequest) -> RuntimeHandleResult {
            if _request.address_space.is_none() {
                RuntimeHandleResult::failure(RuntimeStatus::InvalidHandle)
            } else {
                RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
            }
        }
        fn bind_context_thread(_binding: ContextThreadBinding) -> RuntimeStatus {
            RuntimeStatus::Success
        }
        fn destroy_context(_context: ExecutionContextHandle) -> RuntimeStatus {
            RuntimeStatus::Unsupported
        }
        unsafe fn switch_context(
            _previous: ExecutionContextHandle,
            _next: ExecutionContextHandle,
        ) {
            panic!("ax-net unit tests do not switch scheduler contexts")
        }
        fn install_address_space(_address_space: AddressSpaceHandle) -> RuntimeStatus {
            RuntimeStatus::Unsupported
        }
        fn flush_tlb_local(_start: usize, _size: usize) {}
        fn trace_sched_switch(_record: SchedSwitchRecord) {}
        fn fatal_invariant(code: u32, argument: usize) -> ! {
            panic!("ax-net test scheduler invariant {code} failed with {argument:#x}")
        }
    }
}

pub(crate) struct InstalledTestRuntime {
    _lock: std::sync::MutexGuard<'static, ()>,
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
            identity: ThreadIdentityV1::new(0, 0),
        }),
        RuntimeStatus::Success
    );
}

struct NetTestKernelGuard;

#[ax_crate_interface::impl_interface]
impl ax_kernel_guard::KernelGuardIf for NetTestKernelGuard {
    fn disable_preempt() {}

    fn enable_preempt() {}
}
