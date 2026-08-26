//! OS runtime capability injection.
//!
//! The driver core never references a concrete kernel's runtime crate. Instead
//! the OS glue layer installs a [`WifiRuntime`] implementation once at startup
//! via [`set_runtime`], and the core reaches timing / delay / yield through
//! [`runtime`].

extern crate alloc;

use alloc::boxed::Box;
use core::sync::atomic::{AtomicPtr, Ordering};

/// OS runtime capabilities the Wi-Fi driver core needs.
///
/// The core itself depends on no concrete kernel runtime crate; it obtains
/// timing, delay and yield capabilities through this trait. The OS glue layer
/// implements and injects it.
///
/// Queue ownership and task creation intentionally do not cross this boundary:
/// the unified network runtime owns the sole fixed-CPU executor.
pub trait WifiRuntime: Send + Sync + 'static {
    /// Monotonic clock in nanoseconds. Used for timeouts and elapsed-time math.
    fn now_nanos(&self) -> u64;

    /// Blocking delay for the given milliseconds (init/firmware power-up only).
    fn sleep_ms(&self, ms: u64);

    /// Yield the fixed owner CPU task while an active hardware transaction is
    /// waiting for progress.
    fn yield_now(&self);
}

static RUNTIME: AtomicPtr<&'static dyn WifiRuntime> = AtomicPtr::new(core::ptr::null_mut());

/// Installs the OS runtime capability provider. Call once during init, before
/// any driver operation that needs timing/delay/yield.
pub fn set_runtime(rt: &'static dyn WifiRuntime) {
    // Box the fat pointer so we can store it behind a single thin AtomicPtr.
    let boxed = Box::new(rt);
    let ptr = Box::into_raw(boxed);
    let old = RUNTIME.swap(ptr, Ordering::AcqRel);
    if !old.is_null() {
        // Drop the previously installed provider reference.
        unsafe { drop(Box::from_raw(old)) };
    }
}

/// Returns the installed runtime provider.
///
/// # Panics
/// Panics if [`set_runtime`] was not called first — that is a driver
/// integration bug in the OS glue layer.
pub(crate) fn runtime() -> &'static dyn WifiRuntime {
    let ptr = RUNTIME.load(Ordering::Acquire);
    assert!(
        !ptr.is_null(),
        "aic8800: WifiRuntime not installed; call aic8800::set_runtime() during init"
    );
    unsafe { *ptr }
}
