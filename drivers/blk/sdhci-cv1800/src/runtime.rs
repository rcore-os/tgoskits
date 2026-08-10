//! OS timing capability injection.
//!
//! The controller driver needs millisecond delays and a CPU-yield while polling
//! hardware, but must not bind to any specific kernel's task runtime. The OS
//! glue installs a [`SdhciDelay`] provider once via [`set_delay`]; the driver
//! reaches it through [`delay`].

use core::sync::atomic::{AtomicPtr, Ordering};

/// Timing capabilities the SDHCI controller needs from the OS.
pub trait SdhciDelay: Send + Sync + 'static {
    /// Blocking delay for the given milliseconds.
    fn delay_ms(&self, ms: u64);
    /// Yield the CPU to other tasks while polling hardware.
    fn yield_now(&self);
}

static DELAY: AtomicPtr<&'static dyn SdhciDelay> = AtomicPtr::new(core::ptr::null_mut());

/// Installs the timing capability provider. Call once during init, before
/// driving the controller.
pub fn set_delay(provider: &'static dyn SdhciDelay) {
    let boxed = alloc::boxed::Box::new(provider);
    let ptr = alloc::boxed::Box::into_raw(boxed);
    let old = DELAY.swap(ptr, Ordering::AcqRel);
    if !old.is_null() {
        unsafe { drop(alloc::boxed::Box::from_raw(old)) };
    }
}

pub(crate) fn delay() -> &'static dyn SdhciDelay {
    let ptr = DELAY.load(Ordering::Acquire);
    assert!(
        !ptr.is_null(),
        "sdhci-cv1800: SdhciDelay not installed; call sdhci_cv1800::set_delay() during init"
    );
    unsafe { *ptr }
}
