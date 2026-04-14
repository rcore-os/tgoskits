//! [ArceOS](https://github.com/arceos-org/arceos) synchronization primitives.
//!
//! Currently supported primitives:
//!
//! - [`Mutex`]: A mutual exclusion primitive.
//! - mod [`spin`]: spinlocks imported from the [`ax-kspin`] crate.
//!
//! # Cargo Features
//!
//! - `multitask`: For use in the multi-threaded environments. If the feature is
//!   not enabled, [`Mutex`] will be an alias of [`spin::SpinNoIrq`]. This
//!   feature is enabled by default.

#![cfg_attr(any(not(test), target_os = "none"), no_std)]
#![cfg_attr(all(test, target_os = "none"), no_main)]
#![cfg_attr(all(test, target_os = "none"), feature(custom_test_frameworks))]
#![cfg_attr(doc, feature(doc_cfg))]
#![cfg_attr(
    all(test, target_os = "none"),
    test_runner(crate::bare_metal_test_runner)
)]

pub use ax_kspin as spin;

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

#[cfg(feature = "multitask")]
mod mutex;

#[cfg(not(feature = "multitask"))]
#[cfg_attr(doc, doc(cfg(not(feature = "multitask"))))]
pub use ax_kspin::{SpinNoIrq as Mutex, SpinNoIrqGuard as MutexGuard};

#[cfg(feature = "multitask")]
#[cfg_attr(doc, doc(cfg(feature = "multitask")))]
pub use self::mutex::{Mutex, MutexGuard, RawMutex};
