//! Fake TaskRuntime linked only into the ax-task unit-test binary.

use core::{
    cell::{Cell, RefCell},
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::runtime::{TaskRuntime, *};

static NEXT_TOKEN: AtomicUsize = AtomicUsize::new(1);
static INSTALLED_ADDRESS_SPACE: AtomicUsize = AtomicUsize::new(usize::MAX);

std::thread_local! {
    static ACTIVE_IRQ_TOKENS: RefCell<std::vec::Vec<usize>> = const { RefCell::new(std::vec::Vec::new()) };
    static TASK_SYSTEM_HANDLE: Cell<usize> = const { Cell::new(0) };
    static CPU_LOCAL_HANDLE: Cell<usize> = const { Cell::new(0) };
    static SCHEDULER_FRAME_DEPTH: Cell<usize> = const { Cell::new(0) };
    static MAX_SCHEDULER_FRAME_DEPTH: Cell<usize> = const { Cell::new(0) };
    static IRQ_ENTER_SCHEDULER_FRAME_DEPTH: Cell<usize> = const { Cell::new(0) };
    static IRQ_GUARDS_AT_CONTEXT_SWITCH: Cell<usize> = const { Cell::new(usize::MAX) };
    static ALLOW_CONTEXT_SWITCH: Cell<bool> = const { Cell::new(false) };
    static SCHEDULE_CONTEXT_SAFE: Cell<bool> = const { Cell::new(true) };
    static SCHEDULER_FRAME_ENTER_STATUS: Cell<RuntimeStatus> = const { Cell::new(RuntimeStatus::Success) };
    static SCHEDULER_IPI_STATUS: Cell<RuntimeStatus> = const { Cell::new(RuntimeStatus::Success) };
    static SCHEDULER_IPI_BUSY_REMAINING: Cell<usize> = const { Cell::new(0) };
    static SCHEDULER_IPI_SEND_COUNT: Cell<usize> = const { Cell::new(0) };
    static IN_HARD_IRQ: Cell<bool> = const { Cell::new(false) };
    static CONTEXT_BIND_STATUS: Cell<RuntimeStatus> = const { Cell::new(RuntimeStatus::Success) };
    static LAST_CONTEXT_BINDING: Cell<Option<ContextThreadBinding>> = const { Cell::new(None) };
    static CONTEXT_SWITCH_TAIL_STATUS: Cell<RuntimeStatus> = const { Cell::new(RuntimeStatus::Success) };
    static CONTEXT_SWITCH_TAIL_COUNT: Cell<usize> = const { Cell::new(0) };
    static HOOK_REENTRY_QUERY: Cell<HookReentryQuery> = const { Cell::new(HookReentryQuery::None) };
    static HOOK_REENTRY_ERROR: Cell<Option<crate::TaskError>> = const { Cell::new(None) };
    static IRQ_EXIT_SCHEDULE_REMAINING: Cell<usize> = const { Cell::new(0) };
    static IRQ_EXIT_SCHEDULE_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

#[derive(Clone, Copy)]
enum HookReentryQuery {
    None,
    CurrentThread,
    NeedsReschedule,
}

fn run_hook_reentry_query() {
    let query = HOOK_REENTRY_QUERY.with(|query| query.replace(HookReentryQuery::None));
    let error = match query {
        HookReentryQuery::None => return,
        HookReentryQuery::CurrentThread => crate::current_thread_id().err(),
        HookReentryQuery::NeedsReschedule => crate::current_cpu_needs_resched().err(),
    };
    HOOK_REENTRY_ERROR.with(|observed| observed.set(error));
}

struct UnitTestRuntime;

#[crate::runtime::impl_extern_trait(name = "ax-task_0_7", abi = "rust")]
impl TaskRuntime for UnitTestRuntime {
    unsafe fn task_system_handle() -> TaskSystemHandle {
        TASK_SYSTEM_HANDLE.with(|handle| {
            // SAFETY: unit fixtures keep this pinned system alive until the
            // thread-local handle is cleared.
            unsafe { TaskSystemHandle::from_raw(handle.get()) }
        })
    }
    unsafe fn current_cpu_local_handle() -> CurrentCpuLocalHandle {
        CPU_LOCAL_HANDLE.with(|handle| {
            // SAFETY: unit fixtures install only the current thread's pinned
            // CpuLocal and clear the handle before destroying it.
            unsafe { CurrentCpuLocalHandle::from_raw(handle.get()) }
        })
    }
    unsafe fn cpu_remote_handle(cpu: RuntimeCpuId) -> CpuRemoteHandle {
        let raw = TASK_SYSTEM_HANDLE.with(Cell::get);
        if raw == 0 {
            return CpuRemoteHandle::NONE;
        }
        // SAFETY: unit fixtures keep the pinned system alive until clearing
        // these thread-local handles.
        let system = unsafe { &*core::ptr::with_exposed_provenance::<crate::TaskSystem>(raw) };
        system
            .cpu_remote(crate::CpuId::new(cpu.as_u32()))
            .map_or(CpuRemoteHandle::NONE, |remote| {
                // SAFETY: CpuRemote is Arc-backed by TaskSystem and the fixture
                // keeps that system alive while this handle is published.
                unsafe {
                    CpuRemoteHandle::from_raw(
                        (remote as *const crate::CpuRemote).expose_provenance(),
                    )
                }
            })
    }
    fn current_cpu_id() -> RuntimeCpuId {
        RuntimeCpuId::new(0)
    }
    fn online_cpu_count() -> u32 {
        1
    }

    fn irq_guard_enter() -> IrqGuardToken {
        let scheduler_depth = SCHEDULER_FRAME_DEPTH.with(Cell::get);
        IRQ_ENTER_SCHEDULER_FRAME_DEPTH
            .with(|observed| observed.set(observed.get().max(scheduler_depth)));
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow_mut().push(token));
        // SAFETY: the token was just inserted into ACTIVE_IRQ_TOKENS and stays
        // valid until the matching irq_guard_exit call removes it.
        unsafe { IrqGuardToken::from_raw(token) }
    }

    unsafe fn irq_guard_exit(token: IrqGuardToken) {
        ACTIVE_IRQ_TOKENS.with(|tokens| {
            let mut tokens = tokens.borrow_mut();
            let index = tokens
                .iter()
                .position(|active| *active == token.into_raw())
                .expect("test IRQ token must be active");
            tokens.swap_remove(index);
        });
        let may_reenter = ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow().is_empty())
            && SCHEDULER_FRAME_DEPTH.with(|depth| depth.get() == 0)
            && !IRQ_EXIT_SCHEDULE_ACTIVE.with(Cell::get)
            && IRQ_EXIT_SCHEDULE_REMAINING.with(|remaining| {
                let current = remaining.get();
                if current == 0 {
                    false
                } else {
                    remaining.set(current - 1);
                    true
                }
            });
        if may_reenter {
            IRQ_EXIT_SCHEDULE_ACTIVE.with(|active| active.set(true));
            crate::schedule_current_cpu()
                .expect("configured IRQ-exit scheduler reentry must reach a safe point");
            IRQ_EXIT_SCHEDULE_ACTIVE.with(|active| active.set(false));
        }
    }

    fn finish_context_switch_tail() -> RuntimeStatus {
        CONTEXT_SWITCH_TAIL_COUNT.with(|count| count.set(count.get() + 1));
        CONTEXT_SWITCH_TAIL_STATUS.with(Cell::get)
    }

    fn finish_initial_context_switch() {
        SCHEDULER_FRAME_DEPTH.with(|depth| {
            let current = depth.get();
            assert_eq!(
                current, 1,
                "initial context must inherit one scheduler baton"
            );
            depth.set(0);
        });
    }

    fn scheduler_frame_guard_enter(
        _origin: RuntimeScheduleOrigin,
        _entry: RuntimeSchedulerEntry,
    ) -> RuntimeStatus {
        let status = SCHEDULER_FRAME_ENTER_STATUS.with(Cell::get);
        if status != RuntimeStatus::Success {
            return status;
        }
        SCHEDULER_FRAME_DEPTH.with(|depth| {
            let next = depth
                .get()
                .checked_add(1)
                .expect("test scheduler frame overflow");
            depth.set(next);
            MAX_SCHEDULER_FRAME_DEPTH.with(|maximum| maximum.set(maximum.get().max(next)));
        });
        RuntimeStatus::Success
    }

    fn scheduler_frame_guard_exit(_return_to: RuntimeSchedulerReturn) -> bool {
        let scheduler_clear = SCHEDULER_FRAME_DEPTH.with(|depth| {
            let current = depth.get();
            assert!(current > 0, "unbalanced test scheduler frame exit");
            depth.set(current - 1);
            current == 1
        });
        scheduler_clear && ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow().is_empty())
    }

    fn in_hard_irq() -> bool {
        IN_HARD_IRQ.with(Cell::get)
    }
    fn validate_schedule_context(_origin: RuntimeScheduleOrigin) -> RuntimeStatus {
        if SCHEDULE_CONTEXT_SAFE.with(Cell::get) {
            RuntimeStatus::Success
        } else {
            RuntimeStatus::UnsafeContext
        }
    }
    fn monotonic_ns() -> u64 {
        run_hook_reentry_query();
        0
    }
    fn timer_resolution_ns() -> u64 {
        1
    }
    fn program_oneshot_timer(_deadline_ns: u64) -> RuntimeStatus {
        run_hook_reentry_query();
        RuntimeStatus::Success
    }
    fn send_scheduler_ipi(_cpu: RuntimeCpuId) -> RuntimeStatus {
        run_hook_reentry_query();
        SCHEDULER_IPI_SEND_COUNT.with(|count| count.set(count.get() + 1));
        let busy = SCHEDULER_IPI_BUSY_REMAINING.with(|remaining| {
            let current = remaining.get();
            if current == 0 {
                false
            } else {
                remaining.set(current - 1);
                true
            }
        });
        if busy {
            RuntimeStatus::Busy
        } else {
            SCHEDULER_IPI_STATUS.with(Cell::get)
        }
    }
    fn wait_for_interrupt() {}
    fn allocate_stack(_request: StackRequest) -> RuntimeHandleResult {
        RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
    }
    fn deallocate_stack(_stack: StackHandle) -> RuntimeStatus {
        RuntimeStatus::Unsupported
    }
    fn allocate_tls(_request: TlsRequest) -> RuntimeHandleResult {
        RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
    }
    fn deallocate_tls(_tls: TlsHandle) -> RuntimeStatus {
        RuntimeStatus::Unsupported
    }
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
    fn bind_context_thread(binding: ContextThreadBinding) -> RuntimeStatus {
        LAST_CONTEXT_BINDING.with(|observed| observed.set(Some(binding)));
        CONTEXT_BIND_STATUS.with(Cell::get)
    }
    fn destroy_context(_context: ExecutionContextHandle) -> RuntimeStatus {
        RuntimeStatus::Unsupported
    }
    unsafe fn switch_context(_previous: ExecutionContextHandle, _next: ExecutionContextHandle) {
        assert!(
            ALLOW_CONTEXT_SWITCH.with(Cell::get),
            "unit-test context switches must be explicitly scoped"
        );
        IRQ_GUARDS_AT_CONTEXT_SWITCH.with(|observed| {
            observed.set(ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow().len()));
        });
    }
    fn install_address_space(address_space: AddressSpaceHandle) -> RuntimeStatus {
        INSTALLED_ADDRESS_SPACE.store(address_space.into_raw(), Ordering::Release);
        RuntimeStatus::Success
    }
    fn flush_tlb_local(_start: usize, _size: usize) {}
    fn trace_sched_switch(_record: SchedSwitchRecord) {}
    fn fatal_invariant(_code: u32, _argument: usize) -> ! {
        panic!("scheduler invariant reported by unit test")
    }
}

pub(crate) fn configure_context_binding(status: RuntimeStatus) {
    CONTEXT_BIND_STATUS.with(|current| current.set(status));
    LAST_CONTEXT_BINDING.with(|observed| observed.set(None));
}

pub(crate) fn last_context_binding() -> Option<ContextThreadBinding> {
    LAST_CONTEXT_BINDING.with(Cell::get)
}

pub(crate) fn configure_context_switch_tail(status: RuntimeStatus) {
    CONTEXT_SWITCH_TAIL_STATUS.with(|current| current.set(status));
    CONTEXT_SWITCH_TAIL_COUNT.with(|count| count.set(0));
}

pub(crate) fn context_switch_tail_count() -> usize {
    CONTEXT_SWITCH_TAIL_COUNT.with(Cell::get)
}

pub(crate) fn configure_scheduler_ipi(status: RuntimeStatus, busy_before_status: usize) {
    SCHEDULER_IPI_STATUS.with(|current| current.set(status));
    SCHEDULER_IPI_BUSY_REMAINING.with(|remaining| remaining.set(busy_before_status));
    SCHEDULER_IPI_SEND_COUNT.with(|count| count.set(0));
}

pub(crate) fn scheduler_ipi_send_count() -> usize {
    SCHEDULER_IPI_SEND_COUNT.with(Cell::get)
}

pub(crate) fn reset_irq_state() {
    ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow_mut().clear());
}

pub(crate) fn active_irq_guards() -> usize {
    ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow().len())
}

pub(crate) fn reset_installed_address_space() {
    INSTALLED_ADDRESS_SPACE.store(usize::MAX, Ordering::Release);
}

pub(crate) fn reset_scheduler_frame_state() {
    SCHEDULER_FRAME_DEPTH.with(|depth| depth.set(0));
    MAX_SCHEDULER_FRAME_DEPTH.with(|depth| depth.set(0));
    IRQ_ENTER_SCHEDULER_FRAME_DEPTH.with(|depth| depth.set(0));
    IRQ_GUARDS_AT_CONTEXT_SWITCH.with(|count| count.set(usize::MAX));
}

pub(crate) fn set_schedule_context_safe(safe: bool) {
    SCHEDULE_CONTEXT_SAFE.with(|state| state.set(safe));
}

pub(crate) fn set_scheduler_frame_enter_status(status: RuntimeStatus) {
    SCHEDULER_FRAME_ENTER_STATUS.with(|state| state.set(status));
}

pub(crate) fn set_hard_irq(active: bool) {
    IN_HARD_IRQ.with(|state| state.set(active));
}

pub(crate) fn reenter_current_thread_from_next_hook() {
    HOOK_REENTRY_ERROR.with(|observed| observed.set(None));
    HOOK_REENTRY_QUERY.with(|query| query.set(HookReentryQuery::CurrentThread));
}

pub(crate) fn reenter_needs_reschedule_from_next_hook() {
    HOOK_REENTRY_ERROR.with(|observed| observed.set(None));
    HOOK_REENTRY_QUERY.with(|query| query.set(HookReentryQuery::NeedsReschedule));
}

pub(crate) fn take_hook_reentry_error() -> Option<crate::TaskError> {
    HOOK_REENTRY_ERROR.with(|observed| observed.take())
}

pub(crate) fn configure_irq_exit_schedule_reentry(count: usize) {
    IRQ_EXIT_SCHEDULE_REMAINING.with(|remaining| remaining.set(count));
    IRQ_EXIT_SCHEDULE_ACTIVE.with(|active| active.set(false));
}

pub(crate) fn irq_exit_schedule_reentry_active() -> bool {
    IRQ_EXIT_SCHEDULE_ACTIVE.with(Cell::get)
}

pub(crate) fn scheduler_frame_state() -> (usize, usize, usize) {
    (
        SCHEDULER_FRAME_DEPTH.with(Cell::get),
        MAX_SCHEDULER_FRAME_DEPTH.with(Cell::get),
        IRQ_ENTER_SCHEDULER_FRAME_DEPTH.with(Cell::get),
    )
}

pub(crate) fn irq_guards_at_context_switch() -> usize {
    IRQ_GUARDS_AT_CONTEXT_SWITCH.with(Cell::get)
}

pub(crate) struct AllowedContextSwitch;

impl Drop for AllowedContextSwitch {
    fn drop(&mut self) {
        ALLOW_CONTEXT_SWITCH.with(|allowed| allowed.set(false));
    }
}

pub(crate) fn allow_context_switch() -> AllowedContextSwitch {
    ALLOW_CONTEXT_SWITCH.with(|allowed| {
        assert!(!allowed.replace(true), "nested test context-switch scope");
    });
    AllowedContextSwitch
}

pub(crate) fn installed_address_space() -> Option<usize> {
    let raw = INSTALLED_ADDRESS_SPACE.load(Ordering::Acquire);
    (raw != usize::MAX).then_some(raw)
}

pub(crate) fn install_task_handles(task_system: usize, cpu_local: usize) {
    TASK_SYSTEM_HANDLE.with(|handle| handle.set(task_system));
    CPU_LOCAL_HANDLE.with(|handle| handle.set(cpu_local));
}

pub(crate) fn clear_task_handles() {
    install_task_handles(0, 0);
}
