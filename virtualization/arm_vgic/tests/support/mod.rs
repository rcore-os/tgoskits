use std::{
    panic::Location,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_sync::interface::{AcquireResult, ContextState, LockMetadata};

struct TestContextOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::ContextOps for TestContextOps {
    fn enter(_context: u8) -> ContextState {
        ContextState::new(0, 0)
    }

    fn exit(_context: u8, _state: ContextState) {}

    fn irq_return_preempt_enter() -> usize {
        0
    }

    fn irq_return_preempt_exit(_state: usize) {}

    fn hardirq_enter() {}

    fn hardirq_exit() {}
}

struct TestSpinOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::SpinOps for TestSpinOps {
    fn acquire(
        locked: &AtomicBool,
        _metadata: &LockMetadata,
        _lock_addr: usize,
        _context: u8,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) -> ContextState {
        while locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::thread::yield_now();
        }
        ContextState::new(0, 0)
    }

    fn try_acquire(
        locked: &AtomicBool,
        _metadata: &LockMetadata,
        _lock_addr: usize,
        _context: u8,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) -> AcquireResult {
        AcquireResult::new(
            locked
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok(),
            ContextState::new(0, 0),
        )
    }

    fn release(locked: &AtomicBool, _lock_addr: usize, _context: u8, _context_state: ContextState) {
        locked.store(false, Ordering::Release);
    }

    fn force_release(locked: &AtomicBool, _lock_addr: usize, _context: u8) {
        locked.store(false, Ordering::Release);
    }

    fn is_locked(locked: &AtomicBool) -> bool {
        locked.load(Ordering::Acquire)
    }
}

struct TestMutexOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::MutexOps for TestMutexOps {
    fn acquire(
        storage: &ax_sync::interface::MutexStorage,
        _next_waiter_sequence: &core::sync::atomic::AtomicU64,
        _metadata: &LockMetadata,
        _lock_addr: usize,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) {
        let word = storage.owner_word();
        while word
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::thread::yield_now();
        }
    }

    fn try_acquire(
        storage: &ax_sync::interface::MutexStorage,
        _next_waiter_sequence: &core::sync::atomic::AtomicU64,
        _metadata: &LockMetadata,
        _lock_addr: usize,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) -> bool {
        storage
            .owner_word()
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    fn release(storage: &ax_sync::interface::MutexStorage, _lock_addr: usize) {
        storage.owner_word().store(0, Ordering::Release);
    }

    fn force_release(storage: &ax_sync::interface::MutexStorage, _lock_addr: usize) {
        storage.owner_word().store(0, Ordering::Release);
    }

    fn is_owned_by_current(storage: &ax_sync::interface::MutexStorage) -> bool {
        storage.owner_word().load(Ordering::Acquire) != 0
    }

    fn is_locked(storage: &ax_sync::interface::MutexStorage) -> bool {
        storage.owner_word().load(Ordering::Acquire) != 0
    }

    fn destroy(_storage: &mut ax_sync::interface::MutexStorage) {}
}
