//! Non-sleeping synchronization for cgroup hierarchy state.

pub(crate) use ax_sync::RawSpinLock;

// Host component tests exercise the real wrapper with atomic spin storage.
// IRQ masking and native scheduler semantics require the kernel runtime.
#[cfg(test)]
mod tests {
    use core::{
        panic::Location,
        sync::atomic::{AtomicBool, Ordering},
    };

    use ax_sync::interface::{AcquireResult, ContextState, LockMetadata};

    struct HostSpinOps;

    #[ax_crate_interface::impl_interface]
    impl ax_sync::interface::SpinOps for HostSpinOps {
        fn acquire(
            locked: &AtomicBool,
            _metadata: &LockMetadata,
            _lock_addr: usize,
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

        fn release(locked: &AtomicBool, _lock_addr: usize, _context: u8, _state: ContextState) {
            locked.store(false, Ordering::Release);
        }

        fn force_release(locked: &AtomicBool, _lock_addr: usize, _context: u8) {
            locked.store(false, Ordering::Release);
        }

        fn is_locked(locked: &AtomicBool) -> bool {
            locked.load(Ordering::Acquire)
        }
    }
}
