//! Per-integration-binary fake TaskRuntime.

use core::{
    cell::{Cell, RefCell},
    pin::Pin,
};

use ax_task::{
    CpuId, CpuLocal, CpuRemote, TaskSystem, impl_trait,
    runtime::{TaskRuntime, *},
};

mod virtual_runtime;
use virtual_runtime::{VirtualIdleState, VirtualRuntimeState};
pub use virtual_runtime::{VirtualRuntimeEvent, VirtualRuntimeEventKind};

const MAX_TEST_CPUS: usize = 8;
const MAX_ACTIVE_GUARDS: usize = 64;

#[derive(Clone, Copy)]
struct ActiveGuardTokens {
    slots: [usize; MAX_ACTIVE_GUARDS],
    len: usize,
}

impl ActiveGuardTokens {
    const fn new() -> Self {
        Self {
            slots: [0; MAX_ACTIVE_GUARDS],
            len: 0,
        }
    }

    fn push(&mut self, token: usize) {
        assert!(self.len < MAX_ACTIVE_GUARDS, "test guard nesting overflow");
        self.slots[self.len] = token;
        self.len += 1;
    }

    fn remove(&mut self, token: usize, kind: &str) {
        let index = self.slots[..self.len]
            .iter()
            .position(|active| *active == token)
            .unwrap_or_else(|| panic!("integration {kind} token must be active"));
        self.len -= 1;
        self.slots[index] = self.slots[self.len];
        self.slots[self.len] = 0;
    }

    const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

std::thread_local! {
    // Every integration fixture installs borrowed object addresses only for its
    // own host test thread. Keeping the complete fake runtime thread-local
    // prevents parallel tests from observing another fixture or a pointer after
    // that fixture has been destroyed.
    static NEXT_TOKEN: Cell<usize> = const { Cell::new(1) };
    static TASK_SYSTEM: Cell<usize> = const { Cell::new(0) };
    static CPU_LOCALS: RefCell<[usize; MAX_TEST_CPUS]> = const { RefCell::new([0; MAX_TEST_CPUS]) };
    static CPU_REMOTES: RefCell<[usize; MAX_TEST_CPUS]> = const { RefCell::new([0; MAX_TEST_CPUS]) };
    static VIRTUAL_RUNTIME: RefCell<VirtualRuntimeState> = const { RefCell::new(VirtualRuntimeState::new()) };
    static ONLINE_CPU_COUNT: Cell<usize> = const { Cell::new(1) };
    static DESTROYED_CONTEXTS: Cell<usize> = const { Cell::new(0) };
    static DESTROYED_ADDRESS_SPACES: Cell<usize> = const { Cell::new(0) };
    static DEALLOCATED_STACKS: Cell<usize> = const { Cell::new(0) };
    static DEALLOCATED_TLS: Cell<usize> = const { Cell::new(0) };
    static ACTIVE_IRQ_TOKENS: RefCell<ActiveGuardTokens> = const { RefCell::new(ActiveGuardTokens::new()) };
    static ACTIVE_PREEMPT_TOKENS: RefCell<ActiveGuardTokens> = const { RefCell::new(ActiveGuardTokens::new()) };
    static CURRENT_CPU: Cell<u32> = const { Cell::new(0) };
    static IN_HARD_IRQ: Cell<bool> = const { Cell::new(false) };
    static LAST_ONESHOT_NS: Cell<u64> = const { Cell::new(0) };
    static LAST_DEADLINE_GENERATION: Cell<u64> = const { Cell::new(0) };
    static LAST_DEFERRED_WORK: Cell<bool> = const { Cell::new(false) };
    static TIMER_RESOLUTION_NS: Cell<u64> = const { Cell::new(1) };
    static MONOTONIC_NS: Cell<u64> = const { Cell::new(0) };
}

struct IntegrationRuntime;

impl_trait! {
    impl TaskRuntime for IntegrationRuntime {
        unsafe fn task_system_handle() -> TaskSystemHandle {
            TASK_SYSTEM.with(|handle| {
                // SAFETY: each fixture keeps its pinned TaskSystem alive until
                // clearing this thread-local handle.
                unsafe { TaskSystemHandle::from_raw(handle.get()) }
            })
        }

        unsafe fn current_cpu_local_handle() -> CurrentCpuLocalHandle {
            let index = CURRENT_CPU.with(|cpu| cpu.get() as usize);
            let raw = CPU_LOCALS.with(|handles| handles.borrow().get(index).copied().unwrap_or(0));
            // SAFETY: the fixture publishes only the selected CPU's pinned
            // CpuLocal and clears every entry before destroying the objects.
            unsafe { CurrentCpuLocalHandle::from_raw(raw) }
        }

        unsafe fn current_cpu_remote_handle() -> CpuRemoteHandle {
            let index = CURRENT_CPU.with(|cpu| cpu.get() as usize);
            let raw = CPU_REMOTES.with(|handles| handles.borrow().get(index).copied().unwrap_or(0));
            // SAFETY: each fixture retains the TaskSystem that owns every
            // installed current-CPU endpoint until clear_handles.
            unsafe { CpuRemoteHandle::from_raw(raw) }
        }

        unsafe fn cpu_remote_handle(cpu: RuntimeCpuId) -> CpuRemoteHandle {
            let raw = TASK_SYSTEM.with(Cell::get);
            if raw == 0 {
                return CpuRemoteHandle::NONE;
            }
            // SAFETY: each fixture keeps its pinned TaskSystem alive until it
            // clears these thread-local handles.
            let system = unsafe { &*core::ptr::with_exposed_provenance::<TaskSystem>(raw) };
            system
                .cpu_remote(CpuId::new(cpu.as_u32()))
                .map_or(CpuRemoteHandle::NONE, |remote| {
                    // SAFETY: CpuRemote is Arc-backed by the fixture-owned
                    // TaskSystem for the complete published-handle lifetime.
                    unsafe {
                        CpuRemoteHandle::from_raw(
                            (remote as *const CpuRemote).expose_provenance(),
                        )
                    }
                })
        }

        unsafe fn current_cpu_id() -> RuntimeCpuId {
            CURRENT_CPU.with(|cpu| RuntimeCpuId::new(cpu.get()))
        }
        fn online_cpu_count() -> u32 {
            ONLINE_CPU_COUNT.with(|count| count.get() as u32)
        }

        fn prepare_cpu_online(_cpu: RuntimeCpuId) -> RuntimeStatus {
            VIRTUAL_RUNTIME.with(|runtime| {
                let mut runtime = runtime.borrow_mut();
                let Some(cpu) = runtime.cpu_mut(_cpu.as_u32()) else {
                    return RuntimeStatus::InvalidArgument;
                };
                cpu.online = true;
                RuntimeStatus::Success
            })
        }

        fn prepare_cpu_offline(_cpu: RuntimeCpuId) -> RuntimeStatus {
            VIRTUAL_RUNTIME.with(|runtime| {
                let mut runtime = runtime.borrow_mut();
                let Some(cpu) = runtime.cpu_mut(_cpu.as_u32()) else {
                    return RuntimeStatus::InvalidArgument;
                };
                if cpu.ipi_edge_pending
                    || cpu.scheduler_work_pending
                    || cpu.switch_tail_pending
                    || cpu.scheduler_frame_depth != 0
                {
                    return RuntimeStatus::Busy;
                }
                cpu.online = false;
                RuntimeStatus::Success
            })
        }

        fn irq_guard_enter() -> IrqGuardToken {
            let token = NEXT_TOKEN.with(|next| {
                let token = next.get();
                next.set(token.wrapping_add(1).max(1));
                token
            });
            ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow_mut().push(token));
            // SAFETY: the token is present in ACTIVE_IRQ_TOKENS until the
            // matching test-runtime exit operation consumes it.
            unsafe { IrqGuardToken::from_raw(token) }
        }

        unsafe fn irq_guard_exit(token: IrqGuardToken) {
            ACTIVE_IRQ_TOKENS.with(|tokens| {
                tokens.borrow_mut().remove(token.into_raw(), "IRQ");
            });
        }

        fn preempt_guard_enter() -> PreemptGuardToken {
            let token = NEXT_TOKEN.with(|next| {
                let token = next.get();
                next.set(token.wrapping_add(1).max(1));
                token
            });
            ACTIVE_PREEMPT_TOKENS.with(|tokens| tokens.borrow_mut().push(token));
            // SAFETY: the token remains active until the matching fake-runtime
            // exit consumes it on this host execution context.
            unsafe { PreemptGuardToken::from_raw(token) }
        }

        unsafe fn preempt_guard_exit(token: PreemptGuardToken) {
            ACTIVE_PREEMPT_TOKENS.with(|tokens| {
                tokens.borrow_mut().remove(token.into_raw(), "preempt");
            });
        }

        fn publish_local_scheduler_work() -> bool {
            let cpu = CURRENT_CPU.with(Cell::get);
            VIRTUAL_RUNTIME.with(|runtime| {
                runtime
                    .borrow_mut()
                    .publish_scheduler_work(cpu)
                    .expect("current virtual CPU must exist");
            });
            IN_HARD_IRQ.with(Cell::get)
        }

        fn finish_context_switch_tail() {
            let cpu = CURRENT_CPU.with(Cell::get);
            VIRTUAL_RUNTIME.with(|runtime| {
                let mut runtime = runtime.borrow_mut();
                let outgoing = {
                    let state = runtime
                        .cpu_mut(cpu)
                        .expect("current virtual CPU must exist");
                    if state.switch_tail_pending {
                        state.switch_tail_pending = false;
                        core::mem::take(&mut state.outgoing_context)
                    } else {
                        // Core-only scheduler tests invoke the completion API
                        // after directly applying a ScheduleDecision, without
                        // crossing the facade's architecture switch boundary.
                        // Keep that transition explicit in the trace while the
                        // end-to-end virtual-runtime tests require a non-zero
                        // outgoing context.
                        0
                    }
                };
                runtime.record(
                    cpu,
                    VirtualRuntimeEventKind::SwitchTailCompleted,
                    0,
                    outgoing,
                    0,
                );
            });
        }

        fn finish_initial_context_switch() {
            // Integration tests do not execute real architecture context
            // switches; their scheduler baton is modeled by the facade tests.
        }

        fn scheduler_frame_guard_enter(
            _origin: RuntimeScheduleOrigin,
            _entry: RuntimeSchedulerEntry,
        ) -> RuntimeStatus {
            let cpu = CURRENT_CPU.with(Cell::get);
            VIRTUAL_RUNTIME.with(|runtime| {
                let mut runtime = runtime.borrow_mut();
                let depth = {
                    let state = runtime
                        .cpu_mut(cpu)
                        .expect("current virtual CPU must exist");
                    state.scheduler_work_pending = false;
                    state.scheduler_frame_depth = state
                        .scheduler_frame_depth
                        .checked_add(1)
                        .expect("virtual scheduler frame depth exhausted");
                    state.scheduler_frame_depth as u64
                };
                runtime.record(
                    cpu,
                    VirtualRuntimeEventKind::SchedulerFrameEntered,
                    depth,
                    0,
                    0,
                );
            });
            RuntimeStatus::Success
        }

        fn scheduler_frame_guard_exit(_return_to: RuntimeSchedulerReturn) -> bool {
            let cpu = CURRENT_CPU.with(Cell::get);
            VIRTUAL_RUNTIME.with(|runtime| {
                let mut runtime = runtime.borrow_mut();
                let depth = {
                    let state = runtime
                        .cpu_mut(cpu)
                        .expect("current virtual CPU must exist");
                    state.scheduler_frame_depth = state
                        .scheduler_frame_depth
                        .checked_sub(1)
                        .expect("virtual scheduler frame exit without entry");
                    state.scheduler_frame_depth as u64
                };
                runtime.record(
                    cpu,
                    VirtualRuntimeEventKind::SchedulerFrameExited,
                    depth,
                    0,
                    0,
                );
            });
            ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow().is_empty())
        }

        fn in_hard_irq() -> bool { IN_HARD_IRQ.with(Cell::get) }
        fn validate_schedule_context(_origin: RuntimeScheduleOrigin) -> RuntimeStatus {
            if IN_HARD_IRQ.with(Cell::get) {
                RuntimeStatus::UnsafeContext
            } else {
                RuntimeStatus::Success
            }
        }
        fn validate_owner_cpu_context() -> RuntimeStatus {
            RuntimeStatus::Success
        }
        fn monotonic_ns() -> u64 { MONOTONIC_NS.with(Cell::get) }
        fn timer_resolution_ns() -> u64 { TIMER_RESOLUTION_NS.with(Cell::get) }
        fn publish_task_deadline(update: TaskDeadlineUpdate) {
            LAST_ONESHOT_NS.with(|deadline| {
                deadline.set(update.deadline().map_or(0, MonotonicDeadline::as_nanos))
            });
            LAST_DEADLINE_GENERATION.with(|generation| generation.set(update.generation()));
            LAST_DEFERRED_WORK.with(|pending| pending.set(update.deferred_work()));
        }
        fn send_scheduler_ipi(cpu: RuntimeCpuId) -> RuntimeStatus {
            VIRTUAL_RUNTIME.with(|runtime| runtime.borrow_mut().publish_ipi(cpu.as_u32()))
        }
        fn wait_for_interrupt() {
            let cpu = CURRENT_CPU.with(Cell::get);
            let remote_has_work = CPU_REMOTES.with(|handles| {
                let raw = handles.borrow()[cpu as usize];
                if raw == 0 {
                    return false;
                }
                // SAFETY: the fixture keeps every installed Arc-backed remote
                // endpoint alive until clear_handles.
                let remote = unsafe {
                    &*core::ptr::with_exposed_provenance::<CpuRemote>(raw)
                };
                remote.needs_reschedule()
            });
            VIRTUAL_RUNTIME.with(|runtime| {
                let mut runtime = runtime.borrow_mut();
                let (kind, generation) = {
                    let state = runtime
                        .cpu_mut(cpu)
                        .expect("current virtual CPU must exist");
                    if remote_has_work
                        || state.scheduler_work_pending
                        || state.ipi_edge_pending
                        || state.ipi_published_epoch != state.ipi_claimed_epoch
                    {
                        (VirtualRuntimeEventKind::IdleCommitAborted, state.ipi_published_epoch)
                    } else {
                        state.idle_state = VirtualIdleState::Sleeping;
                        (VirtualRuntimeEventKind::IdleCommitted, state.ipi_published_epoch)
                    }
                };
                runtime.record(cpu, kind, generation, 0, 0);
                // The deterministic hook returns only after a virtual wake.
                // Keeping Sleeping observable solely in the ordered event log
                // prevents a host test thread from impersonating two CPUs at once.
                runtime
                    .cpu_mut(cpu)
                    .expect("current virtual CPU must exist")
                    .idle_state = VirtualIdleState::Running;
            });
        }
        fn allocate_stack(_request: StackRequest) -> RuntimeHandleResult {
            RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
        }
        fn deallocate_stack(_stack: StackHandle) {
            DEALLOCATED_STACKS.with(|count| count.set(count.get() + 1));
        }
        fn allocate_tls(_request: TlsRequest) -> RuntimeHandleResult {
            RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
        }
        fn deallocate_tls(_tls: TlsHandle) {
            DEALLOCATED_TLS.with(|count| count.set(count.get() + 1));
        }
        fn create_kernel_context(_request: KernelContextRequest) -> RuntimeHandleResult {
            RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
        }
        fn create_user_context(_request: UserContextRequest) -> RuntimeHandleResult {
            RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
        }
        fn bind_context_thread(_binding: ContextThreadBinding) -> RuntimeStatus {
            RuntimeStatus::Success
        }
        fn destroy_context(_context: ExecutionContextHandle) {
            DESTROYED_CONTEXTS.with(|count| count.set(count.get() + 1));
        }
        fn destroy_address_space(
            _address_space: AddressSpaceHandle,
        ) -> AddressSpaceDestroyOutcome {
            DESTROYED_ADDRESS_SPACES.with(|count| count.set(count.get() + 1));
            AddressSpaceDestroyOutcome::Released
        }
        fn arm_address_space_reclaim(
            _address_space: AddressSpaceHandle,
        ) -> AddressSpaceReclaimArmOutcome {
            AddressSpaceReclaimArmOutcome::Ready
        }
        unsafe fn switch_context(
            previous: ExecutionContextHandle,
            next: ExecutionContextHandle,
        ) {
            let cpu = CURRENT_CPU.with(Cell::get);
            VIRTUAL_RUNTIME.with(|runtime| {
                let mut runtime = runtime.borrow_mut();
                let (previous_raw, next_raw) = (previous.into_raw(), next.into_raw());
                {
                    let state = runtime
                        .cpu_mut(cpu)
                        .expect("current virtual CPU must exist");
                    assert_ne!(state.scheduler_frame_depth, 0, "context switch requires scheduler baton");
                    assert!(!state.switch_tail_pending, "previous switch tail must complete before another switch");
                    assert_ne!(previous_raw, 0, "previous context must be live");
                    assert_ne!(next_raw, 0, "next context must be live");
                    if state.current_context == 0 {
                        state.current_context = previous_raw;
                    }
                    assert_eq!(
                        state.current_context, previous_raw,
                        "context switch must depart from the CPU's published runtime context"
                    );
                    state.outgoing_context = previous_raw;
                    state.current_context = next_raw;
                    state.switch_tail_pending = true;
                }
                runtime.record(
                    cpu,
                    VirtualRuntimeEventKind::ContextSwitched,
                    0,
                    previous_raw,
                    next_raw,
                );
            });
        }
        fn activate_address_space(_activation: AddressSpaceActivation) -> RuntimeStatus {
            RuntimeStatus::Success
        }
        fn flush_tlb_local(_start: usize, _size: usize) {}
        fn trace_sched_switch(_record: SchedSwitchRecord) {}
        fn fatal_invariant(_code: u32, _argument: usize) -> ! {
            panic!("scheduler invariant reported by integration test")
        }
    }
}

pub fn install_handles(task_system: usize, cpu_local: Pin<&mut CpuLocal>) {
    TASK_SYSTEM.with(|handle| handle.set(task_system));
    install_cpu_raw(0, owner_cpu_handle(cpu_local));
    install_cpu_remote_raw(0, task_system);
    CURRENT_CPU.with(|cpu| cpu.set(0));
    ONLINE_CPU_COUNT.with(|count| count.set(1));
}

pub fn install_cpu(cpu: u32, cpu_local: Pin<&mut CpuLocal>) {
    install_cpu_raw(cpu, owner_cpu_handle(cpu_local));
    let task_system = TASK_SYSTEM.with(Cell::get);
    install_cpu_remote_raw(cpu, task_system);
    VIRTUAL_RUNTIME.with(|runtime| {
        runtime
            .borrow_mut()
            .cpu_mut(cpu)
            .expect("installed virtual CPU must fit the test topology")
            .online = true;
    });
}

// Every integration-test crate compiles this shared runtime provider as its
// own module. Keep both typed installation entry points part of that provider
// even when a particular test exercises only the global facade.
const _: fn(usize, Pin<&mut CpuLocal>) = install_handles;
const _: fn(u32, Pin<&mut CpuLocal>) = install_cpu;
const _: fn() -> (u64, u64, bool) = last_task_deadline_update;
const _: fn(u32) -> usize = ipi_count;
const _: fn(u32) -> bool = consume_ipi;
const _: fn(u32) -> bool = dispatch_scheduler_ipi;
const _: fn(u32) -> bool = local_scheduler_work_pending;
const _: fn() -> bool = consume_local_scheduler_work;
const _: fn(u32) = set_current_cpu;
const _: fn() -> Vec<VirtualRuntimeEvent> = virtual_runtime_events;
const _: fn() = clear_virtual_runtime_events;

/// Exposes the mutable provenance of a pinned owner-CPU scheduler object.
fn owner_cpu_handle(cpu: Pin<&mut CpuLocal>) -> usize {
    // SAFETY: test fixtures keep the allocation pinned and serialize every
    // owner access until they clear the installed fake-runtime handle.
    (unsafe { Pin::get_unchecked_mut(cpu) } as *mut CpuLocal).expose_provenance()
}

fn install_cpu_raw(cpu: u32, cpu_local: usize) {
    CPU_LOCALS.with(|handles| handles.borrow_mut()[cpu as usize] = cpu_local);
}

fn install_cpu_remote_raw(cpu: u32, task_system: usize) {
    let remote = if task_system == 0 {
        0
    } else {
        // SAFETY: install/clear bracket the fixture-owned TaskSystem lifetime.
        let system = unsafe { &*core::ptr::with_exposed_provenance::<TaskSystem>(task_system) };
        system.runtime_cpu_remote_handle(CpuId::new(cpu)).into_raw()
    };
    CPU_REMOTES.with(|handles| handles.borrow_mut()[cpu as usize] = remote);
}

pub fn set_online_cpu_count(count: usize) {
    ONLINE_CPU_COUNT.with(|online| online.set(count));
    VIRTUAL_RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        for (index, cpu) in runtime.cpus.iter_mut().enumerate() {
            cpu.online = index < count;
        }
    });
}

pub fn set_hard_irq(in_hard_irq: bool) {
    IN_HARD_IRQ.with(|state| state.set(in_hard_irq));
}

pub fn ipi_count(cpu: u32) -> usize {
    VIRTUAL_RUNTIME.with(|runtime| {
        runtime
            .borrow()
            .cpu(cpu)
            .expect("queried virtual CPU must fit the test topology")
            .ipi_send_count
    })
}

pub fn consume_ipi(cpu: u32) -> bool {
    VIRTUAL_RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        if runtime.claim_ipi(cpu).is_none() {
            return false;
        }
        runtime
            .publish_scheduler_work(cpu)
            .expect("claimed virtual IPI must target a valid CPU");
        true
    })
}

/// Dispatches one physical scheduler IPI edge on its target CPU.
pub fn dispatch_scheduler_ipi(cpu: u32) -> bool {
    consume_ipi(cpu)
}

/// Reports scheduler work published locally by one virtual CPU.
pub fn local_scheduler_work_pending(cpu: u32) -> bool {
    VIRTUAL_RUNTIME.with(|runtime| {
        runtime
            .borrow()
            .cpu(cpu)
            .is_some_and(|state| state.scheduler_work_pending)
    })
}

pub fn consume_local_scheduler_work() -> bool {
    let cpu = CURRENT_CPU.with(Cell::get);
    VIRTUAL_RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let Some(state) = runtime.cpu_mut(cpu) else {
            return false;
        };
        core::mem::replace(&mut state.scheduler_work_pending, false)
    })
}

pub fn set_current_cpu(cpu: u32) {
    assert!(
        (cpu as usize) < MAX_TEST_CPUS,
        "virtual CPU is out of range"
    );
    CURRENT_CPU.with(|current| current.set(cpu));
}

pub fn virtual_runtime_events() -> Vec<VirtualRuntimeEvent> {
    VIRTUAL_RUNTIME.with(|runtime| runtime.borrow().events())
}

pub fn clear_virtual_runtime_events() {
    VIRTUAL_RUNTIME.with(|runtime| runtime.borrow_mut().clear_events());
}

pub fn resource_release_counts() -> (usize, usize, usize, usize) {
    (
        DESTROYED_CONTEXTS.with(Cell::get),
        DESTROYED_ADDRESS_SPACES.with(Cell::get),
        DEALLOCATED_STACKS.with(Cell::get),
        DEALLOCATED_TLS.with(Cell::get),
    )
}

pub fn last_oneshot_ns() -> u64 {
    LAST_ONESHOT_NS.with(Cell::get)
}

pub fn last_task_deadline_update() -> (u64, u64, bool) {
    (
        LAST_DEADLINE_GENERATION.with(Cell::get),
        LAST_ONESHOT_NS.with(Cell::get),
        LAST_DEFERRED_WORK.with(Cell::get),
    )
}

pub fn set_timer_resolution_ns(resolution_ns: u64) {
    TIMER_RESOLUTION_NS.with(|resolution| resolution.set(resolution_ns));
}

pub fn set_monotonic_ns(now_ns: u64) {
    MONOTONIC_NS.with(|now| now.set(now_ns));
}

pub fn reset_resource_release_counts() {
    DESTROYED_CONTEXTS.with(|count| count.set(0));
    DESTROYED_ADDRESS_SPACES.with(|count| count.set(0));
    DEALLOCATED_STACKS.with(|count| count.set(0));
    DEALLOCATED_TLS.with(|count| count.set(0));
}

pub fn clear_handles() {
    TASK_SYSTEM.with(|handle| handle.set(0));
    for cpu in 0..MAX_TEST_CPUS as u32 {
        install_cpu_raw(cpu, 0);
        install_cpu_remote_raw(cpu, 0);
    }
    VIRTUAL_RUNTIME.with(|runtime| *runtime.borrow_mut() = VirtualRuntimeState::new());
    CURRENT_CPU.with(|cpu| cpu.set(0));
    set_hard_irq(false);
    set_online_cpu_count(1);
    reset_resource_release_counts();
    LAST_ONESHOT_NS.with(|deadline| deadline.set(0));
    LAST_DEADLINE_GENERATION.with(|generation| generation.set(0));
    LAST_DEFERRED_WORK.with(|pending| pending.set(false));
    let _cleared_oneshot = last_oneshot_ns();
    set_timer_resolution_ns(1);
    set_monotonic_ns(0);
    let _reset_counts = resource_release_counts();
}
