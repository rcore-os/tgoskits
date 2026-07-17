//! Trait-FFI runtime stubs linked only into the ax-net unit-test binary.

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_kspin::{LockRuntime, LockdepEvent, impl_trait as impl_lock_runtime};
use ax_task::{
    impl_trait as impl_task_runtime,
    runtime::{TaskRuntime, *},
};

static NEXT_IRQ_TOKEN: AtomicUsize = AtomicUsize::new(1);

struct NetTestTaskRuntime;

impl_task_runtime! {
    impl TaskRuntime for NetTestTaskRuntime {
        unsafe fn task_system_handle() -> TaskSystemHandle { TaskSystemHandle::NONE }
        unsafe fn current_cpu_local_handle() -> CurrentCpuLocalHandle {
            CurrentCpuLocalHandle::NONE
        }
        unsafe fn cpu_remote_handle(_cpu: RuntimeCpuId) -> CpuRemoteHandle {
            CpuRemoteHandle::NONE
        }
        fn current_cpu_id() -> RuntimeCpuId { RuntimeCpuId::new(0) }
        fn online_cpu_count() -> u32 { 1 }
        fn irq_guard_enter() -> IrqGuardToken {
            // SAFETY: the monotonically issued token remains live until the
            // matching no-op test exit consumes its modeled guard scope.
            unsafe {
                IrqGuardToken::from_raw(NEXT_IRQ_TOKEN.fetch_add(1, Ordering::Relaxed))
            }
        }
        unsafe fn irq_guard_exit(_token: IrqGuardToken) {}
        fn finish_context_switch_tail() -> RuntimeStatus { RuntimeStatus::Success }
        fn finish_initial_context_switch() {}
        fn scheduler_frame_guard_enter(
            _origin: RuntimeScheduleOrigin,
            _entry: RuntimeSchedulerEntry,
        ) -> RuntimeStatus { RuntimeStatus::Success }
        fn scheduler_frame_guard_exit(return_to: RuntimeSchedulerReturn) -> bool {
            matches!(return_to, RuntimeSchedulerReturn::Task)
        }
        fn in_hard_irq() -> bool { false }
        fn validate_schedule_context(_origin: RuntimeScheduleOrigin) -> RuntimeStatus {
            RuntimeStatus::Success
        }
        fn monotonic_ns() -> u64 { ax_hal::time::monotonic_time_nanos() }
        fn timer_resolution_ns() -> u64 { 1 }
        fn program_oneshot_timer(_deadline_ns: u64) -> RuntimeStatus { RuntimeStatus::Success }
        fn dispatch_expired_timer(_event: RuntimeTimerEventV1) -> RuntimeStatus {
            RuntimeStatus::Unsupported
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

struct NetTestLockRuntime;

impl_lock_runtime! {
    impl LockRuntime for NetTestLockRuntime {
        fn irq_enter() {}
        fn irq_exit() {}
        fn preempt_enter() {}
        fn preempt_exit() {}
        unsafe fn preempt_exit_irq_return() {}
        fn current_thread_id() -> u64 { 0 }
        fn lockdep_acquire(_event: LockdepEvent) {}
        fn lockdep_release(_event: LockdepEvent) {}
        fn lockdep_set_trace_enabled(_enabled: bool) {}
        fn lockdep_dump_trace() {}
    }
}
