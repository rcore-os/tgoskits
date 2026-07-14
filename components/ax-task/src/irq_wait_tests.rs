use alloc::boxed::Box;
use core::{
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
};

use super::*;

#[test]
fn consumes_an_irq_that_arrived_before_registration() {
    let cell = IrqWaitCell::new();
    let (registration, wakes) = registration();

    assert_eq!(cell.notify(), IrqNotifyResult::Pending);
    assert_eq!(
        cell.register(registration),
        IrqRegisterResult::ConsumedPending
    );
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
}

#[test]
fn irq_wakes_the_single_registered_thread() {
    let cell = IrqWaitCell::new();
    let (registration, wakes) = registration();

    assert_eq!(cell.register(registration), IrqRegisterResult::Registered);
    assert_eq!(cell.notify(), IrqNotifyResult::Notified);
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
    assert_eq!(cell.notify(), IrqNotifyResult::Pending);
}

#[test]
fn rejects_a_second_waiter_without_scanning() {
    let cell = IrqWaitCell::new();
    let (first, _) = registration();
    let (second, _) = registration();

    assert_eq!(cell.register(first), IrqRegisterResult::Registered);
    assert_eq!(cell.register(second), IrqRegisterResult::Occupied);
    assert!(cell.unregister(first));
}

fn registration() -> (Pin<&'static IrqWaitRegistration>, &'static AtomicUsize) {
    let wakes = Box::leak(Box::new(AtomicUsize::new(0)));
    let wake = unsafe {
        // The leaked counter is stable and the callback is atomic-only.
        IrqWakeHandle::from_raw(wakes as *const AtomicUsize as usize, count_wake)
    };
    let registration: &'static IrqWaitRegistration =
        Box::leak(Box::new(IrqWaitRegistration::new(wake)));
    let registration = unsafe {
        // The leaked registration remains pinned for every cell operation.
        Pin::new_unchecked(registration)
    };
    (registration, wakes)
}

/// Counts one direct IRQ wake.
///
/// # Safety
///
/// `data` must point to the leaked atomic installed by `registration`.
unsafe fn count_wake(data: usize) {
    let wakes = unsafe {
        // The registration constructor preserves the leaked atomic address.
        &*(data as *const AtomicUsize)
    };
    wakes.fetch_add(1, Ordering::Relaxed);
}
