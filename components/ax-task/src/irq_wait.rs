//! Single-waiter hard-IRQ notification cell.
//!
//! Multi-waiter events should target a fixed service thread through this cell;
//! that thread performs any wait-queue fan-out in ordinary task context.

use core::{
    marker::PhantomPinned,
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};

/// Trusted direct wake capability stored in a stable IRQ registration.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct IrqWakeHandle {
    data: usize,
    wake: unsafe fn(usize),
}

impl IrqWakeHandle {
    /// Creates a direct hard-IRQ wake capability.
    ///
    /// # Safety
    ///
    /// `wake(data)` must remain valid for the registration lifetime. It must be
    /// concurrency-safe, non-blocking, allocation-free, and must not invoke user
    /// code or scan a wait queue.
    pub const unsafe fn from_raw(data: usize, wake: unsafe fn(usize)) -> Self {
        Self { data, wake }
    }

    fn wake(self) {
        unsafe {
            // Construction requires this fixed runtime operation to remain valid
            // and hard-IRQ-safe for the registration lifetime.
            (self.wake)(self.data);
        }
    }
}

impl core::fmt::Debug for IrqWakeHandle {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("IrqWakeHandle")
            .field("data", &self.data)
            .finish_non_exhaustive()
    }
}

/// Pinned one-shot registration containing a stable direct thread wake.
#[derive(Debug)]
pub struct IrqWaitRegistration {
    wake: IrqWakeHandle,
    attached: AtomicBool,
    _pin: PhantomPinned,
}

impl IrqWaitRegistration {
    /// Creates a detached registration reusable across one-shot waits.
    pub const fn new(wake: IrqWakeHandle) -> Self {
        Self {
            wake,
            attached: AtomicBool::new(false),
            _pin: PhantomPinned,
        }
    }

    fn reserve(&self) -> bool {
        self.attached
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    fn release_and_wake(&self) {
        self.attached.store(false, Ordering::Release);
        self.wake.wake();
    }

    fn release(&self) {
        self.attached.store(false, Ordering::Release);
    }
}

// SAFETY: IRQ-visible registration state is atomic and the wake capability's
// constructor requires concurrent hard-IRQ safety.
unsafe impl Send for IrqWaitRegistration {}
// SAFETY: Shared operations access only atomics and the immutable trusted wake.
unsafe impl Sync for IrqWaitRegistration {}

/// Outcome of task-context waiter registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqRegisterResult {
    /// The cell owns the sole waiter until notify or unregister.
    Registered,
    /// A coalesced earlier interrupt consumed the registration and woke it.
    ConsumedPending,
    /// Another waiter is registered or this registration belongs to another cell.
    Occupied,
}

/// Outcome of one bounded hard-IRQ notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqNotifyResult {
    /// One stable direct waiter was removed and woken.
    Notified,
    /// No waiter was present; one coalesced pending bit was published.
    Pending,
}

/// Pending-bit plus single-waiter hard-IRQ event cell.
#[derive(Debug)]
pub struct IrqWaitCell {
    pending: AtomicBool,
    waiter: AtomicPtr<IrqWaitRegistration>,
}

impl IrqWaitCell {
    /// Creates an empty notification cell.
    pub const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            waiter: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Registers one stable waiter, consuming an earlier IRQ when present.
    pub fn register(&self, registration: Pin<&'static IrqWaitRegistration>) -> IrqRegisterResult {
        if !registration.reserve() {
            return IrqRegisterResult::Occupied;
        }
        let registration_ptr = registration.get_ref() as *const IrqWaitRegistration as *mut _;
        if self
            .waiter
            .compare_exchange(
                ptr::null_mut(),
                registration_ptr,
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_err()
        {
            registration.release();
            return IrqRegisterResult::Occupied;
        }

        if self.pending.swap(false, Ordering::AcqRel) {
            if self
                .waiter
                .compare_exchange(
                    registration_ptr,
                    ptr::null_mut(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                registration.release_and_wake();
            }
            return IrqRegisterResult::ConsumedPending;
        }

        if self.waiter.load(Ordering::Acquire) == registration_ptr {
            IrqRegisterResult::Registered
        } else {
            // A concurrent notifier already owns and will wake the registration.
            IrqRegisterResult::ConsumedPending
        }
    }

    /// Removes a matching waiter before it blocks or after cancellation.
    pub fn unregister(&self, registration: Pin<&'static IrqWaitRegistration>) -> bool {
        let registration_ptr = registration.get_ref() as *const IrqWaitRegistration as *mut _;
        if self
            .waiter
            .compare_exchange(
                registration_ptr,
                ptr::null_mut(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            registration.release();
            true
        } else {
            false
        }
    }

    /// Wakes the sole registered thread or publishes one coalesced pending bit.
    ///
    /// This operation performs a bounded number of atomics and at most one trusted
    /// direct wake. It never scans a wait queue or allocates.
    pub fn notify(&self) -> IrqNotifyResult {
        let waiter = self.waiter.swap(ptr::null_mut(), Ordering::AcqRel);
        if !waiter.is_null() {
            unsafe {
                // The cell owns one pinned registration until swap removes it.
                (*waiter).release_and_wake();
            }
            return IrqNotifyResult::Notified;
        }

        self.pending.store(true, Ordering::Release);

        // Close the null-observation/register-publication race: either this pass
        // takes the newly published waiter, or register observes the pending bit.
        let waiter = self.waiter.swap(ptr::null_mut(), Ordering::AcqRel);
        if waiter.is_null() {
            IrqNotifyResult::Pending
        } else {
            self.pending.store(false, Ordering::Release);
            unsafe {
                // The second swap owns the single pinned registration as above.
                (*waiter).release_and_wake();
            }
            IrqNotifyResult::Notified
        }
    }

    /// Reports whether an IRQ is coalesced for the next registration.
    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
}

impl Default for IrqWaitCell {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "irq_wait_tests.rs"]
mod tests;
