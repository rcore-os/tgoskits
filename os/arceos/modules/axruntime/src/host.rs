//! Host execution-context adapter for the production `ax-task::sync` engine.

use core::cell::{Cell, RefCell};
#[cfg(feature = "multitask")]
use std::sync::OnceLock;

#[cfg(feature = "multitask")]
use ax_task::{
    SchedulePolicy, TaskSystem, TaskSystemConfig, ThreadHandle, ThreadSpec,
    runtime::{
        CpuRemoteHandle, CurrentThreadPublication, IrqGuardToken, LocalIrqState, PreemptGuardToken,
        RuntimeCpuId, TaskSystemHandle,
    },
};

#[cfg(feature = "multitask")]
static TASK_SYSTEM: OnceLock<TaskSystem> = OnceLock::new();

std::thread_local! {
    static NEXT_PREEMPT_TOKEN: Cell<usize> = const { Cell::new(1) };
    static ACTIVE_PREEMPT_TOKENS: RefCell<std::vec::Vec<usize>> = const { RefCell::new(std::vec::Vec::new()) };
    static IRQ_ENABLED: Cell<bool> = const { Cell::new(true) };
    static HARDIRQ_DEPTH: Cell<usize> = const { Cell::new(0) };
    #[cfg(feature = "multitask")]
    static NEXT_IRQ_TOKEN: Cell<usize> = const { Cell::new(1) };
    #[cfg(feature = "multitask")]
    static ACTIVE_IRQ_TOKENS: RefCell<std::vec::Vec<usize>> = const { RefCell::new(std::vec::Vec::new()) };
    #[cfg(feature = "multitask")]
    static IRQ_GUARD_BASE_ENABLED: Cell<Option<bool>> = const { Cell::new(None) };
    #[cfg(feature = "multitask")]
    static CURRENT_THREAD: RefCell<Option<ThreadHandle>> = const { RefCell::new(None) };
}

#[cfg(feature = "multitask")]
fn task_system() -> &'static TaskSystem {
    TASK_SYSTEM.get_or_init(|| {
        TaskSystem::new(TaskSystemConfig::new(1)).expect("host sync task system must initialize")
    })
}

#[cfg(feature = "multitask")]
fn with_current_thread<R>(operation: impl FnOnce(&ThreadHandle) -> R) -> R {
    CURRENT_THREAD.with(|slot| {
        if slot.borrow().is_none() {
            let thread = task_system()
                .create_thread(ThreadSpec::new(SchedulePolicy::default()))
                .expect("host sync thread must register");
            *slot.borrow_mut() = Some(thread);
        }
        operation(
            slot.borrow()
                .as_ref()
                .expect("host sync thread must remain registered"),
        )
    })
}

pub(crate) fn preempt_enter() -> usize {
    let token = NEXT_PREEMPT_TOKEN.with(|next| {
        let token = next.get();
        next.set(
            token
                .checked_add(1)
                .expect("host preemption token overflow"),
        );
        token
    });
    ACTIVE_PREEMPT_TOKENS.with(|tokens| {
        tokens.borrow_mut().push(token);
    });
    token
}

pub(crate) unsafe fn preempt_exit(token: usize) {
    ACTIVE_PREEMPT_TOKENS.with(|tokens| {
        let mut tokens = tokens.borrow_mut();
        let index = tokens
            .iter()
            .position(|active| *active == token)
            .expect("host preemption token must be live");
        tokens.swap_remove(index);
    });
}

pub(crate) fn irq_save_and_disable() -> usize {
    IRQ_ENABLED.with(|enabled| usize::from(enabled.replace(false)))
}

pub(crate) unsafe fn irq_restore(state: usize) {
    IRQ_ENABLED.with(|enabled| enabled.set(state != 0));
}

pub(crate) fn hardirq_enter() {
    HARDIRQ_DEPTH.with(|depth| {
        depth.set(
            depth
                .get()
                .checked_add(1)
                .expect("host hardirq depth overflow"),
        );
    });
}

pub(crate) fn hardirq_exit() {
    HARDIRQ_DEPTH.with(|depth| {
        let current = depth.get();
        assert_ne!(current, 0, "host hardirq exit without entry");
        depth.set(current - 1);
    });
}

#[cfg(feature = "multitask")]
pub(crate) fn in_hardirq() -> bool {
    HARDIRQ_DEPTH.with(|depth| depth.get() != 0)
}

#[cfg(feature = "multitask")]
pub(crate) fn task_system_handle() -> TaskSystemHandle {
    let raw = (task_system() as *const TaskSystem).expose_provenance();
    // SAFETY: `TASK_SYSTEM` owns this allocation until process shutdown.
    unsafe { TaskSystemHandle::from_raw(raw) }
}

#[cfg(feature = "multitask")]
pub(crate) fn current_thread_publication() -> CurrentThreadPublication {
    with_current_thread(ThreadHandle::runtime_publication)
}

#[cfg(feature = "multitask")]
pub(crate) fn current_cpu_id() -> RuntimeCpuId {
    RuntimeCpuId::new(0)
}

#[cfg(feature = "multitask")]
pub(crate) fn current_cpu_remote_handle() -> CpuRemoteHandle {
    task_system().runtime_cpu_remote_handle(ax_task::CpuId::new(0))
}

#[cfg(feature = "multitask")]
pub(crate) fn cpu_remote_handle(cpu: RuntimeCpuId) -> CpuRemoteHandle {
    if cpu.as_u32() == 0 {
        current_cpu_remote_handle()
    } else {
        CpuRemoteHandle::NONE
    }
}

#[cfg(feature = "multitask")]
pub(crate) fn local_irq_save_and_disable() -> LocalIrqState {
    // SAFETY: the matching host restore operation accepts this encoded boolean.
    unsafe { LocalIrqState::from_raw(irq_save_and_disable()) }
}

#[cfg(feature = "multitask")]
pub(crate) unsafe fn local_irq_restore(state: LocalIrqState) {
    unsafe { irq_restore(state.into_raw()) };
}

#[cfg(feature = "multitask")]
pub(crate) fn irq_guard_enter() -> IrqGuardToken {
    let token = NEXT_IRQ_TOKEN.with(|next| {
        let token = next.get();
        next.set(token.checked_add(1).expect("host IRQ token overflow"));
        token
    });
    ACTIVE_IRQ_TOKENS.with(|tokens| {
        let mut tokens = tokens.borrow_mut();
        if tokens.is_empty() {
            IRQ_GUARD_BASE_ENABLED.with(|base| {
                let previous = IRQ_ENABLED.with(|enabled| enabled.replace(false));
                base.set(Some(previous));
            });
        }
        tokens.push(token);
    });
    // SAFETY: the token stays live in ACTIVE_IRQ_TOKENS until the matching
    // host IRQ-guard exit consumes it.
    unsafe { IrqGuardToken::from_raw(token) }
}

#[cfg(feature = "multitask")]
pub(crate) unsafe fn irq_guard_exit(token: IrqGuardToken) {
    ACTIVE_IRQ_TOKENS.with(|tokens| {
        let mut tokens = tokens.borrow_mut();
        let index = tokens
            .iter()
            .position(|active| *active == token.into_raw())
            .expect("host IRQ token must be live");
        tokens.swap_remove(index);
        if tokens.is_empty() {
            let base = IRQ_GUARD_BASE_ENABLED
                .with(|base| base.replace(None))
                .expect("host outer IRQ state must be recorded");
            IRQ_ENABLED.with(|enabled| enabled.set(base));
        }
    });
}

#[cfg(feature = "multitask")]
pub(crate) fn preempt_guard_enter() -> PreemptGuardToken {
    // SAFETY: the matching host exit operation consumes this live depth token.
    unsafe { PreemptGuardToken::from_raw(preempt_enter()) }
}

#[cfg(feature = "multitask")]
pub(crate) unsafe fn preempt_guard_exit(token: PreemptGuardToken) {
    unsafe { preempt_exit(token.into_raw()) };
}
