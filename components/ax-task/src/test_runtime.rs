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
}

struct UnitTestRuntime;

#[crate::runtime::impl_extern_trait(name = "ax-task_0_7", abi = "rust")]
impl TaskRuntime for UnitTestRuntime {
    fn task_system_handle() -> TaskSystemHandle {
        TASK_SYSTEM_HANDLE.with(|handle| TaskSystemHandle::from_raw(handle.get()))
    }
    fn current_cpu_local_handle() -> CpuLocalHandle {
        CPU_LOCAL_HANDLE.with(|handle| CpuLocalHandle::from_raw(handle.get()))
    }
    fn cpu_local_handle(cpu: RuntimeCpuId) -> CpuLocalHandle {
        if cpu.as_u32() == 0 {
            Self::current_cpu_local_handle()
        } else {
            CpuLocalHandle::NONE
        }
    }
    fn current_cpu_id() -> RuntimeCpuId {
        RuntimeCpuId::new(0)
    }
    fn online_cpu_count() -> u32 {
        1
    }

    fn irq_guard_enter() -> IrqGuardToken {
        let scheduler_depth = SCHEDULER_FRAME_DEPTH.with(Cell::get);
        IRQ_ENTER_SCHEDULER_FRAME_DEPTH.with(|observed| observed.set(scheduler_depth));
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        ACTIVE_IRQ_TOKENS.with(|tokens| tokens.borrow_mut().push(token));
        IrqGuardToken::from_raw(token)
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
    }

    fn finish_initial_context_switch() {
        ACTIVE_IRQ_TOKENS.with(|tokens| {
            tokens
                .borrow_mut()
                .pop()
                .expect("initial context must inherit one IRQ guard");
        });
    }

    fn scheduler_frame_guard_enter() {
        SCHEDULER_FRAME_DEPTH.with(|depth| {
            let next = depth
                .get()
                .checked_add(1)
                .expect("test scheduler frame overflow");
            depth.set(next);
            MAX_SCHEDULER_FRAME_DEPTH.with(|maximum| maximum.set(maximum.get().max(next)));
        });
    }

    fn scheduler_frame_guard_exit() {
        SCHEDULER_FRAME_DEPTH.with(|depth| {
            let current = depth.get();
            assert!(current > 0, "unbalanced test scheduler frame exit");
            depth.set(current - 1);
        });
    }

    fn in_hard_irq() -> bool {
        false
    }
    fn monotonic_ns() -> u64 {
        0
    }
    fn timer_resolution_ns() -> u64 {
        1
    }
    fn program_oneshot_timer(_deadline_ns: u64) -> RuntimeStatus {
        RuntimeStatus::Success
    }
    fn send_scheduler_ipi(_cpu: RuntimeCpuId) -> RuntimeStatus {
        RuntimeStatus::Success
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
    fn destroy_context(_context: ExecutionContextHandle) -> RuntimeStatus {
        RuntimeStatus::Unsupported
    }
    unsafe fn switch_context(_previous: ExecutionContextHandle, _next: ExecutionContextHandle) {
        panic!("unit-test runtime has no execution contexts")
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
}

pub(crate) fn scheduler_frame_state() -> (usize, usize, usize) {
    (
        SCHEDULER_FRAME_DEPTH.with(Cell::get),
        MAX_SCHEDULER_FRAME_DEPTH.with(Cell::get),
        IRQ_ENTER_SCHEDULER_FRAME_DEPTH.with(Cell::get),
    )
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
