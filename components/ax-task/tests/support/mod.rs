//! Per-integration-binary fake TaskRuntime.

use core::cell::{Cell, RefCell};

use ax_task::{
    impl_trait,
    runtime::{TaskRuntime, *},
};

const MAX_TEST_CPUS: usize = 8;

std::thread_local! {
    // Every integration fixture installs borrowed object addresses only for its
    // own host test thread. Keeping the complete fake runtime thread-local
    // prevents parallel tests from observing another fixture or a pointer after
    // that fixture has been destroyed.
    static NEXT_TOKEN: Cell<usize> = const { Cell::new(1) };
    static TASK_SYSTEM: Cell<usize> = const { Cell::new(0) };
    static CPU_LOCALS: RefCell<[usize; MAX_TEST_CPUS]> = const { RefCell::new([0; MAX_TEST_CPUS]) };
    static IPI_COUNTS: RefCell<[usize; MAX_TEST_CPUS]> = const { RefCell::new([0; MAX_TEST_CPUS]) };
    static ONLINE_CPU_COUNT: Cell<usize> = const { Cell::new(1) };
    static DESTROYED_CONTEXTS: Cell<usize> = const { Cell::new(0) };
    static DEALLOCATED_STACKS: Cell<usize> = const { Cell::new(0) };
    static DEALLOCATED_TLS: Cell<usize> = const { Cell::new(0) };
    static ACTIVE_IRQ_TOKENS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    static CURRENT_CPU: Cell<u32> = const { Cell::new(0) };
    static IN_HARD_IRQ: Cell<bool> = const { Cell::new(false) };
    static LAST_ONESHOT_NS: Cell<u64> = const { Cell::new(0) };
    static TIMER_RESOLUTION_NS: Cell<u64> = const { Cell::new(1) };
    static MONOTONIC_NS: Cell<u64> = const { Cell::new(0) };
}

struct IntegrationRuntime;

impl_trait! {
    impl TaskRuntime for IntegrationRuntime {
        fn task_system_handle() -> TaskSystemHandle {
            TASK_SYSTEM.with(|handle| TaskSystemHandle::from_raw(handle.get()))
        }

        fn current_cpu_local_handle() -> CpuLocalHandle {
            CURRENT_CPU.with(|cpu| Self::cpu_local_handle(RuntimeCpuId::new(cpu.get())))
        }

        fn cpu_local_handle(cpu: RuntimeCpuId) -> CpuLocalHandle {
            let index = cpu.as_u32() as usize;
            let raw = CPU_LOCALS.with(|handles| handles.borrow().get(index).copied().unwrap_or(0));
            CpuLocalHandle::from_raw(raw)
        }

        fn current_cpu_id() -> RuntimeCpuId {
            CURRENT_CPU.with(|cpu| RuntimeCpuId::new(cpu.get()))
        }
        fn online_cpu_count() -> u32 {
            ONLINE_CPU_COUNT.with(|count| count.get() as u32)
        }

        fn irq_guard_enter() -> IrqGuardToken {
            let token = NEXT_TOKEN.with(|next| {
                let token = next.get();
                next.set(token.wrapping_add(1).max(1));
                token
            });
            ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow_mut().push(token));
            IrqGuardToken::from_raw(token)
        }

        unsafe fn irq_guard_exit(token: IrqGuardToken) {
            ACTIVE_IRQ_TOKENS.with(|tokens| {
                let mut tokens = tokens.borrow_mut();
                let index = tokens
                    .iter()
                    .position(|active| *active == token.into_raw())
                    .expect("integration IRQ token must be active");
                tokens.swap_remove(index);
            });
        }

        fn finish_initial_context_switch() {
            ACTIVE_IRQ_TOKENS.with(|tokens| {
                tokens
                    .borrow_mut()
                    .pop()
                    .expect("initial context must inherit one IRQ guard");
            });
        }

        fn scheduler_frame_guard_enter() {}

        fn scheduler_frame_guard_exit() {}

        fn in_hard_irq() -> bool { IN_HARD_IRQ.with(Cell::get) }
        fn monotonic_ns() -> u64 { MONOTONIC_NS.with(Cell::get) }
        fn timer_resolution_ns() -> u64 { TIMER_RESOLUTION_NS.with(Cell::get) }
        fn program_oneshot_timer(deadline_ns: u64) -> RuntimeStatus {
            LAST_ONESHOT_NS.with(|deadline| deadline.set(deadline_ns));
            RuntimeStatus::Success
        }
        fn send_scheduler_ipi(cpu: RuntimeCpuId) -> RuntimeStatus {
            IPI_COUNTS.with(|counts| {
                let mut counts = counts.borrow_mut();
                let Some(count) = counts.get_mut(cpu.as_u32() as usize) else {
                    return RuntimeStatus::InvalidArgument;
                };
                *count = count.checked_add(1).expect("integration IPI count overflow");
                RuntimeStatus::Success
            })
        }
        fn wait_for_interrupt() {}
        fn allocate_stack(_request: StackRequest) -> RuntimeHandleResult {
            RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
        }
        fn deallocate_stack(_stack: StackHandle) -> RuntimeStatus {
            DEALLOCATED_STACKS.with(|count| count.set(count.get() + 1));
            RuntimeStatus::Success
        }
        fn allocate_tls(_request: TlsRequest) -> RuntimeHandleResult {
            RuntimeHandleResult::failure(RuntimeStatus::Unsupported)
        }
        fn deallocate_tls(_tls: TlsHandle) -> RuntimeStatus {
            DEALLOCATED_TLS.with(|count| count.set(count.get() + 1));
            RuntimeStatus::Success
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
        fn destroy_context(_context: ExecutionContextHandle) -> RuntimeStatus {
            DESTROYED_CONTEXTS.with(|count| count.set(count.get() + 1));
            if _context.into_raw() == usize::MAX {
                RuntimeStatus::Busy
            } else {
                RuntimeStatus::Success
            }
        }
        unsafe fn switch_context(
            _previous: ExecutionContextHandle,
            _next: ExecutionContextHandle,
        ) {
            panic!("integration runtime has no execution contexts")
        }
        fn install_address_space(_address_space: AddressSpaceHandle) -> RuntimeStatus {
            RuntimeStatus::Unsupported
        }
        fn flush_tlb_local(_start: usize, _size: usize) {}
        fn trace_sched_switch(_record: SchedSwitchRecord) {}
        fn fatal_invariant(_code: u32, _argument: usize) -> ! {
            panic!("scheduler invariant reported by integration test")
        }
    }
}

pub fn install_handles(task_system: usize, cpu_local: usize) {
    TASK_SYSTEM.with(|handle| handle.set(task_system));
    CPU_LOCALS.with(|handles| handles.borrow_mut()[0] = cpu_local);
    CURRENT_CPU.with(|cpu| cpu.set(0));
    ONLINE_CPU_COUNT.with(|count| count.set(1));
}

pub fn install_cpu(cpu: u32, cpu_local: usize) {
    CPU_LOCALS.with(|handles| handles.borrow_mut()[cpu as usize] = cpu_local);
}

pub fn set_online_cpu_count(count: usize) {
    ONLINE_CPU_COUNT.with(|online| online.set(count));
}

pub fn set_hard_irq(in_hard_irq: bool) {
    IN_HARD_IRQ.with(|state| state.set(in_hard_irq));
}

pub fn ipi_count(cpu: u32) -> usize {
    IPI_COUNTS.with(|counts| counts.borrow()[cpu as usize])
}

pub fn resource_release_counts() -> (usize, usize, usize) {
    (
        DESTROYED_CONTEXTS.with(Cell::get),
        DEALLOCATED_STACKS.with(Cell::get),
        DEALLOCATED_TLS.with(Cell::get),
    )
}

pub fn last_oneshot_ns() -> u64 {
    LAST_ONESHOT_NS.with(Cell::get)
}

pub fn set_timer_resolution_ns(resolution_ns: u64) {
    TIMER_RESOLUTION_NS.with(|resolution| resolution.set(resolution_ns));
}

pub fn set_monotonic_ns(now_ns: u64) {
    MONOTONIC_NS.with(|now| now.set(now_ns));
}

pub fn reset_resource_release_counts() {
    DESTROYED_CONTEXTS.with(|count| count.set(0));
    DEALLOCATED_STACKS.with(|count| count.set(0));
    DEALLOCATED_TLS.with(|count| count.set(0));
}

pub fn clear_handles() {
    install_handles(0, 0);
    for cpu in 0..MAX_TEST_CPUS as u32 {
        install_cpu(cpu, 0);
        IPI_COUNTS.with(|counts| counts.borrow_mut()[cpu as usize] = 0);
        let _cleared = ipi_count(cpu);
    }
    CURRENT_CPU.with(|cpu| cpu.set(0));
    set_hard_irq(false);
    set_online_cpu_count(1);
    reset_resource_release_counts();
    LAST_ONESHOT_NS.with(|deadline| deadline.set(0));
    let _cleared_oneshot = last_oneshot_ns();
    set_timer_resolution_ns(1);
    set_monotonic_ns(0);
    let _reset_counts = resource_release_counts();
}
