//! OS runtime capability injection.
//!
//! The driver core never references a concrete kernel's runtime crate. Instead
//! the OS glue layer installs a [`WifiRuntime`] implementation once at startup
//! via [`set_runtime`], and the core reaches timing / delay / yield through
//! [`runtime`].

use core::sync::atomic::{AtomicPtr, Ordering};

use wifi_host::WifiRuntime;

static RUNTIME: AtomicPtr<&'static dyn WifiRuntime> = AtomicPtr::new(core::ptr::null_mut());

/// Installs the OS runtime capability provider. Call once during init, before
/// any driver operation that needs timing/delay/yield.
pub fn set_runtime(rt: &'static dyn WifiRuntime) {
    // Box the fat pointer so we can store it behind a single thin AtomicPtr.
    let boxed = alloc::boxed::Box::new(rt);
    let ptr = alloc::boxed::Box::into_raw(boxed);
    let old = RUNTIME.swap(ptr, Ordering::AcqRel);
    if !old.is_null() {
        // Drop the previously installed provider reference.
        unsafe { drop(alloc::boxed::Box::from_raw(old)) };
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
