//! IRQ-save adapter for crates parameterized by [`lock_api::RawMutex`].

use core::{cell::UnsafeCell, panic::Location, sync::atomic::AtomicBool};

use crate::interface::{CONTEXT_PREEMPT_IRQSAVE, LockMetadata};

/// A raw mutex whose acquisition disables preemption and saves local IRQs.
#[repr(C)]
pub struct RawIrqSaveMutex {
    locked: AtomicBool,
    metadata: LockMetadata,
    context_state: UnsafeCell<Option<usize>>,
}

unsafe impl Sync for RawIrqSaveMutex {}

impl RawIrqSaveMutex {
    /// Creates a new unlocked raw mutex.
    #[track_caller]
    pub const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            metadata: LockMetadata::new(),
            context_state: UnsafeCell::new(None),
        }
    }

    fn addr(&self) -> usize {
        self as *const Self as usize
    }
}

impl Default for RawIrqSaveMutex {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl lock_api::RawMutex for RawIrqSaveMutex {
    const INIT: Self = Self::new();

    type GuardMarker = lock_api::GuardNoSend;

    fn lock(&self) {
        let result = crate::interface::spin_acquire(
            &self.locked,
            &self.metadata,
            self.addr(),
            CONTEXT_PREEMPT_IRQSAVE,
            0,
            false,
            Location::caller(),
        );
        assert!(result.acquired(), "blocking raw mutex acquisition failed");
        // SAFETY: only the thread that acquired the raw mutex writes this
        // slot, and no other owner can exist until `unlock`.
        unsafe { *self.context_state.get() = Some(result.context_state()) };
    }

    fn try_lock(&self) -> bool {
        let result = crate::interface::spin_acquire(
            &self.locked,
            &self.metadata,
            self.addr(),
            CONTEXT_PREEMPT_IRQSAVE,
            0,
            true,
            Location::caller(),
        );
        if result.acquired() {
            // SAFETY: this caller is now the unique raw mutex owner.
            unsafe { *self.context_state.get() = Some(result.context_state()) };
            true
        } else {
            false
        }
    }

    unsafe fn unlock(&self) {
        // SAFETY: the RawMutex contract requires the current thread to own
        // the mutex, so it is the unique accessor of this saved token.
        let context_state = unsafe { &mut *self.context_state.get() }
            .take()
            .expect("raw mutex unlocked without a saved context state");
        crate::interface::spin_release(
            &self.locked,
            self.addr(),
            CONTEXT_PREEMPT_IRQSAVE,
            context_state,
        );
    }
}
