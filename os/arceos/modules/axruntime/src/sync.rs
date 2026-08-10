//! ArceOS runtime providers for `ax-sync` capabilities.

#[cfg(all(feature = "multitask", not(feature = "host-test")))]
use alloc::boxed::Box;
#[cfg(all(feature = "multitask", not(feature = "host-test")))]
use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

#[cfg(not(feature = "host-test"))]
struct RuntimeCriticalSectionOps;

#[cfg(not(feature = "host-test"))]
#[ax_crate_interface::impl_interface]
impl ax_sync::CriticalSectionOps for RuntimeCriticalSectionOps {
    fn disable_preempt() {
        #[cfg(feature = "multitask")]
        ax_task::disable_preempt();
    }

    fn enable_preempt() {
        #[cfg(feature = "multitask")]
        ax_task::enable_preempt();
    }

    fn irq_save_and_disable() -> usize {
        let was_enabled = ax_hal::asm::irqs_enabled();
        ax_hal::asm::disable_irqs();
        usize::from(was_enabled)
    }

    fn irq_restore(state: usize) {
        if state != 0 {
            ax_hal::asm::enable_irqs();
        } else {
            ax_hal::asm::disable_irqs();
        }
    }
}

#[cfg(all(feature = "multitask", not(feature = "host-test")))]
struct RuntimeMutexOps;

#[cfg(all(feature = "multitask", not(feature = "host-test")))]
#[ax_crate_interface::impl_interface]
impl ax_sync::MutexRuntimeOps for RuntimeMutexOps {
    fn might_sleep(caller: &'static core::panic::Location<'static>) {
        ax_task::might_sleep_at(caller);
    }

    fn current_task_id() -> u64 {
        ax_task::current().id().as_u64()
    }

    fn wait_until_unlocked(wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64) {
        let wait_queue = ensure_wait_queue(wait_queue);
        wait_queue.wait_until(|| owner_id.load(Ordering::Acquire) == 0);
    }

    fn wake_one(wait_queue: &AtomicPtr<()>) {
        let wait_queue = wait_queue
            .load(Ordering::Acquire)
            .cast::<ax_task::WaitQueue>();
        if !wait_queue.is_null() {
            // SAFETY: the queue stays allocated until the containing mutex is
            // dropped, which safe Rust cannot race with this borrowed call.
            unsafe { &*wait_queue }.notify_one(true);
        }
    }

    fn drop_wait_queue(wait_queue: *mut ()) {
        // SAFETY: the capability contract transfers the uniquely owned queue
        // pointer back to the provider.
        let wait_queue = unsafe { Box::from_raw(wait_queue.cast::<ax_task::WaitQueue>()) };
        assert!(
            wait_queue.is_empty(),
            "dropping a mutex wait queue with blocked tasks"
        );
    }
}

#[cfg(all(feature = "multitask", not(feature = "host-test")))]
fn ensure_wait_queue(slot: &AtomicPtr<()>) -> &ax_task::WaitQueue {
    let existing = slot.load(Ordering::Acquire).cast::<ax_task::WaitQueue>();
    if !existing.is_null() {
        // SAFETY: installed queue pointers remain valid until mutex drop.
        return unsafe { &*existing };
    }

    let candidate = Box::into_raw(Box::new(ax_task::WaitQueue::new()));
    match slot.compare_exchange(
        core::ptr::null_mut(),
        candidate.cast::<()>(),
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => {
            // SAFETY: `candidate` is now owned by `slot`.
            unsafe { &*candidate }
        }
        Err(installed) => {
            // SAFETY: the failed candidate was never published.
            unsafe { drop(Box::from_raw(candidate)) };
            // SAFETY: the winning queue pointer is installed in `slot`.
            unsafe { &*installed.cast::<ax_task::WaitQueue>() }
        }
    }
}

#[cfg(all(feature = "lockdep", not(feature = "host-test")))]
struct RuntimeLockdepOps;

#[cfg(all(feature = "lockdep", not(feature = "host-test")))]
#[ax_crate_interface::impl_interface]
impl ax_sync::LockdepOps for RuntimeLockdepOps {
    fn irq_save_and_disable() -> usize {
        let was_enabled = ax_hal::asm::irqs_enabled();
        ax_hal::asm::disable_irqs();
        usize::from(was_enabled)
    }

    fn irq_restore(state: usize) {
        if state != 0 {
            ax_hal::asm::enable_irqs();
        } else {
            ax_hal::asm::disable_irqs();
        }
    }

    fn collect_current_task_held_locks(snapshot: &mut ax_sync::HeldLockSnapshot) {
        ax_task::collect_current_task_held_locks(snapshot);
    }

    fn push_current_task_held_lock(held: ax_sync::HeldLock) {
        ax_task::push_current_task_held_lock(held);
    }

    fn pop_current_task_held_lock(lock_addr: usize) {
        ax_task::pop_current_task_held_lock(lock_addr);
    }

    fn console_write_str(s: &str) {
        ax_hal::console::write_bytes(s.as_bytes());
    }

    fn fatal() -> ! {
        ax_hal::power::system_off()
    }
}
