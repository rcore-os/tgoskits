//! Minimal `ax-sync` runtime bridge for ArceBoot.
//!
//! ArceBoot is single-core, has no scheduler, preemption, or IRQ handling, so
//! every lock operation reduces to a plain atomic compare-exchange spin. The
//! `context` parameters are accepted and ignored; `context_state` is always
//! zero. This mirrors the `axruntime` bridge (`axruntime/src/sync.rs`) but
//! without the task/IRQ plumbing.

use core::{
    panic::Location,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering},
};

use ax_sync::interface::{AcquireResult, LOCK_MODE_READ, LockMetadata};

/// `SpinRwLock` state encoding used by this bridge.
const RW_WRITE_LOCKED: usize = usize::MAX;

struct BootContextOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::ContextOps for BootContextOps {
    fn enter(_context: u8) -> usize {
        0
    }

    fn exit(_context: u8, _state: usize) {}

    fn exit_preempt_from_irq_return(_state: usize) {}
}

struct BootSpinOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::SpinOps for BootSpinOps {
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
        AcquireResult::new(acquired, 0)
    }

    fn release(locked: &AtomicBool, _lock_addr: usize, _context: u8, _context_state: usize) {
        locked.store(false, Ordering::Release);
    }

    fn force_release(locked: &AtomicBool, _lock_addr: usize, _context: u8) {
        locked.store(false, Ordering::Release);
    }

    fn is_locked(locked: &AtomicBool) -> bool {
        locked.load(Ordering::Acquire)
    }
}

struct BootRwLockOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::RwLockOps for BootRwLockOps {
    fn acquire(
        state: &AtomicUsize,
        _metadata: &LockMetadata,
        _lock_addr: usize,
        _context: u8,
        mode: u8,
        is_try: bool,
        _caller: &'static Location<'static>,
    ) -> AcquireResult {
        let acquired = if mode == LOCK_MODE_READ {
            // Increment the reader count unless a writer holds the lock.
            loop {
                let cur = state.load(Ordering::Acquire);
                if cur == RW_WRITE_LOCKED {
                    if is_try {
                        break false;
                    }
                    core::hint::spin_loop();
                    continue;
                }
                match state.compare_exchange_weak(
                    cur,
                    cur + 1,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break true,
                    Err(_) => continue,
                }
            }
        } else {
            // Exclusive or write mode: claim the whole lock.
            if is_try {
                state
                    .compare_exchange(0, RW_WRITE_LOCKED, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            } else {
                while state
                    .compare_exchange(0, RW_WRITE_LOCKED, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
                {
                    core::hint::spin_loop();
                }
                true
            }
        };
        AcquireResult::new(acquired, 0)
    }

    fn release(
        state: &AtomicUsize,
        _lock_addr: usize,
        _context: u8,
        _context_state: usize,
        mode: u8,
    ) {
        if mode == LOCK_MODE_READ {
            state.fetch_sub(1, Ordering::Release);
        } else {
            state.store(0, Ordering::Release);
        }
    }

    fn force_read_decrement(state: &AtomicUsize, _lock_addr: usize, _context: u8) {
        state.fetch_sub(1, Ordering::Release);
    }
}

struct BootMutexOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::MutexOps for BootMutexOps {
    fn acquire(
        _wait_queue: &AtomicPtr<()>,
        owner_id: &AtomicU64,
        _metadata: &LockMetadata,
        _lock_addr: usize,
        _subclass: u32,
        is_try: bool,
        _caller: &'static Location<'static>,
    ) -> bool {
        // Single-core bootloader: `owner_id` is 0 (unlocked) or 1 (locked).
        if is_try {
            owner_id
                .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        } else {
            while owner_id
                .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                core::hint::spin_loop();
            }
            true
        }
    }

    fn release(_wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64, _lock_addr: usize) {
        owner_id.store(0, Ordering::Release);
    }

    fn force_release(_wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64, _lock_addr: usize) {
        owner_id.store(0, Ordering::Release);
    }

    fn is_owned_by_current(owner_id: &AtomicU64) -> bool {
        owner_id.load(Ordering::Acquire) == 1
    }

    fn is_locked(owner_id: &AtomicU64) -> bool {
        owner_id.load(Ordering::Acquire) != 0
    }

    fn drop_wait_queue(_wait_queue: *mut ()) {}
}

struct BootLockdepOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::LockdepOps for BootLockdepOps {
    fn set_trace_enabled(_enabled: bool) {}

    fn dump_trace() {}
}
