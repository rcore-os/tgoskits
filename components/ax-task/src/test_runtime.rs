//! Fake TaskRuntime linked only into the ax-task unit-test binary.

#[cfg(feature = "lockdep")]
use alloc::boxed::Box;
#[cfg(feature = "lockdep")]
use core::pin::Pin;
use core::{
    cell::{Cell, RefCell},
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::runtime::{TaskRuntime, *};

static NEXT_TOKEN: AtomicUsize = AtomicUsize::new(1);
static INSTALLED_ADDRESS_SPACE: AtomicUsize = AtomicUsize::new(usize::MAX);
const MAX_TEST_CPUS: usize = 64;

std::thread_local! {
    static ACTIVE_IRQ_TOKENS: RefCell<std::vec::Vec<usize>> = const { RefCell::new(std::vec::Vec::new()) };
    static IRQ_GUARD_ENTRIES: Cell<usize> = const { Cell::new(0) };
    static ACTIVE_PREEMPT_TOKENS: RefCell<std::vec::Vec<usize>> = const { RefCell::new(std::vec::Vec::new()) };
    static PREEMPT_GUARD_ENTRIES: Cell<usize> = const { Cell::new(0) };
    static LOCAL_IRQ_ENABLED: Cell<bool> = const { Cell::new(true) };
    static PREEMPT_EXIT_LOCAL_IRQ_ENABLED: Cell<Option<bool>> = const { Cell::new(None) };
    static IRQ_RETURN_EXIT_LOCAL_IRQ_ENABLED: Cell<Option<bool>> = const { Cell::new(None) };
    static TASK_SYSTEM_HANDLE: Cell<usize> = const { Cell::new(0) };
    static CPU_LOCAL_HANDLE: Cell<usize> = const { Cell::new(0) };
    static CURRENT_CPU_REMOTE_HANDLE: Cell<usize> = const { Cell::new(0) };
    static CURRENT_CPU_REMOTE_HANDLE_READS: Cell<usize> = const { Cell::new(0) };
    static CPU_LOCAL_HANDLE_READS: Cell<usize> = const { Cell::new(0) };
    static CPU_REMOTE_HANDLE_READS: Cell<usize> = const { Cell::new(0) };
    static CPU_OWNER_CLAIMS: Cell<usize> = const { Cell::new(0) };
    static SCHEDULER_FRAME_DEPTH: Cell<usize> = const { Cell::new(0) };
    static MAX_SCHEDULER_FRAME_DEPTH: Cell<usize> = const { Cell::new(0) };
    static IRQ_ENTER_SCHEDULER_FRAME_DEPTH: Cell<usize> = const { Cell::new(0) };
    static IRQ_GUARDS_AT_CONTEXT_SWITCH: Cell<usize> = const { Cell::new(usize::MAX) };
    static ALLOW_CONTEXT_SWITCH: Cell<bool> = const { Cell::new(false) };
    static SCHEDULE_CONTEXT_SAFE: Cell<bool> = const { Cell::new(true) };
    static SCHEDULER_FRAME_ENTER_STATUS: Cell<RuntimeStatus> = const { Cell::new(RuntimeStatus::Success) };
    static SCHEDULER_IPI_STATUS: Cell<RuntimeStatus> = const { Cell::new(RuntimeStatus::Success) };
    static SCHEDULER_IPI_SEND_COUNT: Cell<usize> = const { Cell::new(0) };
    static SCHEDULER_IPI_IRQ_GUARDS: Cell<usize> = const { Cell::new(usize::MAX) };
    static IDLE_WAIT_CALLS: Cell<usize> = const { Cell::new(0) };
    static IDLE_WAIT_OBSERVED_POLLING: Cell<bool> = const { Cell::new(false) };
    static IDLE_WAIT_PUBLISH_RESCHEDULE: Cell<bool> = const { Cell::new(false) };
    static IN_HARD_IRQ: Cell<bool> = const { Cell::new(false) };
    static HARDIRQ_DEPTH: Cell<usize> = const { Cell::new(0) };
    static CONTEXT_BIND_STATUS: Cell<RuntimeStatus> = const { Cell::new(RuntimeStatus::Success) };
    static LAST_CONTEXT_BINDING: Cell<Option<ContextThreadBinding>> = const { Cell::new(None) };
    static IRQ_GUARDS_AT_CONTEXT_BIND: Cell<usize> = const { Cell::new(usize::MAX) };
    static CONTEXT_SWITCH_TAIL_COUNT: Cell<usize> = const { Cell::new(0) };
    static HOOK_REENTRY_QUERY: Cell<HookReentryQuery> = const { Cell::new(HookReentryQuery::None) };
    static HOOK_REENTRY_ERROR: Cell<Option<crate::TaskError>> = const { Cell::new(None) };
    static IRQ_EXIT_SCHEDULE_REMAINING: Cell<usize> = const { Cell::new(0) };
    static IRQ_EXIT_SCHEDULE_ACTIVE: Cell<bool> = const { Cell::new(false) };
    static LOCAL_SCHEDULER_WORK_PUBLICATIONS: Cell<usize> = const { Cell::new(0) };
    static MONOTONIC_NS: Cell<u64> = const { Cell::new(0) };
    static SCHEDULER_NS: RefCell<[u64; MAX_TEST_CPUS]> = const {
        RefCell::new([0; MAX_TEST_CPUS])
    };
    static HARDIRQ_NS: RefCell<[u64; MAX_TEST_CPUS]> = const {
        RefCell::new([0; MAX_TEST_CPUS])
    };
    static MONOTONIC_READS: Cell<usize> = const { Cell::new(0) };
    static SCHEDULER_READS: Cell<usize> = const { Cell::new(0) };
    static LAST_SCHEDULER_DEADLINE_UPDATE: Cell<Option<SchedulerDeadlineUpdate>> = const { Cell::new(None) };
    static CPU_ONLINE_STATUS: Cell<RuntimeStatus> = const { Cell::new(RuntimeStatus::Success) };
    static CPU_OFFLINE_STATUS: Cell<RuntimeStatus> = const { Cell::new(RuntimeStatus::Success) };
    static CPU_LIFECYCLE_EVENTS: RefCell<std::vec::Vec<CpuLifecycleEvent>> =
        const { RefCell::new(std::vec::Vec::new()) };
    static SWITCH_OBSERVATIONS: RefCell<std::vec::Vec<SwitchObservation>> =
        const { RefCell::new(std::vec::Vec::new()) };
    static RESOURCE_RELEASE_STATUS: Cell<RuntimeStatus> =
        const { Cell::new(RuntimeStatus::Unsupported) };
    static ADDRESS_SPACE_DESTROY_OUTCOME: Cell<AddressSpaceDestroyOutcome> =
        const { Cell::new(AddressSpaceDestroyOutcome::Released) };
    static ADDRESS_SPACE_RECLAIM_ARM_OUTCOME: Cell<AddressSpaceReclaimArmOutcome> =
        const { Cell::new(AddressSpaceReclaimArmOutcome::Ready) };
    static ADDRESS_SPACE_MEMBARRIER_BITS: RefCell<std::collections::BTreeMap<usize, u32>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
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
    DestroyAddressSpace,
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
    unsafe fn current_cpu_owner_handles() -> CurrentCpuOwnerHandles {
        CPU_LOCAL_HANDLE_READS.with(|reads| reads.set(reads.get() + 1));
        let local = CPU_LOCAL_HANDLE.with(|handle| {
            // SAFETY: unit fixtures install only the current thread's pinned
            // CpuLocal and clear the handle before destroying it.
            unsafe { CurrentCpuLocalHandle::from_raw(handle.get()) }
        });
        let remote = CURRENT_CPU_REMOTE_HANDLE.with(|handle| {
            // SAFETY: the fixture retains the TaskSystem that owns this
            // current-CPU endpoint until the snapshot is no longer usable.
            unsafe { CpuRemoteHandle::from_raw(handle.get()) }
        });
        // SAFETY: both handles are installed together for modeled CPU 0 and
        // remain live for the surrounding test runtime scope.
        unsafe { CurrentCpuOwnerHandles::new(RuntimeCpuId::new(0), local, remote) }
    }
    unsafe fn current_cpu_remote_handle() -> CpuRemoteHandle {
        CURRENT_CPU_REMOTE_HANDLE_READS.with(|reads| reads.set(reads.get() + 1));
        CURRENT_CPU_REMOTE_HANDLE.with(|handle| {
            // SAFETY: unit fixtures install only CPU 0's Arc-backed endpoint
            // and retain the owning TaskSystem until this slot is cleared.
            unsafe { CpuRemoteHandle::from_raw(handle.get()) }
        })
    }
    unsafe fn current_thread_publication() -> CurrentThreadPublication {
        let raw = CPU_LOCAL_HANDLE.with(Cell::get);
        if raw == 0 {
            return CurrentThreadPublication::NONE;
        }
        // SAFETY: fixtures keep this pinned CpuLocal and its current-core Arc
        // alive while the modeled task context is installed.
        let local = unsafe { &*core::ptr::with_exposed_provenance::<crate::CpuLocal>(raw) };
        local
            .current_core()
            .map_or(CurrentThreadPublication::NONE, |core| {
                CurrentThreadPublication::from_core(core.id(), &core)
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

    fn local_irq_save_and_disable() -> LocalIrqState {
        let was_enabled = LOCAL_IRQ_ENABLED.replace(false);
        // SAFETY: the fake runtime accepts the encoded boolean in its matching
        // restore operation.
        unsafe { LocalIrqState::from_raw(usize::from(was_enabled)) }
    }

    unsafe fn local_irq_restore(state: LocalIrqState) {
        LOCAL_IRQ_ENABLED.set(state.into_raw() != 0);
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
            && ACTIVE_PREEMPT_TOKENS.with(|tokens| tokens.borrow().is_empty())
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
        PREEMPT_EXIT_LOCAL_IRQ_ENABLED.with(|observed| {
            observed.set(Some(LOCAL_IRQ_ENABLED.with(Cell::get)));
        });
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

    unsafe fn preempt_guard_exit_irq_return(token: PreemptGuardToken) {
        IRQ_RETURN_EXIT_LOCAL_IRQ_ENABLED.with(|observed| {
            observed.set(Some(LOCAL_IRQ_ENABLED.with(Cell::get)));
        });
        // SAFETY: the IRQ-return model consumes the same active token as the
        // ordinary fake-runtime exit while preserving raw IRQ state separately.
        unsafe { Self::preempt_guard_exit(token) };
    }

    fn hardirq_enter() {
        HARDIRQ_DEPTH.with(|depth| depth.set(depth.get() + 1));
    }

    fn hardirq_exit() {
        HARDIRQ_DEPTH.with(|depth| {
            depth.set(
                depth
                    .get()
                    .checked_sub(1)
                    .expect("test hard-IRQ depth underflow"),
            );
        });
    }

    fn publish_local_scheduler_work() -> bool {
        LOCAL_SCHEDULER_WORK_PUBLICATIONS.with(|count| count.set(count.get() + 1));
        IN_HARD_IRQ.with(Cell::get)
            || HARDIRQ_DEPTH.with(|depth| depth.get() != 0)
            || IRQ_EXIT_SCHEDULE_REMAINING.with(|remaining| remaining.get() != 0)
    }

    fn finish_context_switch_tail() {
        CONTEXT_SWITCH_TAIL_COUNT.with(|count| count.set(count.get() + 1));
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
        IN_HARD_IRQ.with(Cell::get) || HARDIRQ_DEPTH.with(|depth| depth.get() != 0)
    }
    fn validate_schedule_context(_origin: RuntimeScheduleOrigin) -> RuntimeStatus {
        let irq_clear = ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow().is_empty());
        let preempt_clear = ACTIVE_PREEMPT_TOKENS.with(|tokens| tokens.borrow().is_empty());
        let scheduler_clear = SCHEDULER_FRAME_DEPTH.with(|depth| depth.get() == 0);
        if SCHEDULE_CONTEXT_SAFE.with(Cell::get)
            && !(IN_HARD_IRQ.with(Cell::get) || HARDIRQ_DEPTH.with(|depth| depth.get() != 0))
            && irq_clear
            && preempt_clear
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
    fn monotonic_now() -> crate::runtime::MonotonicInstant {
        run_hook_reentry_query();
        MONOTONIC_READS.with(|reads| reads.set(reads.get() + 1));
        crate::runtime::MonotonicInstant::from_nanos(MONOTONIC_NS.with(Cell::get))
            .expect("test monotonic clock must remain in the ktime domain")
    }
    fn rq_clock_sample(cpu: RuntimeCpuId) -> RqClockSample {
        run_hook_reentry_query();
        SCHEDULER_READS.with(|reads| reads.set(reads.get() + 1));
        let index = cpu.as_u32() as usize;
        let now_ns = SCHEDULER_NS.with(|clocks| {
            clocks
                .borrow()
                .get(index)
                .copied()
                .expect("test scheduler CPU must fit the fake clock table")
        });
        let hardirq_time_ns = HARDIRQ_NS.with(|clocks| {
            clocks
                .borrow()
                .get(index)
                .copied()
                .expect("test scheduler CPU must fit the fake IRQ clock table")
        });
        RqClockSample::new(
            crate::SchedulerTimestamp::from_nanos(now_ns),
            hardirq_time_ns,
        )
    }
    fn publish_scheduler_deadline(update: SchedulerDeadlineUpdate) {
        run_hook_reentry_query();
        LAST_SCHEDULER_DEADLINE_UPDATE.with(|observed| observed.set(Some(update)));
    }
    fn notify_scheduler_cpu(_cpu: RuntimeCpuId) -> RuntimeStatus {
        run_hook_reentry_query();
        SCHEDULER_IPI_SEND_COUNT.with(|count| count.set(count.get() + 1));
        let irq_guards = ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow().len());
        SCHEDULER_IPI_IRQ_GUARDS.with(|observed| observed.set(irq_guards));
        let status = SCHEDULER_IPI_STATUS.with(Cell::get);
        status
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
    fn deallocate_stack(_stack: StackHandle) {
        record_resource_release_event(ResourceReleaseEvent::DeallocateStack);
        let status = RESOURCE_RELEASE_STATUS.with(Cell::get);
        if status != RuntimeStatus::Success {
            Self::fatal_invariant(0x5253_0003, status as usize);
        }
    }
    fn allocate_tls(_request: TlsRequest) -> RuntimeHandleResult {
        RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
    }
    fn deallocate_tls(_tls: TlsHandle) {
        record_resource_release_event(ResourceReleaseEvent::DeallocateTls);
        let status = RESOURCE_RELEASE_STATUS.with(Cell::get);
        if status != RuntimeStatus::Success {
            Self::fatal_invariant(0x5253_0002, status as usize);
        }
    }
    fn create_kernel_context(_request: KernelContextRequest) -> RuntimeHandleResult {
        RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
    }
    fn create_user_context(_request: UserContextRequest) -> RuntimeHandleResult {
        RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
    }
    fn bind_context_thread(binding: ContextThreadBinding) -> RuntimeStatus {
        LAST_CONTEXT_BINDING.with(|observed| observed.set(Some(binding)));
        IRQ_GUARDS_AT_CONTEXT_BIND.with(|observed| {
            observed.set(ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow().len()));
        });
        CONTEXT_BIND_STATUS.with(Cell::get)
    }
    fn destroy_context(_context: ExecutionContextHandle) {
        record_resource_release_event(ResourceReleaseEvent::DestroyContext);
        let status = RESOURCE_RELEASE_STATUS.with(Cell::get);
        if status != RuntimeStatus::Success {
            Self::fatal_invariant(0x5253_0001, status as usize);
        }
    }

    fn destroy_address_space(_address_space: AddressSpaceHandle) -> AddressSpaceDestroyOutcome {
        record_resource_release_event(ResourceReleaseEvent::DestroyAddressSpace);
        ADDRESS_SPACE_DESTROY_OUTCOME.with(Cell::get)
    }

    fn arm_address_space_reclaim(
        _address_space: AddressSpaceHandle,
    ) -> AddressSpaceReclaimArmOutcome {
        ADDRESS_SPACE_RECLAIM_ARM_OUTCOME.with(Cell::get)
    }
    fn address_space_membarrier_state(
        address_space: AddressSpaceHandle,
    ) -> AddressSpaceMembarrierState {
        let raw = address_space.into_raw();
        assert_ne!(
            raw, 0,
            "test membarrier state requires a user address space"
        );
        let bits = ADDRESS_SPACE_MEMBARRIER_BITS
            .with(|states| states.borrow().get(&raw).copied().unwrap_or(0));
        // SAFETY: fixture handles remain unique while their test task exists.
        let identity = unsafe { AddressSpaceMembarrierId::from_raw(raw) };
        // SAFETY: the fixture changes only typed requested/ready bits below.
        unsafe { AddressSpaceMembarrierState::new(identity, bits) }
    }
    fn update_address_space_membarrier_state(
        address_space: AddressSpaceHandle,
        registration: MembarrierRegistration,
        phase: MembarrierRegistrationPhase,
    ) -> AddressSpaceMembarrierState {
        let raw = address_space.into_raw();
        ADDRESS_SPACE_MEMBARRIER_BITS.with(|states| {
            let mut states = states.borrow_mut();
            let bits = states.entry(raw).or_default();
            *bits |= match phase {
                MembarrierRegistrationPhase::Begin => registration.requested_bit(),
                MembarrierRegistrationPhase::Complete => registration.ready_bit(),
            };
        });
        Self::address_space_membarrier_state(address_space)
    }
    fn synchronize_membarrier_cpu(
        _cpu: RuntimeCpuId,
        action: RuntimeMembarrierAction,
    ) -> RuntimeStatus {
        match action {
            RuntimeMembarrierAction::MemoryBarrier => core::sync::atomic::fence(Ordering::SeqCst),
            RuntimeMembarrierAction::RefreshRunQueue => {
                crate::refresh_current_membarrier_run_queue().unwrap()
            }
        }
        RuntimeStatus::Success
    }
    unsafe fn switch_context(_switch: ContextSwitch) {
        assert!(
            ALLOW_CONTEXT_SWITCH.with(Cell::get),
            "unit-test context switches must be explicitly scoped"
        );
        IRQ_GUARDS_AT_CONTEXT_SWITCH.with(|observed| {
            observed.set(ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow().len()));
        });
    }
    fn activate_address_space(activation: AddressSpaceActivation) -> RuntimeStatus {
        let address_space = activation
            .user_handle()
            .map_or(0, AddressSpaceHandle::into_raw);
        INSTALLED_ADDRESS_SPACE.store(address_space, Ordering::Release);
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
    fn emergency_console_write(_message: &str) {}
    fn fatal_invariant(code: u32, argument: usize) -> ! {
        panic!("scheduler invariant {code:#010x} reported with argument {argument:#x}")
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

pub(crate) fn configure_address_space_destroy(outcome: AddressSpaceDestroyOutcome) {
    ADDRESS_SPACE_DESTROY_OUTCOME.with(|current| current.set(outcome));
}

pub(crate) fn configure_address_space_reclaim_arm(outcome: AddressSpaceReclaimArmOutcome) {
    ADDRESS_SPACE_RECLAIM_ARM_OUTCOME.with(|current| current.set(outcome));
}

pub(crate) fn clear_resource_release_events() {
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

pub(crate) fn reset_context_switch_tail_count() {
    CONTEXT_SWITCH_TAIL_COUNT.with(|count| count.set(0));
}

pub(crate) fn context_switch_tail_count() -> usize {
    CONTEXT_SWITCH_TAIL_COUNT.with(Cell::get)
}

pub(crate) fn configure_scheduler_ipi(status: RuntimeStatus) {
    SCHEDULER_IPI_STATUS.with(|current| current.set(status));
    SCHEDULER_IPI_SEND_COUNT.with(|count| count.set(0));
    SCHEDULER_IPI_IRQ_GUARDS.with(|observed| observed.set(usize::MAX));
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
    PREEMPT_EXIT_LOCAL_IRQ_ENABLED.with(|observed| observed.set(None));
    IRQ_RETURN_EXIT_LOCAL_IRQ_ENABLED.with(|observed| observed.set(None));
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

pub(crate) fn reset_local_irq_state() {
    LOCAL_IRQ_ENABLED.set(true);
    PREEMPT_EXIT_LOCAL_IRQ_ENABLED.with(|observed| observed.set(None));
    IRQ_RETURN_EXIT_LOCAL_IRQ_ENABLED.with(|observed| observed.set(None));
}

pub(crate) fn local_irqs_enabled() -> bool {
    LOCAL_IRQ_ENABLED.with(Cell::get)
}

pub(crate) fn preempt_exit_local_irqs_enabled() -> Option<bool> {
    PREEMPT_EXIT_LOCAL_IRQ_ENABLED.with(Cell::get)
}

pub(crate) fn irq_return_exit_local_irqs_enabled() -> Option<bool> {
    IRQ_RETURN_EXIT_LOCAL_IRQ_ENABLED.with(Cell::get)
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

pub(crate) fn reset_local_scheduler_work_publications() {
    LOCAL_SCHEDULER_WORK_PUBLICATIONS.with(|count| count.set(0));
}

pub(crate) fn local_scheduler_work_publications() -> usize {
    LOCAL_SCHEDULER_WORK_PUBLICATIONS.with(Cell::get)
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

#[cfg(feature = "lockdep")]
pub(crate) struct InstalledDefaultTaskRuntime {
    _cpu: Pin<Box<crate::CpuLocal>>,
    _system: Pin<Box<crate::TaskSystem>>,
}

#[cfg(feature = "lockdep")]
impl InstalledDefaultTaskRuntime {
    pub(crate) fn new() -> Self {
        let system = Box::pin(
            crate::TaskSystem::new(crate::TaskSystemConfig::new(1))
                .expect("lockdep test task system must initialize"),
        );
        let mut cpu = system
            .create_cpu_local(crate::CpuId::new(0))
            .expect("lockdep test CPU must initialize");
        system
            .install_bootstrap_thread(
                cpu.as_mut(),
                crate::ThreadSpec::new(crate::SchedulePolicy::default()),
            )
            .expect("lockdep test bootstrap thread must initialize");
        system
            .bring_cpu_online(cpu.as_mut())
            .expect("lockdep test CPU must become online");
        install_task_handles(
            (system.as_ref().get_ref() as *const crate::TaskSystem).expose_provenance(),
            // SAFETY: the returned fixture owns this pinned CPU until Drop
            // clears the thread-local runtime handles.
            (unsafe { Pin::get_unchecked_mut(cpu.as_mut()) } as *mut crate::CpuLocal)
                .expose_provenance(),
        );
        Self {
            _cpu: cpu,
            _system: system,
        }
    }
}

#[cfg(feature = "lockdep")]
impl Drop for InstalledDefaultTaskRuntime {
    fn drop(&mut self) {
        clear_task_handles();
    }
}

pub(crate) fn reset_cpu_handle_reads() {
    CPU_LOCAL_HANDLE_READS.with(|reads| reads.set(0));
    CURRENT_CPU_REMOTE_HANDLE_READS.with(|reads| reads.set(0));
    CPU_REMOTE_HANDLE_READS.with(|reads| reads.set(0));
    CPU_OWNER_CLAIMS.with(|claims| claims.set(0));
}

pub(crate) fn cpu_handle_reads() -> (usize, usize) {
    (
        CPU_LOCAL_HANDLE_READS.with(Cell::get),
        CPU_REMOTE_HANDLE_READS.with(Cell::get),
    )
}

pub(crate) fn current_cpu_remote_handle_reads() -> usize {
    CURRENT_CPU_REMOTE_HANDLE_READS.with(Cell::get)
}

pub(crate) fn record_cpu_owner_claim() {
    CPU_OWNER_CLAIMS.with(|claims| claims.set(claims.get() + 1));
}

pub(crate) fn cpu_owner_claims() -> usize {
    CPU_OWNER_CLAIMS.with(Cell::get)
}

pub(crate) fn clear_task_handles() {
    install_task_handles(0, 0);
    reset_cpu_handle_reads();
    MONOTONIC_NS.with(|now| now.set(0));
    SCHEDULER_NS.with(|clocks| clocks.borrow_mut().fill(0));
    HARDIRQ_NS.with(|clocks| clocks.borrow_mut().fill(0));
    MONOTONIC_READS.with(|reads| reads.set(0));
    SCHEDULER_READS.with(|reads| reads.set(0));
    LAST_SCHEDULER_DEADLINE_UPDATE.with(|observed| observed.set(None));
    CPU_LIFECYCLE_EVENTS.with(|events| events.borrow_mut().clear());
    configure_cpu_lifecycle(RuntimeStatus::Success, RuntimeStatus::Success);
}

pub(crate) fn set_monotonic_ns(now_ns: u64) {
    MONOTONIC_NS.with(|now| now.set(now_ns));
}

pub(crate) fn set_scheduler_ns(now_ns: u64) {
    set_scheduler_ns_for_cpu(0, now_ns);
}

pub(crate) fn set_scheduler_ns_for_cpu(cpu: u32, now_ns: u64) {
    SCHEDULER_NS.with(|clocks| {
        *clocks
            .borrow_mut()
            .get_mut(cpu as usize)
            .expect("test scheduler CPU must fit the fake clock table") = now_ns;
    });
}

pub(crate) fn reset_monotonic_reads() {
    MONOTONIC_READS.with(|reads| reads.set(0));
}

pub(crate) fn monotonic_reads() -> usize {
    MONOTONIC_READS.with(Cell::get)
}

pub(crate) fn reset_scheduler_reads() {
    SCHEDULER_READS.with(|reads| reads.set(0));
}

pub(crate) fn scheduler_reads() -> usize {
    SCHEDULER_READS.with(Cell::get)
}

pub(crate) fn take_scheduler_deadline_update() -> Option<SchedulerDeadlineUpdate> {
    LAST_SCHEDULER_DEADLINE_UPDATE.with(Cell::take)
}

pub(crate) fn take_cpu_lifecycle_events() -> std::vec::Vec<CpuLifecycleEvent> {
    CPU_LIFECYCLE_EVENTS.with(|events| core::mem::take(&mut *events.borrow_mut()))
}

pub(crate) fn configure_cpu_lifecycle(online_status: RuntimeStatus, offline_status: RuntimeStatus) {
    CPU_ONLINE_STATUS.with(|status| status.set(online_status));
    CPU_OFFLINE_STATUS.with(|status| status.set(offline_status));
}
