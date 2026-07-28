use alloc::boxed::Box;
use core::{
    mem::ManuallyDrop,
    ptr::{NonNull, with_exposed_provenance},
    sync::atomic::{AtomicUsize, Ordering},
};

use super::*;

#[test]
fn consumes_an_irq_that_arrived_before_registration_without_self_wake() {
    let cell = IrqWaitCell::new();
    let registration = TestRegistration::new();

    assert_eq!(cell.notify(), IrqNotifyResult::Pending);
    assert!(matches!(
        cell.register(registration.registration()),
        IrqRegisterResult::ConsumedPending
    ));
    assert_eq!(
        registration.wake_count(),
        0,
        "the running consumer must not publish a stale scheduler wake for an event it consumed \
         synchronously"
    );
    let token = expect_registered(cell.register(registration.registration()));
    assert_eq!(cell.notify(), IrqNotifyResult::Notified);
    assert_eq!(registration.wake_count(), 1);
    assert!(token.is_quiescent());
    token.try_quiesce().unwrap();
}

#[test]
fn irq_wakes_the_single_registered_thread() {
    let cell = IrqWaitCell::new();
    let registration = TestRegistration::new();

    let token = expect_registered(cell.register(registration.registration()));
    assert_eq!(cell.notify(), IrqNotifyResult::Notified);
    assert_eq!(registration.wake_count(), 1);
    token.try_quiesce().unwrap();
    assert_eq!(cell.notify(), IrqNotifyResult::Pending);
}

#[test]
fn rejects_a_second_waiter_without_scanning() {
    let cell = IrqWaitCell::new();
    let first = TestRegistration::new();
    let second = TestRegistration::new();

    let token = expect_registered(cell.register(first.registration()));
    assert!(matches!(
        cell.register(second.registration()),
        IrqRegisterResult::Occupied
    ));
    assert_eq!(cell.unregister(&token), IrqUnregisterResult::Detached);
    token.try_quiesce().unwrap();
}

#[test]
#[should_panic(expected = "dropped before token quiescence")]
fn dropping_an_attached_registration_is_rejected() {
    let cell = IrqWaitCell::new();
    let registration = TestRegistration::new();
    let token = expect_registered(cell.register(registration.registration()));

    core::mem::forget(token);
    drop(registration);
}

#[test]
fn detached_registration_is_not_quiescent_until_irq_wake_returns() {
    struct BlockingWake {
        entered: AtomicUsize,
        release: AtomicUsize,
        completed: AtomicUsize,
    }

    unsafe fn blocking_wake(data: usize) {
        let state = unsafe { &*with_exposed_provenance::<BlockingWake>(data) };
        state.entered.store(1, Ordering::Release);
        while state.release.load(Ordering::Acquire) == 0 {
            core::hint::spin_loop();
        }
        state.completed.store(1, Ordering::Release);
    }

    let cell = IrqWaitCell::new();
    let state = Box::leak(Box::new(BlockingWake {
        entered: AtomicUsize::new(0),
        release: AtomicUsize::new(0),
        completed: AtomicUsize::new(0),
    }));
    let wake = unsafe {
        IrqWakeHandle::from_raw(
            (state as *mut BlockingWake).expose_provenance(),
            blocking_wake,
        )
    };
    let registration = IrqWaitRegistration::new_test(wake);
    let token = expect_registered(cell.register(&registration));

    std::thread::scope(|scope| {
        let notifier = scope.spawn(|| cell.notify());
        while state.entered.load(Ordering::Acquire) == 0 {
            std::thread::yield_now();
        }

        assert!(!token.is_attached());
        let quiescent_while_wake_uses_payload = token.is_quiescent();

        state.release.store(1, Ordering::Release);
        assert_eq!(notifier.join().unwrap(), IrqNotifyResult::Notified);
        assert!(
            !quiescent_while_wake_uses_payload,
            "detachment must not authorize reclamation while the IRQ wake still reads its payload"
        );
    });
    assert_eq!(state.completed.load(Ordering::Acquire), 1);
    assert!(token.is_quiescent());
    token.try_quiesce().unwrap();
}

fn expect_registered<'cell, 'registration>(
    result: IrqRegisterResult<'cell, 'registration>,
) -> IrqWaitToken<'cell, 'registration> {
    match result {
        IrqRegisterResult::Registered(token) => token,
        other => panic!("expected a registered IRQ waiter, got {other:?}"),
    }
}

struct TestRegistration {
    registration: ManuallyDrop<IrqWaitRegistration>,
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
            registration: ManuallyDrop::new(IrqWaitRegistration::new_test(wake)),
            wakes,
        }
    }

    fn registration(&self) -> &IrqWaitRegistration {
        &self.registration
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
