// Minimal host-side lock provider for ax-driver tests.
//
// ax-driver's tests register their own `Klib` mock (see `TestKlib`), which
// conflicts with ax-runtime's production `Klib` implementation, so the tests
// cannot link ax-runtime as the lock provider. Instead this module registers
// a self-consistent spin engine through the same `ax-crate-interface`
// boundary; host tests are single-threaded, so no preemption or IRQ token is
// tracked (the returned `ContextState` is a placeholder).

use core::{
    panic::Location,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use ax_sync::interface::{
    AcquireResult, ContextState, LOCK_MODE_READ, LOCK_MODE_WRITE, LockMetadata,
};

const WRITER: usize = 1;

struct TestLockOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::SpinOps for TestLockOps {
    fn acquire(
        locked: &AtomicBool,
        _metadata: &LockMetadata,
        _lock_addr: usize,
        _context: u8,
        _subclass: u32,
        is_try: bool,
        _caller: &'static Location<'static>,
    ) -> AcquireResult {
        let acquired = if is_try {
            locked
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        } else {
            while locked
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                core::hint::spin_loop();
            }
            true
        };
        AcquireResult::new(acquired, ContextState::new(0, 0))
    }

    fn release(locked: &AtomicBool, _lock_addr: usize, _context: u8, _context_state: ContextState) {
        locked.store(false, Ordering::Release);
    }

    fn force_release(locked: &AtomicBool, _lock_addr: usize, _context: u8) {
        locked.store(false, Ordering::Release);
    }

    fn is_locked(locked: &AtomicBool) -> bool {
        locked.load(Ordering::Relaxed)
    }
}

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::RwLockOps for TestLockOps {
    fn acquire(
        state: &AtomicUsize,
        _metadata: &LockMetadata,
        _lock_addr: usize,
        _context: u8,
        mode: u8,
        is_try: bool,
        _caller: &'static Location<'static>,
    ) -> AcquireResult {
        let acquired = if mode == LOCK_MODE_WRITE {
            if is_try {
                state
                    .compare_exchange(0, WRITER, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            } else {
                while state
                    .compare_exchange(0, WRITER, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
                {
                    core::hint::spin_loop();
                }
                true
            }
        } else {
            loop {
                let current = state.load(Ordering::Relaxed);
                if current & WRITER != 0 {
                    if is_try {
                        break false;
                    }
                    core::hint::spin_loop();
                    continue;
                }
                if state
                    .compare_exchange(current, current + 2, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    break true;
                }
            }
        };
        AcquireResult::new(acquired, ContextState::new(0, 0))
    }

    fn release(
        state: &AtomicUsize,
        _lock_addr: usize,
        _context: u8,
        _context_state: ContextState,
        mode: u8,
    ) {
        if mode == LOCK_MODE_WRITE {
            state.store(0, Ordering::Release);
        } else {
            state.fetch_sub(2, Ordering::Release);
        }
    }

    fn force_read_decrement(state: &AtomicUsize, _lock_addr: usize, _context: u8) {
        state.fetch_sub(2, Ordering::Release);
    }
}
