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
    static IRQ_GUARD_ENTRIES: Cell<usize> = const { Cell::new(0) };
    static ACTIVE_PREEMPT_TOKENS: RefCell<std::vec::Vec<usize>> = const { RefCell::new(std::vec::Vec::new()) };
    static PREEMPT_GUARD_ENTRIES: Cell<usize> = const { Cell::new(0) };
    static TASK_SYSTEM_HANDLE: Cell<usize> = const { Cell::new(0) };
    static CPU_LOCAL_HANDLE: Cell<usize> = const { Cell::new(0) };
    static CURRENT_CPU_REMOTE_HANDLE: Cell<usize> = const { Cell::new(0) };
    static CPU_LOCAL_HANDLE_READS: Cell<usize> = const { Cell::new(0) };
    static CPU_REMOTE_HANDLE_READS: Cell<usize> = const { Cell::new(0) };
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
    static SCHEDULER_IPI_IRQ_GUARDS: Cell<usize> = const { Cell::new(usize::MAX) };
    static IDLE_WAIT_CALLS: Cell<usize> = const { Cell::new(0) };
    static IDLE_WAIT_OBSERVED_POLLING: Cell<bool> = const { Cell::new(false) };
    static IDLE_WAIT_PUBLISH_RESCHEDULE: Cell<bool> = const { Cell::new(false) };
    static IN_HARD_IRQ: Cell<bool> = const { Cell::new(false) };
    static CONTEXT_BIND_STATUS: Cell<RuntimeStatus> = const { Cell::new(RuntimeStatus::Success) };
    static LAST_CONTEXT_BINDING: Cell<Option<ContextThreadBinding>> = const { Cell::new(None) };
    static IRQ_GUARDS_AT_CONTEXT_BIND: Cell<usize> = const { Cell::new(usize::MAX) };
    static CONTEXT_SWITCH_TAIL_STATUS: Cell<RuntimeStatus> = const { Cell::new(RuntimeStatus::Success) };
    static CONTEXT_SWITCH_TAIL_COUNT: Cell<usize> = const { Cell::new(0) };
    static HOOK_REENTRY_QUERY: Cell<HookReentryQuery> = const { Cell::new(HookReentryQuery::None) };
    static HOOK_REENTRY_ERROR: Cell<Option<crate::TaskError>> = const { Cell::new(None) };
    static IRQ_EXIT_SCHEDULE_REMAINING: Cell<usize> = const { Cell::new(0) };
    static IRQ_EXIT_SCHEDULE_ACTIVE: Cell<bool> = const { Cell::new(false) };
    static MONOTONIC_NS: Cell<u64> = const { Cell::new(0) };
    static MONOTONIC_READS: Cell<usize> = const { Cell::new(0) };
    static LAST_TASK_DEADLINE_UPDATE: Cell<Option<TaskDeadlineUpdate>> = const { Cell::new(None) };
    static TASK_DEADLINE_PUBLISH_STATUS: Cell<RuntimeStatus> =
        const { Cell::new(RuntimeStatus::Success) };
    static TASK_DEADLINE_PUBLISH_SUCCESS_REMAINING: Cell<usize> = const { Cell::new(0) };
    static CPU_ONLINE_STATUS: Cell<RuntimeStatus> = const { Cell::new(RuntimeStatus::Success) };
    static CPU_OFFLINE_STATUS: Cell<RuntimeStatus> = const { Cell::new(RuntimeStatus::Success) };
    static CPU_LIFECYCLE_EVENTS: RefCell<std::vec::Vec<CpuLifecycleEvent>> =
        const { RefCell::new(std::vec::Vec::new()) };
    static SWITCH_OBSERVATIONS: RefCell<std::vec::Vec<SwitchObservation>> =
        const { RefCell::new(std::vec::Vec::new()) };
    static RESOURCE_RELEASE_STATUS: Cell<RuntimeStatus> =
        const { Cell::new(RuntimeStatus::Unsupported) };
    static RESOURCE_RELEASE_EVENTS: RefCell<std::vec::Vec<ResourceReleaseEvent>> =
        const { RefCell::new(std::vec::Vec::new()) };
}

#[derive(Clone, Copy)]
enum HookReentryQuery {
    None,
    CurrentThread,
    NeedsReschedule,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SwitchObservation {
    Trace(SchedSwitchRecord),
    SwitchOut {
        thread: crate::ThreadId,
        reason: crate::SwitchReason,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceReleaseEvent {
    DestroyContext,
    DeallocateTls,
    DeallocateStack,
    DropExtension,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CpuLifecycleEvent {
    Online(RuntimeCpuId),
    Offline(RuntimeCpuId),
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
        CPU_LOCAL_HANDLE_READS.with(|reads| reads.set(reads.get() + 1));
        CPU_LOCAL_HANDLE.with(|handle| {
            // SAFETY: unit fixtures install only the current thread's pinned
            // CpuLocal and clear the handle before destroying it.
            unsafe { CurrentCpuLocalHandle::from_raw(handle.get()) }
        })
    }
    unsafe fn current_cpu_remote_handle() -> CpuRemoteHandle {
        CURRENT_CPU_REMOTE_HANDLE.with(|handle| {
            // SAFETY: unit fixtures install only CPU 0's Arc-backed endpoint
            // and retain the owning TaskSystem until this slot is cleared.
            unsafe { CpuRemoteHandle::from_raw(handle.get()) }
        })
    }
    unsafe fn cpu_remote_handle(cpu: RuntimeCpuId) -> CpuRemoteHandle {
        CPU_REMOTE_HANDLE_READS.with(|reads| reads.set(reads.get() + 1));
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
    unsafe fn current_cpu_id() -> RuntimeCpuId {
        RuntimeCpuId::new(0)
    }
    fn online_cpu_count() -> u32 {
        1
    }

    fn prepare_cpu_online(cpu: RuntimeCpuId) -> RuntimeStatus {
        CPU_LIFECYCLE_EVENTS.with(|events| {
            events.borrow_mut().push(CpuLifecycleEvent::Online(cpu));
        });
        CPU_ONLINE_STATUS.with(Cell::get)
    }

    fn prepare_cpu_offline(cpu: RuntimeCpuId) -> RuntimeStatus {
        CPU_LIFECYCLE_EVENTS.with(|events| {
            events.borrow_mut().push(CpuLifecycleEvent::Offline(cpu));
        });
        CPU_OFFLINE_STATUS.with(Cell::get)
    }

    fn irq_guard_enter() -> IrqGuardToken {
        IRQ_GUARD_ENTRIES.with(|entries| entries.set(entries.get() + 1));
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

    fn preempt_guard_enter() -> PreemptGuardToken {
        let owner_scope = ACTIVE_IRQ_TOKENS.with(|tokens| !tokens.borrow().is_empty())
            || SCHEDULER_FRAME_DEPTH.with(|depth| depth.get() != 0);
        if owner_scope {
            return PreemptGuardToken::NONE;
        }
        PREEMPT_GUARD_ENTRIES.with(|entries| entries.set(entries.get() + 1));
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        ACTIVE_PREEMPT_TOKENS.with(|tokens| tokens.borrow_mut().push(token));
        // SAFETY: the token remains active until the matching exit consumes
        // it in this single-threaded fake execution context.
        unsafe { PreemptGuardToken::from_raw(token) }
    }

    unsafe fn preempt_guard_exit(token: PreemptGuardToken) {
        assert!(
            !token.is_none(),
            "an inherited owner scope must not be exited as an ordinary preemption guard"
        );
        ACTIVE_PREEMPT_TOKENS.with(|tokens| {
            let mut tokens = tokens.borrow_mut();
            let index = tokens
                .iter()
                .position(|active| *active == token.into_raw())
                .expect("test preempt token must be active");
            tokens.swap_remove(index);
        });
    }

    fn local_scheduler_work_is_self_serviced() -> bool {
        IN_HARD_IRQ.with(Cell::get)
            || IRQ_EXIT_SCHEDULE_REMAINING.with(|remaining| remaining.get() != 0)
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
        let irq_clear = ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow().is_empty());
        let scheduler_clear = SCHEDULER_FRAME_DEPTH.with(|depth| depth.get() == 0);
        if SCHEDULE_CONTEXT_SAFE.with(Cell::get)
            && !IN_HARD_IRQ.with(Cell::get)
            && irq_clear
            && scheduler_clear
        {
            RuntimeStatus::Success
        } else {
            RuntimeStatus::UnsafeContext
        }
    }
    fn validate_owner_cpu_context() -> RuntimeStatus {
        let irq_pinned = ACTIVE_IRQ_TOKENS.with(|tokens| !tokens.borrow().is_empty());
        let scheduler_pinned = SCHEDULER_FRAME_DEPTH.with(|depth| depth.get() != 0);
        if irq_pinned || scheduler_pinned {
            RuntimeStatus::Success
        } else {
            RuntimeStatus::UnsafeContext
        }
    }
    fn monotonic_ns() -> u64 {
        run_hook_reentry_query();
        MONOTONIC_READS.with(|reads| reads.set(reads.get() + 1));
        MONOTONIC_NS.with(Cell::get)
    }
    fn timer_resolution_ns() -> u64 {
        1
    }
    fn publish_task_deadline(update: TaskDeadlineUpdate) -> RuntimeStatus {
        run_hook_reentry_query();
        LAST_TASK_DEADLINE_UPDATE.with(|observed| observed.set(Some(update)));
        let publish_succeeds = TASK_DEADLINE_PUBLISH_SUCCESS_REMAINING.with(|remaining| {
            let current = remaining.get();
            if current == 0 {
                false
            } else {
                remaining.set(current - 1);
                true
            }
        });
        if publish_succeeds {
            RuntimeStatus::Success
        } else {
            TASK_DEADLINE_PUBLISH_STATUS.with(Cell::get)
        }
    }
    fn send_scheduler_ipi(_cpu: RuntimeCpuId) -> RuntimeStatus {
        run_hook_reentry_query();
        SCHEDULER_IPI_SEND_COUNT.with(|count| count.set(count.get() + 1));
        let irq_guards = ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow().len());
        SCHEDULER_IPI_IRQ_GUARDS.with(|observed| observed.set(irq_guards));
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
    fn wait_for_interrupt() {
        IDLE_WAIT_CALLS.with(|calls| calls.set(calls.get() + 1));
        let raw = TASK_SYSTEM_HANDLE.with(Cell::get);
        if raw == 0 {
            return;
        }
        // SAFETY: the fixture keeps the installed task system pinned until it
        // clears this thread-local handle after the idle call returns.
        let system = unsafe { &*core::ptr::with_exposed_provenance::<crate::TaskSystem>(raw) };
        let remote = system
            .cpu_remote(crate::CpuId::new(0))
            .expect("test idle wait requires the installed CPU");
        IDLE_WAIT_OBSERVED_POLLING.with(|observed| observed.set(remote.is_idle_polling()));
        if IDLE_WAIT_PUBLISH_RESCHEDULE.with(Cell::get) {
            remote.request_reschedule();
        }
    }
    fn allocate_stack(_request: StackRequest) -> RuntimeHandleResult {
        RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
    }
    fn deallocate_stack(_stack: StackHandle) -> RuntimeStatus {
        record_resource_release_event(ResourceReleaseEvent::DeallocateStack);
        RESOURCE_RELEASE_STATUS.with(Cell::get)
    }
    fn allocate_tls(_request: TlsRequest) -> RuntimeHandleResult {
        RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
    }
    fn deallocate_tls(_tls: TlsHandle) -> RuntimeStatus {
        record_resource_release_event(ResourceReleaseEvent::DeallocateTls);
        RESOURCE_RELEASE_STATUS.with(Cell::get)
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
        IRQ_GUARDS_AT_CONTEXT_BIND.with(|observed| {
            observed.set(ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow().len()));
        });
        CONTEXT_BIND_STATUS.with(Cell::get)
    }
    fn destroy_context(_context: ExecutionContextHandle) -> RuntimeStatus {
        record_resource_release_event(ResourceReleaseEvent::DestroyContext);
        RESOURCE_RELEASE_STATUS.with(Cell::get)
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
    fn trace_sched_switch(record: SchedSwitchRecord) {
        SWITCH_OBSERVATIONS.with(|observations| {
            observations
                .borrow_mut()
                .push(SwitchObservation::Trace(record));
        });
    }
    fn fatal_invariant(_code: u32, _argument: usize) -> ! {
        panic!("scheduler invariant reported by unit test")
    }
}

pub(crate) fn configure_context_binding(status: RuntimeStatus) {
    CONTEXT_BIND_STATUS.with(|current| current.set(status));
    LAST_CONTEXT_BINDING.with(|observed| observed.set(None));
    IRQ_GUARDS_AT_CONTEXT_BIND.with(|observed| observed.set(usize::MAX));
}

pub(crate) fn configure_resource_release(status: RuntimeStatus) {
    RESOURCE_RELEASE_STATUS.with(|current| current.set(status));
    RESOURCE_RELEASE_EVENTS.with(|events| events.borrow_mut().clear());
}

pub(crate) fn record_resource_release_event(event: ResourceReleaseEvent) {
    RESOURCE_RELEASE_EVENTS.with(|events| events.borrow_mut().push(event));
}

pub(crate) fn resource_release_events() -> std::vec::Vec<ResourceReleaseEvent> {
    RESOURCE_RELEASE_EVENTS.with(|events| events.borrow().clone())
}

pub(crate) fn last_context_binding() -> Option<ContextThreadBinding> {
    LAST_CONTEXT_BINDING.with(Cell::get)
}

pub(crate) fn irq_guards_at_context_bind() -> usize {
    IRQ_GUARDS_AT_CONTEXT_BIND.with(Cell::get)
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
    SCHEDULER_IPI_IRQ_GUARDS.with(|observed| observed.set(usize::MAX));
}

pub(crate) fn configure_task_deadline_publish(
    status: RuntimeStatus,
    successful_before_status: usize,
) {
    TASK_DEADLINE_PUBLISH_STATUS.with(|current| current.set(status));
    TASK_DEADLINE_PUBLISH_SUCCESS_REMAINING
        .with(|remaining| remaining.set(successful_before_status));
}

pub(crate) fn scheduler_ipi_send_count() -> usize {
    SCHEDULER_IPI_SEND_COUNT.with(Cell::get)
}

pub(crate) fn scheduler_ipi_irq_guards() -> usize {
    SCHEDULER_IPI_IRQ_GUARDS.with(Cell::get)
}

pub(crate) fn configure_idle_wait(publish_reschedule: bool) {
    IDLE_WAIT_CALLS.with(|calls| calls.set(0));
    IDLE_WAIT_OBSERVED_POLLING.with(|observed| observed.set(false));
    IDLE_WAIT_PUBLISH_RESCHEDULE.with(|publish| publish.set(publish_reschedule));
}

pub(crate) fn idle_wait_observation() -> (usize, bool) {
    (
        IDLE_WAIT_CALLS.with(Cell::get),
        IDLE_WAIT_OBSERVED_POLLING.with(Cell::get),
    )
}

pub(crate) fn reset_irq_state() {
    ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow_mut().clear());
    IRQ_GUARD_ENTRIES.with(|entries| entries.set(0));
}

pub(crate) fn reset_irq_guard_entries() {
    IRQ_GUARD_ENTRIES.with(|entries| entries.set(0));
}

pub(crate) fn active_irq_guards() -> usize {
    ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow().len())
}

pub(crate) fn irq_guard_entries() -> usize {
    IRQ_GUARD_ENTRIES.with(Cell::get)
}

pub(crate) fn reset_preempt_state() {
    ACTIVE_PREEMPT_TOKENS.with(|tokens| tokens.borrow_mut().clear());
    PREEMPT_GUARD_ENTRIES.with(|entries| entries.set(0));
}

pub(crate) fn reset_preempt_guard_entries() {
    PREEMPT_GUARD_ENTRIES.with(|entries| entries.set(0));
}

pub(crate) fn active_preempt_guards() -> usize {
    ACTIVE_PREEMPT_TOKENS.with(|tokens| tokens.borrow().len())
}

pub(crate) fn preempt_guard_entries() -> usize {
    PREEMPT_GUARD_ENTRIES.with(Cell::get)
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

pub(crate) fn reset_switch_observations() {
    SWITCH_OBSERVATIONS.with(|observations| observations.borrow_mut().clear());
}

pub(crate) fn record_switch_out(thread: crate::ThreadId, reason: crate::SwitchReason) {
    SWITCH_OBSERVATIONS.with(|observations| {
        observations
            .borrow_mut()
            .push(SwitchObservation::SwitchOut { thread, reason });
    });
}

pub(crate) fn take_switch_observations() -> std::vec::Vec<SwitchObservation> {
    SWITCH_OBSERVATIONS.with(|observations| core::mem::take(&mut *observations.borrow_mut()))
}

pub(crate) fn install_task_handles(task_system: usize, cpu_local: usize) {
    TASK_SYSTEM_HANDLE.with(|handle| handle.set(task_system));
    CPU_LOCAL_HANDLE.with(|handle| handle.set(cpu_local));
    let remote = if task_system == 0 {
        0
    } else {
        // SAFETY: the fixture retains this TaskSystem until clear_task_handles.
        let system =
            unsafe { &*core::ptr::with_exposed_provenance::<crate::TaskSystem>(task_system) };
        system
            .runtime_cpu_remote_handle(crate::CpuId::new(0))
            .into_raw()
    };
    CURRENT_CPU_REMOTE_HANDLE.with(|handle| handle.set(remote));
}

pub(crate) fn reset_cpu_handle_reads() {
    CPU_LOCAL_HANDLE_READS.with(|reads| reads.set(0));
    CPU_REMOTE_HANDLE_READS.with(|reads| reads.set(0));
}

pub(crate) fn cpu_handle_reads() -> (usize, usize) {
    (
        CPU_LOCAL_HANDLE_READS.with(Cell::get),
        CPU_REMOTE_HANDLE_READS.with(Cell::get),
    )
}

pub(crate) fn clear_task_handles() {
    install_task_handles(0, 0);
    reset_cpu_handle_reads();
    MONOTONIC_NS.with(|now| now.set(0));
    MONOTONIC_READS.with(|reads| reads.set(0));
    LAST_TASK_DEADLINE_UPDATE.with(|observed| observed.set(None));
    CPU_LIFECYCLE_EVENTS.with(|events| events.borrow_mut().clear());
    configure_cpu_lifecycle(RuntimeStatus::Success, RuntimeStatus::Success);
    configure_task_deadline_publish(RuntimeStatus::Success, 0);
}

pub(crate) fn set_monotonic_ns(now_ns: u64) {
    MONOTONIC_NS.with(|now| now.set(now_ns));
}

pub(crate) fn reset_monotonic_reads() {
    MONOTONIC_READS.with(|reads| reads.set(0));
}

pub(crate) fn monotonic_reads() -> usize {
    MONOTONIC_READS.with(Cell::get)
}

pub(crate) fn take_task_deadline_update() -> Option<TaskDeadlineUpdate> {
    LAST_TASK_DEADLINE_UPDATE.with(Cell::take)
}

pub(crate) fn take_cpu_lifecycle_events() -> std::vec::Vec<CpuLifecycleEvent> {
    CPU_LIFECYCLE_EVENTS.with(|events| core::mem::take(&mut *events.borrow_mut()))
}

pub(crate) fn configure_cpu_lifecycle(online_status: RuntimeStatus, offline_status: RuntimeStatus) {
    CPU_ONLINE_STATUS.with(|status| status.set(online_status));
    CPU_OFFLINE_STATUS.with(|status| status.set(offline_status));
}
