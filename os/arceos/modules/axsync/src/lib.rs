//! [ArceOS](https://github.com/arceos-org/arceos) synchronization primitives.
//!
//! Currently supported primitives:
//!
//! - [`SpinMutex`]: A non-sleeping, IRQ-safe mutual exclusion primitive.
//! - `PiMutex`: An urgency-ordered priority-inheritance sleeping mutex,
//!   available with `multitask`.
//! - mod [`spin`]: spinlocks imported from the `ax-kspin` crate.
//!
//! # Cargo Features
//!
//! - `multitask`: Enables the urgency-ordered priority-inheritance sleeping
//!   `PiMutex`.

#![cfg_attr(any(not(test), target_os = "none"), no_std)]
#![cfg_attr(all(test, target_os = "none"), no_main)]
#![cfg_attr(all(test, target_os = "none"), feature(custom_test_frameworks))]
#![cfg_attr(doc, feature(doc_cfg))]
#![cfg_attr(
    all(test, target_os = "none"),
    test_runner(crate::bare_metal_test_runner)
)]

extern crate alloc;

pub use ax_kspin as spin;
pub use ax_kspin::{SpinNoIrq as SpinMutex, SpinNoIrqGuard as SpinMutexGuard};

#[cfg(all(test, target_os = "none"))]
fn bare_metal_test_runner(_tests: &[&dyn Fn()]) {}

#[cfg(all(test, target_os = "none"))]
#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(all(test, target_os = "none"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(all(feature = "multitask", feature = "lockdep"))]
mod lockdep;

#[cfg(feature = "multitask")]
mod mutex;
#[cfg(all(test, feature = "multitask", not(target_os = "none")))]
mod test_runtime;

#[cfg(feature = "multitask")]
#[cfg_attr(doc, doc(cfg(feature = "multitask")))]
pub use self::mutex::{
    InterruptibleMutexExt, LockSubclass, LockdepMutexExt, PiMutexLockInterrupted, RawMutex,
};
#[cfg(feature = "multitask")]
#[cfg_attr(doc, doc(cfg(feature = "multitask")))]
pub use self::mutex::{Mutex as PiMutex, MutexGuard as PiMutexGuard, RawMutex as RawPiMutex};

#[cfg(all(test, not(target_os = "none")))]
mod public_api_tests {
    #[cfg(feature = "multitask")]
    use core::marker::PhantomData;

    use super::{SpinMutex, SpinMutexGuard};

    trait SameType<T: ?Sized> {}

    impl<T: ?Sized> SameType<T> for T {}

    fn assert_same_type<T: ?Sized + SameType<U>, U: ?Sized>() {}

    #[test]
    fn spin_mutex_names_irq_safe_spin_semantics() {
        assert_same_type::<SpinMutex<u8>, ax_kspin::SpinNoIrq<u8>>();
        assert_same_type::<SpinMutexGuard<'static, u8>, ax_kspin::SpinNoIrqGuard<'static, u8>>();
    }

    #[cfg(feature = "multitask")]
    #[test]
    fn multitask_exposes_priority_inheritance_mutex_explicitly() {
        let _mutex = PhantomData::<super::PiMutex<u8>>;
        let _guard = PhantomData::<super::PiMutexGuard<'static, u8>>;
    }
}
