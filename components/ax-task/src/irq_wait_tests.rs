use alloc::boxed::Box;
use core::{
    mem::ManuallyDrop,
    pin::Pin,
    ptr::{NonNull, with_exposed_provenance},
    sync::atomic::{AtomicUsize, Ordering},
};

use super::*;

#[test]
fn consumes_an_irq_that_arrived_before_registration() {
    let cell = IrqWaitCell::new();
    let registration = TestRegistration::new();

    assert_eq!(cell.notify(), IrqNotifyResult::Pending);
    assert_eq!(
        cell.register(registration.pin()),
        IrqRegisterResult::ConsumedPending
    );
    assert_eq!(registration.wake_count(), 1);
}

#[test]
fn irq_wakes_the_single_registered_thread() {
    let cell = IrqWaitCell::new();
    let registration = TestRegistration::new();

    assert_eq!(
        cell.register(registration.pin()),
        IrqRegisterResult::Registered
    );
    assert_eq!(cell.notify(), IrqNotifyResult::Notified);
    assert_eq!(registration.wake_count(), 1);
    assert_eq!(cell.notify(), IrqNotifyResult::Pending);
}

#[test]
fn rejects_a_second_waiter_without_scanning() {
    let cell = IrqWaitCell::new();
    let first = TestRegistration::new();
    let second = TestRegistration::new();

    assert_eq!(cell.register(first.pin()), IrqRegisterResult::Registered);
    assert_eq!(cell.register(second.pin()), IrqRegisterResult::Occupied);
    assert!(cell.unregister(first.pin()));
}

struct TestRegistration {
    registration: ManuallyDrop<Pin<Box<IrqWaitRegistration>>>,
    wakes: NonNull<AtomicUsize>,
}

impl TestRegistration {
    fn new() -> Self {
        let wakes = NonNull::new(Box::into_raw(Box::new(AtomicUsize::new(0))))
            .expect("Box never yields a null pointer");
        let wake = unsafe {
            // The raw allocation has a stable address and outlives the registration.
            IrqWakeHandle::from_raw(wakes.as_ptr().expose_provenance(), count_wake)
        };
        Self {
            registration: ManuallyDrop::new(Box::pin(IrqWaitRegistration::new(wake))),
            wakes,
        }
    }

    fn pin(&self) -> Pin<&'static IrqWaitRegistration> {
        let registration = self.registration.as_ref().get_ref() as *const IrqWaitRegistration;
        unsafe {
            // The fixture owns a pinned allocation and each test consumes or
            // unregisters the cell reference before dropping the fixture.
            Pin::new_unchecked(&*registration)
        }
    }

    fn wake_count(&self) -> usize {
        unsafe {
            // The fixture exclusively owns the allocation; atomic callbacks may
            // access it concurrently through the same exposed provenance.
            self.wakes.as_ref().load(Ordering::Relaxed)
        }
    }
}

impl Drop for TestRegistration {
    fn drop(&mut self) {
        unsafe {
            // Drop the registration before reclaiming the callback payload.
            ManuallyDrop::drop(&mut self.registration);
            drop(Box::from_raw(self.wakes.as_ptr()));
        }
    }
}

/// Counts one direct IRQ wake.
///
/// # Safety
///
/// `data` must point to the boxed atomic owned by the matching test fixture.
unsafe fn count_wake(data: usize) {
    let wakes = unsafe {
        // The fixture preserves this exposed allocation until unregister/wake.
        &*with_exposed_provenance::<AtomicUsize>(data)
    };
    wakes.fetch_add(1, Ordering::Relaxed);
}
