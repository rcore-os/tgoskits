//! Physical spin and read-write lock state transitions.

#[cfg(feature = "smp")]
use core::sync::atomic::AtomicBool;
use core::{
    hint::spin_loop,
    sync::atomic::{AtomicUsize, Ordering},
};

const READER: usize = 1;
const WRITER: usize = 1 << (usize::BITS - 1);
const MAX_READER: usize = 1 << (usize::BITS - 2);

#[cfg(feature = "smp")]
#[inline(always)]
pub(crate) fn spin_try_acquire_weak(locked: &AtomicBool) -> bool {
    locked
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

#[cfg(feature = "smp")]
#[inline(always)]
pub(crate) fn spin_try_acquire_strong(locked: &AtomicBool) -> bool {
    locked
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

#[cfg(feature = "smp")]
#[inline(always)]
pub(crate) fn spin_acquire(locked: &AtomicBool, mut acquire_once: impl FnMut() -> bool) {
    while !acquire_once() {
        spin_wait_until_unlocked(locked);
    }
}

#[cfg(feature = "smp")]
#[inline(always)]
pub(crate) fn spin_is_locked(locked: &AtomicBool) -> bool {
    locked.load(Ordering::Acquire)
}

#[cfg(feature = "smp")]
#[inline(always)]
pub(crate) fn spin_wait_until_unlocked(locked: &AtomicBool) {
    while spin_is_locked(locked) {
        spin_loop();
    }
}

#[cfg(feature = "smp")]
#[inline(always)]
pub(crate) fn spin_release(locked: &AtomicBool) {
    locked.store(false, Ordering::Release);
}

#[inline(always)]
pub(crate) fn rw_try_acquire_read(state: &AtomicUsize) -> bool {
    let old = state.fetch_add(READER, Ordering::Acquire);
    if old & (WRITER | MAX_READER) == 0 {
        true
    } else {
        state.fetch_sub(READER, Ordering::Release);
        false
    }
}

#[inline(always)]
pub(crate) fn rw_try_acquire_write(state: &AtomicUsize) -> bool {
    state
        .compare_exchange(0, WRITER, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

#[inline(always)]
pub(crate) fn rw_is_write_locked(state: &AtomicUsize) -> bool {
    state.load(Ordering::Acquire) & WRITER != 0
}

#[inline(always)]
fn rw_wait_until_readable(state: &AtomicUsize) {
    while rw_is_write_locked(state) {
        spin_loop();
    }
}

#[inline(always)]
fn rw_wait_until_unlocked(state: &AtomicUsize) {
    while state.load(Ordering::Acquire) != 0 {
        spin_loop();
    }
}

#[inline(always)]
pub(crate) fn rw_acquire_read(state: &AtomicUsize) {
    while !rw_try_acquire_read(state) {
        rw_wait_until_readable(state);
    }
}

#[inline(always)]
pub(crate) fn rw_acquire_write(state: &AtomicUsize) {
    while !rw_try_acquire_write(state) {
        rw_wait_until_unlocked(state);
    }
}

#[inline(always)]
pub(crate) fn rw_release_read(state: &AtomicUsize) {
    state.fetch_sub(READER, Ordering::Release);
}

#[inline(always)]
pub(crate) fn rw_release_write(state: &AtomicUsize) {
    state.fetch_and(!WRITER, Ordering::Release);
}

#[inline(always)]
pub(crate) fn rw_force_read_decrement(state: &AtomicUsize) -> bool {
    let mut observed = state.load(Ordering::Acquire);
    loop {
        let readers = observed & !(WRITER | MAX_READER);
        if readers == 0 {
            return false;
        }

        match state.compare_exchange_weak(
            observed,
            observed - READER,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(current) => observed = current,
        }
    }
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
mod tests {
    use super::*;

    #[cfg(feature = "smp")]
    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn spin_state_uses_one_shared_acquire_release_algorithm() {
        let locked = AtomicBool::new(false);

        assert!(spin_try_acquire_strong(&locked));
        assert!(spin_is_locked(&locked));
        assert!(!spin_try_acquire_strong(&locked));

        spin_release(&locked);
        assert!(!spin_is_locked(&locked));
        assert!(spin_try_acquire_weak(&locked));
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn rw_force_decrement_does_not_underflow_empty_or_writer_state() {
        let state = AtomicUsize::new(0);
        assert!(!rw_force_read_decrement(&state));
        assert_eq!(state.load(Ordering::Relaxed), 0);

        state.store(WRITER, Ordering::Relaxed);
        assert!(!rw_force_read_decrement(&state));
        assert_eq!(state.load(Ordering::Relaxed), WRITER);
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn rw_writer_waits_until_the_last_reader_releases() {
        let state = AtomicUsize::new(0);
        assert!(rw_try_acquire_read(&state));
        assert!(rw_try_acquire_read(&state));
        assert!(!rw_try_acquire_write(&state));

        rw_release_read(&state);
        assert!(!rw_try_acquire_write(&state));
        rw_release_read(&state);
        assert!(rw_try_acquire_write(&state));
        rw_release_write(&state);
        assert_eq!(state.load(Ordering::Relaxed), 0);
    }
}
