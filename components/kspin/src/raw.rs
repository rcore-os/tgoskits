//! Raw spin locks that implement [`lock_api::RawMutex`].
//!
//! Unlike [`BaseSpinLock`](crate::BaseSpinLock), these locks do not own the
//! protected data; they only provide the lock/unlock primitive so they can be
//! plugged into foreign generic code that is parameterised over
//! `lock_api::RawMutex` (for example the `kprobe` crate's `ProbeManager`).
//!
//! The guard semantics still come from a [`BaseGuard`]: acquiring the lock
//! runs `G::acquire()` (e.g. disabling preemption and local IRQs) *before*
//! spinning, and releasing it restores that state. This matches the behaviour
//! of [`SpinNoIrq`](crate::SpinNoIrq) and is what makes the lock safe to take
//! from contexts that may be re-entered by interrupts or trap handlers.

#[cfg(feature = "smp")]
use core::sync::atomic::{AtomicBool, Ordering};
use core::{cell::UnsafeCell, marker::PhantomData};

use ax_kernel_guard::{BaseGuard, NoPreemptIrqSave};

/// A raw spin lock implementing [`lock_api::RawMutex`], whose critical-section
/// guard behaviour is determined by the [`BaseGuard`] type parameter `G`.
///
/// On a single-core build (without the `smp` feature) the atomic flag is
/// elided, but `G::acquire()`/`G::release()` are still run so preemption and
/// IRQ state are managed correctly.
pub struct BaseRawSpinLock<G: BaseGuard> {
    _phantom: PhantomData<G>,

    #[cfg(feature = "smp")]
    locked: AtomicBool,

    // Saved guard state from `G::acquire()`. Only the lock owner writes or
    // reads this slot while the lock is held, so the lack of synchronisation
    // is sound.
    state: UnsafeCell<Option<G::State>>,
}

// The `UnsafeCell<Option<G::State>>` is only ever touched by the thread that
// owns the lock, so the lock as a whole is `Sync`.
unsafe impl<G: BaseGuard> Sync for BaseRawSpinLock<G> {}

impl<G: BaseGuard> BaseRawSpinLock<G> {
    /// Creates a new, unlocked raw spin lock.
    pub const fn new() -> Self {
        Self {
            _phantom: PhantomData,
            #[cfg(feature = "smp")]
            locked: AtomicBool::new(false),
            state: UnsafeCell::new(None),
        }
    }

    #[inline]
    fn save_state(&self, state: G::State) {
        // SAFETY: called only by the thread that just acquired the lock.
        unsafe {
            *self.state.get() = Some(state);
        }
    }

    #[inline]
    fn take_state(&self) -> G::State {
        // SAFETY: called only by the thread that currently holds the lock,
        // which is the same thread that stored the state in `lock()`.
        unsafe {
            (*self.state.get())
                .take()
                .expect("raw spinlock unlocked without saved guard state")
        }
    }
}

impl<G: BaseGuard> Default for BaseRawSpinLock<G> {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl<G: BaseGuard + Send + Sync + 'static> lock_api::RawMutex for BaseRawSpinLock<G> {
    const INIT: Self = Self::new();

    type GuardMarker = lock_api::GuardNoSend;

    fn lock(&self) {
        let state = G::acquire();

        #[cfg(feature = "smp")]
        {
            while self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                // Wait until the lock looks unlocked before retrying.
                while self.locked.load(Ordering::Relaxed) {
                    core::hint::spin_loop();
                }
            }
        }

        self.save_state(state);
    }

    fn try_lock(&self) -> bool {
        let state = G::acquire();

        #[cfg(feature = "smp")]
        {
            if self
                .locked
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                G::release(state);
                return false;
            }
        }

        self.save_state(state);
        true
    }

    unsafe fn unlock(&self) {
        let state = self.take_state();

        #[cfg(feature = "smp")]
        self.locked.store(false, Ordering::Release);

        G::release(state);
    }
}

/// A raw spin lock that disables kernel preemption and local IRQs while held,
/// mirroring [`SpinNoIrq`](crate::SpinNoIrq) but exposed as a
/// [`lock_api::RawMutex`] for use with foreign generic code.
pub type RawSpinNoIrq = BaseRawSpinLock<NoPreemptIrqSave>;
