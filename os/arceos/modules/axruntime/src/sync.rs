//! Native ArceOS lock facade and `ax-sync` bridge provider.

#[cfg(feature = "multitask")]
use core::sync::atomic::AtomicU64;
use core::{
    panic::Location,
    sync::atomic::{AtomicBool, AtomicUsize},
};

pub use ax_task::sync::api::*;

fn context_preempt_enter() -> usize {
    #[cfg(feature = "multitask")]
    {
        crate::guard::enter_lock_preempt().map_or(0, ax_hal::percpu::PreemptGuardOwner::into_raw)
    }
    #[cfg(not(feature = "multitask"))]
    {
        0
    }
}

unsafe fn context_preempt_exit(state: usize) {
    if state == 0 {
        return;
    }
    #[cfg(feature = "multitask")]
    {
        let owner = unsafe { ax_hal::percpu::PreemptGuardOwner::from_raw(state) }
            .expect("a live synchronization guard must retain its preemption owner");
        crate::guard::exit_preempt(owner);
    }
    #[cfg(not(feature = "multitask"))]
    unreachable!("a uniprocessor runtime cannot own a preemption token");
}

unsafe fn context_preempt_exit_irq_return(state: usize) {
    if state == 0 {
        return;
    }
    #[cfg(feature = "multitask")]
    {
        let owner = unsafe { ax_hal::percpu::PreemptGuardOwner::from_raw(state) }
            .expect("an IRQ-return guard must retain its preemption owner");
        crate::guard::exit_preempt_from_irq_return(owner);
    }
    #[cfg(not(feature = "multitask"))]
    unreachable!("a uniprocessor runtime cannot own an IRQ-return preemption token");
}

fn context_irq_save_and_disable() -> usize {
    let was_enabled = ax_hal::asm::irqs_enabled();
    ax_hal::asm::disable_irqs();
    usize::from(was_enabled)
}

unsafe fn context_irq_restore(state: usize) {
    if state != 0 {
        ax_hal::asm::enable_irqs();
    } else {
        ax_hal::asm::disable_irqs();
    }
}

fn context_hardirq_enter() {
    #[cfg(feature = "multitask")]
    crate::irq_time::enter();
}

fn context_hardirq_exit() {
    #[cfg(feature = "multitask")]
    crate::irq_time::exit();
}

static CONTEXT_OPERATIONS: ax_task::sync::bridge::ContextOperations =
    ax_task::sync::bridge::ContextOperations {
        preempt_enter: context_preempt_enter,
        preempt_exit: context_preempt_exit,
        preempt_exit_irq_return: context_preempt_exit_irq_return,
        irq_save_and_disable: context_irq_save_and_disable,
        irq_restore: context_irq_restore,
        hardirq_enter: context_hardirq_enter,
        hardirq_exit: context_hardirq_exit,
    };

fn into_sync_context_state(
    state: ax_task::sync::bridge::ContextState,
) -> ax_sync::interface::ContextState {
    ax_sync::interface::ContextState::new(state.preempt(), state.irq())
}

fn into_task_context_state(
    state: ax_sync::interface::ContextState,
) -> ax_task::sync::bridge::ContextState {
    ax_task::sync::bridge::ContextState::new(state.preempt(), state.irq())
}

struct RuntimeContextOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::ContextOps for RuntimeContextOps {
    fn enter(context: u8) -> ax_sync::interface::ContextState {
        into_sync_context_state(ax_task::sync::bridge::context_enter(
            context,
            &CONTEXT_OPERATIONS,
        ))
    }

    fn exit(context: u8, state: ax_sync::interface::ContextState) {
        ax_task::sync::bridge::context_exit(
            context,
            into_task_context_state(state),
            &CONTEXT_OPERATIONS,
        );
    }

    fn irq_return_preempt_enter() -> usize {
        ax_task::sync::bridge::irq_return_preempt_enter(&CONTEXT_OPERATIONS)
    }

    fn irq_return_preempt_exit(state: usize) {
        // SAFETY: ax-sync returns only the token created by the paired enter.
        unsafe {
            ax_task::sync::bridge::irq_return_preempt_exit(state, &CONTEXT_OPERATIONS);
        }
    }

    fn hardirq_enter() {
        ax_task::sync::bridge::hardirq_enter(&CONTEXT_OPERATIONS);
    }

    fn hardirq_exit() {
        ax_task::sync::bridge::hardirq_exit(&CONTEXT_OPERATIONS);
    }
}

struct RuntimeSpinOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::SpinOps for RuntimeSpinOps {
    fn acquire(
        locked: &AtomicBool,
        metadata: &ax_sync::interface::LockMetadata,
        lock_addr: usize,
        context: u8,
        subclass: u32,
        is_try: bool,
        caller: &'static Location<'static>,
    ) -> ax_sync::interface::AcquireResult {
        let (acquired, context_state) = ax_task::sync::bridge::spin_acquire(
            ax_task::sync::bridge::SpinAcquireRequest {
                locked,
                class: ax_task::sync::bridge::LockClass {
                    class_id: metadata.class_id(),
                    class_key: metadata.class_key(),
                },
                lock_addr,
                context,
                subclass,
                is_try,
                caller,
            },
            &CONTEXT_OPERATIONS,
        );
        ax_sync::interface::AcquireResult::new(acquired, into_sync_context_state(context_state))
    }

    fn release(
        locked: &AtomicBool,
        lock_addr: usize,
        context: u8,
        context_state: ax_sync::interface::ContextState,
    ) {
        ax_task::sync::bridge::spin_release(
            locked,
            lock_addr,
            context,
            into_task_context_state(context_state),
            &CONTEXT_OPERATIONS,
        );
    }

    fn force_release(locked: &AtomicBool, lock_addr: usize, context: u8) {
        ax_task::sync::bridge::spin_force_release(locked, lock_addr, context);
    }

    fn is_locked(locked: &AtomicBool) -> bool {
        ax_task::sync::bridge::spin_is_locked(locked)
    }
}

struct RuntimeRwLockOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::RwLockOps for RuntimeRwLockOps {
    fn acquire(
        state: &AtomicUsize,
        metadata: &ax_sync::interface::LockMetadata,
        lock_addr: usize,
        context: u8,
        mode: u8,
        is_try: bool,
        caller: &'static Location<'static>,
    ) -> ax_sync::interface::AcquireResult {
        let (acquired, context_state) = ax_task::sync::bridge::rwlock_acquire(
            ax_task::sync::bridge::RwLockAcquireRequest {
                state,
                class: ax_task::sync::bridge::LockClass {
                    class_id: metadata.class_id(),
                    class_key: metadata.class_key(),
                },
                lock_addr,
                context,
                mode,
                is_try,
                caller,
            },
            &CONTEXT_OPERATIONS,
        );
        ax_sync::interface::AcquireResult::new(acquired, into_sync_context_state(context_state))
    }

    fn release(
        state: &AtomicUsize,
        lock_addr: usize,
        context: u8,
        context_state: ax_sync::interface::ContextState,
        mode: u8,
    ) {
        ax_task::sync::bridge::rwlock_release(
            state,
            lock_addr,
            context,
            into_task_context_state(context_state),
            mode,
            &CONTEXT_OPERATIONS,
        );
    }

    fn force_read_decrement(state: &AtomicUsize, lock_addr: usize, context: u8) {
        ax_task::sync::bridge::rwlock_force_read_decrement(state, lock_addr, context);
    }
}

#[cfg(feature = "multitask")]
struct RuntimeMutexOps;

#[cfg(feature = "multitask")]
fn into_task_pi_storage(
    storage: &ax_sync::interface::PiMutexStorage,
) -> ax_task::sync::bridge::PiMutexStorage<'_> {
    ax_task::sync::bridge::PiMutexStorage {
        owner: storage.owner_word(),
        generation: storage.generation(),
        wait_state: storage.wait_state(),
        wait_words: storage.wait_storage(),
    }
}

#[cfg(feature = "multitask")]
#[ax_crate_interface::impl_interface]
impl ax_sync::interface::MutexOps for RuntimeMutexOps {
    fn acquire(
        storage: &ax_sync::interface::PiMutexStorage,
        next_waiter_sequence: &AtomicU64,
        metadata: &ax_sync::interface::LockMetadata,
        lock_addr: usize,
        subclass: u32,
        is_try: bool,
        caller: &'static Location<'static>,
    ) -> bool {
        ax_task::sync::bridge::mutex_acquire(ax_task::sync::bridge::MutexAcquireRequest {
            storage: into_task_pi_storage(storage),
            next_waiter_sequence,
            class: ax_task::sync::bridge::LockClass {
                class_id: metadata.class_id(),
                class_key: metadata.class_key(),
            },
            lock_addr,
            subclass,
            is_try,
            caller,
        })
    }

    fn release(storage: &ax_sync::interface::PiMutexStorage, lock_addr: usize) {
        ax_task::sync::bridge::mutex_release(into_task_pi_storage(storage), lock_addr);
    }

    fn force_release(storage: &ax_sync::interface::PiMutexStorage, lock_addr: usize) {
        ax_task::sync::bridge::mutex_force_release(into_task_pi_storage(storage), lock_addr);
    }

    fn is_owned_by_current(storage: &ax_sync::interface::PiMutexStorage) -> bool {
        ax_task::sync::bridge::mutex_is_owned_by_current(into_task_pi_storage(storage))
    }

    fn is_locked(storage: &ax_sync::interface::PiMutexStorage) -> bool {
        ax_task::sync::bridge::mutex_is_locked(into_task_pi_storage(storage))
    }

    fn destroy(storage: &mut ax_sync::interface::PiMutexStorage) {
        let parts = storage.parts_mut();
        ax_task::sync::bridge::mutex_destroy(ax_task::sync::bridge::PiMutexStorageMut {
            owner: parts.owner_word,
            generation: parts.generation,
            wait_state: parts.wait_state,
            wait_words: parts.wait_storage,
        });
    }
}

struct RuntimeLockdepOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::LockdepOps for RuntimeLockdepOps {
    fn set_trace_enabled(enabled: bool) {
        ax_task::sync::bridge::set_lockdep_trace_enabled(enabled);
    }

    fn dump_trace() {
        ax_task::sync::bridge::dump_lockdep_trace();
    }
}
