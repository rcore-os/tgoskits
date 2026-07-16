//! Per-unit-test-binary task and lock runtime symbols.

#[cfg(feature = "lockdep")]
use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_kspin::{LockRuntime, LockdepEvent, impl_trait as impl_lock_runtime};
use ax_task::{
    CpuId, CpuRemote, TaskSystem, impl_trait as impl_task_runtime,
    runtime::{
        AddressSpaceHandle, ContextThreadBinding, CpuRemoteHandle, CurrentCpuLocalHandle,
        ExecutionContextHandle, IrqGuardToken, KernelContextRequest, RuntimeCpuId,
        RuntimeHandleResult, RuntimeScheduleOrigin, RuntimeSchedulerEntry, RuntimeSchedulerReturn,
        RuntimeStatus, SchedSwitchRecord, StackHandle, StackRequest, TaskRuntime, TaskSystemHandle,
        ThreadIdentityV1, TlsHandle, TlsRequest, UserContextRequest,
    },
};

struct UnitTestLockRuntime;
struct UnitTestTaskRuntime;

static TASK_SYSTEM: AtomicUsize = AtomicUsize::new(0);
static CPU_LOCAL: AtomicUsize = AtomicUsize::new(0);
static SCHEDULER_IPIS: AtomicUsize = AtomicUsize::new(0);
static LAST_SCHEDULER_IPI_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);
static PREEMPT_DEPTH: AtomicUsize = AtomicUsize::new(0);
static SCHEDULE_CONTEXT_SAFE: AtomicBool = AtomicBool::new(true);
static RUNTIME_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(feature = "lockdep")]
std::thread_local! {
    static HELD_LOCKS: RefCell<ax_lockdep::HeldLockStack> =
        const { RefCell::new(ax_lockdep::HeldLockStack::new()) };
}

impl_lock_runtime! {
    impl LockRuntime for UnitTestLockRuntime {
        fn irq_enter() {}
        fn irq_exit() {}
        fn preempt_enter() {
            PREEMPT_DEPTH.fetch_add(1, Ordering::AcqRel);
        }
        fn preempt_exit() {
            assert!(PREEMPT_DEPTH.fetch_sub(1, Ordering::AcqRel) > 0);
        }
        unsafe fn preempt_exit_irq_return() {
            assert!(PREEMPT_DEPTH.fetch_sub(1, Ordering::AcqRel) > 0);
        }
        fn current_thread_id() -> u64 { 1 }
        fn lockdep_acquire(_event: LockdepEvent) {}
        fn lockdep_release(_event: LockdepEvent) {}
        fn lockdep_set_trace_enabled(_enabled: bool) {}
        fn lockdep_dump_trace() {}
    }
}

impl_task_runtime! {
    impl TaskRuntime for UnitTestTaskRuntime {
        unsafe fn task_system_handle() -> TaskSystemHandle {
            // SAFETY: install/clear bracket the pinned fixture TaskSystem.
            unsafe { TaskSystemHandle::from_raw(TASK_SYSTEM.load(Ordering::Acquire)) }
        }
        unsafe fn current_cpu_local_handle() -> CurrentCpuLocalHandle {
            // SAFETY: install/clear bracket the pinned owner CpuLocal fixture.
            unsafe { CurrentCpuLocalHandle::from_raw(CPU_LOCAL.load(Ordering::Acquire)) }
        }
        unsafe fn cpu_remote_handle(cpu: RuntimeCpuId) -> CpuRemoteHandle {
            let raw = TASK_SYSTEM.load(Ordering::Acquire);
            if raw == 0 {
                return CpuRemoteHandle::NONE;
            }
            // SAFETY: install/clear keep the pinned fixture TaskSystem alive.
            let system = unsafe { &*core::ptr::with_exposed_provenance::<TaskSystem>(raw) };
            system
                .cpu_remote(CpuId::new(cpu.as_u32()))
                .map_or(CpuRemoteHandle::NONE, |remote| {
                    // SAFETY: TaskSystem owns the Arc-backed endpoint while the
                    // fixture handle remains installed.
                    unsafe {
                        CpuRemoteHandle::from_raw(
                            (remote as *const CpuRemote).expose_provenance(),
                        )
                    }
                })
        }
        fn current_cpu_id() -> RuntimeCpuId { RuntimeCpuId::new(0) }
        fn online_cpu_count() -> u32 { 1 }
        fn irq_guard_enter() -> IrqGuardToken {
            // SAFETY: this single-CPU test runtime models one balanced token.
            unsafe { IrqGuardToken::from_raw(1) }
        }
        unsafe fn irq_guard_exit(_token: IrqGuardToken) {}
        fn finish_context_switch_tail() -> RuntimeStatus { RuntimeStatus::Success }
        fn finish_initial_context_switch() {}
        fn scheduler_frame_guard_enter(
            _origin: RuntimeScheduleOrigin,
            _entry: RuntimeSchedulerEntry,
        ) -> RuntimeStatus { RuntimeStatus::Success }
        fn scheduler_frame_guard_exit(_return_to: RuntimeSchedulerReturn) -> bool { true }
        fn in_hard_irq() -> bool { false }
        fn validate_schedule_context(_origin: ax_task::runtime::RuntimeScheduleOrigin) -> RuntimeStatus {
            if SCHEDULE_CONTEXT_SAFE.load(Ordering::Acquire) {
                RuntimeStatus::Success
            } else {
                RuntimeStatus::UnsafeContext
            }
        }
        fn monotonic_ns() -> u64 { 0 }
        fn timer_resolution_ns() -> u64 { 1 }
        fn program_oneshot_timer(_deadline_ns: u64) -> RuntimeStatus { RuntimeStatus::Success }
        fn send_scheduler_ipi(cpu: RuntimeCpuId) -> RuntimeStatus {
            LAST_SCHEDULER_IPI_CPU.store(cpu.as_u32() as usize, Ordering::Release);
            SCHEDULER_IPIS.fetch_add(1, Ordering::AcqRel);
            RuntimeStatus::Success
        }
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
            panic!("unit-test runtime has no execution contexts")
        }
        fn install_address_space(_address_space: AddressSpaceHandle) -> RuntimeStatus {
            RuntimeStatus::Unsupported
        }
        fn flush_tlb_local(_start: usize, _size: usize) {}
        fn trace_sched_switch(_record: SchedSwitchRecord) {}
        fn fatal_invariant(_code: u32, _argument: usize) -> ! {
            panic!("scheduler invariant reported by ax-sync unit test")
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

#[cfg(feature = "lockdep")]
struct UnitTestKspinLockdep;

#[cfg(feature = "lockdep")]
#[ax_crate_interface::impl_interface]
impl ax_lockdep::KspinLockdepIf for UnitTestKspinLockdep {
    fn collect_current_task_held_locks(snapshot: &mut ax_lockdep::HeldLockSnapshot) {
        HELD_LOCKS.with(|held| snapshot.extend(&held.borrow()));
    }

    fn push_current_task_held_lock(held: ax_lockdep::HeldLock) {
        HELD_LOCKS.with(|stack| stack.borrow_mut().push(held));
    }

    fn pop_current_task_held_lock(lock_addr: usize) {
        HELD_LOCKS.with(|stack| stack.borrow_mut().pop_checked(lock_addr));
    }

    fn console_write_str(_text: &str) {}

    fn fatal() -> ! {
        panic!("ax-sync unit-test lockdep fatal")
    }
}

pub(crate) fn install(task_system: usize, cpu_local: usize) -> std::sync::MutexGuard<'static, ()> {
    let guard = RUNTIME_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    TASK_SYSTEM.store(task_system, Ordering::Release);
    CPU_LOCAL.store(cpu_local, Ordering::Release);
    SCHEDULE_CONTEXT_SAFE.store(true, Ordering::Release);
    guard
}

pub(crate) fn clear() {
    CPU_LOCAL.store(0, Ordering::Release);
    TASK_SYSTEM.store(0, Ordering::Release);
    PREEMPT_DEPTH.store(0, Ordering::Release);
    SCHEDULE_CONTEXT_SAFE.store(true, Ordering::Release);
}

pub(crate) fn set_schedule_context_safe(safe: bool) {
    SCHEDULE_CONTEXT_SAFE.store(safe, Ordering::Release);
}

pub(crate) fn reset_scheduler_ipis() {
    SCHEDULER_IPIS.store(0, Ordering::Release);
    LAST_SCHEDULER_IPI_CPU.store(usize::MAX, Ordering::Release);
}

pub(crate) fn scheduler_ipi_count() -> usize {
    SCHEDULER_IPIS.load(Ordering::Acquire)
}

pub(crate) fn last_scheduler_ipi_cpu() -> Option<usize> {
    let cpu = LAST_SCHEDULER_IPI_CPU.load(Ordering::Acquire);
    (cpu != usize::MAX).then_some(cpu)
}

pub(crate) fn preempt_depth() -> usize {
    PREEMPT_DEPTH.load(Ordering::Acquire)
}
