//! Host-only lock storage backend. This verifies portable borrow and registry
//! rules; it does not model IRQ, preemption, scheduling, or hardware behavior.

use core::{
    panic::Location,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use ax_sync::interface::{AcquireResult, ContextState, LOCK_MODE_READ, LockMetadata};

struct HostLocks;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::SpinOps for HostLocks {
    fn acquire(
        locked: &AtomicBool,
        _metadata: &LockMetadata,
        _addr: usize,
        _context: u8,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) -> ContextState {
        while locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        ContextState::new(0, 0)
    }
    fn try_acquire(
        locked: &AtomicBool,
        _metadata: &LockMetadata,
        _addr: usize,
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
    fn release(locked: &AtomicBool, _addr: usize, _context: u8, _state: ContextState) {
        locked.store(false, Ordering::Release);
    }
    fn force_release(locked: &AtomicBool, _addr: usize, _context: u8) {
        locked.store(false, Ordering::Release);
    }
    fn is_locked(locked: &AtomicBool) -> bool {
        locked.load(Ordering::Relaxed)
    }
}

const WRITER: usize = 1 << (usize::BITS - 1);

fn try_rwlock(state: &AtomicUsize, mode: u8) -> bool {
    if mode != LOCK_MODE_READ {
        return state
            .compare_exchange(0, WRITER, Ordering::Acquire, Ordering::Relaxed)
            .is_ok();
    }
    state
        .try_update(Ordering::Acquire, Ordering::Relaxed, |readers| {
            (readers < WRITER - 1).then(|| readers + 1)
        })
        .is_ok()
}

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::RwLockOps for HostLocks {
    fn acquire(
        state: &AtomicUsize,
        _metadata: &LockMetadata,
        _addr: usize,
        _context: u8,
        mode: u8,
        _caller: &'static Location<'static>,
    ) -> ContextState {
        while !try_rwlock(state, mode) {
            core::hint::spin_loop();
        }
        ContextState::new(0, 0)
    }
    fn try_acquire(
        state: &AtomicUsize,
        _metadata: &LockMetadata,
        _addr: usize,
        _context: u8,
        mode: u8,
        _caller: &'static Location<'static>,
    ) -> AcquireResult {
        AcquireResult::new(try_rwlock(state, mode), ContextState::new(0, 0))
    }
    fn release(
        state: &AtomicUsize,
        _addr: usize,
        _context: u8,
        _context_state: ContextState,
        mode: u8,
    ) {
        if mode == LOCK_MODE_READ {
            state.fetch_sub(1, Ordering::Release);
        } else {
            state.store(0, Ordering::Release);
        }
    }
    fn force_read_decrement(state: &AtomicUsize, _addr: usize, _context: u8) {
        state.fetch_sub(1, Ordering::Release);
    }
}
