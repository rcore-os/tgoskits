//! OS runtime capability injection.
//!
//! The driver core never references a concrete kernel's runtime crate. Instead
//! the OS glue layer installs a [`WifiRuntime`] implementation once at startup
//! via [`set_runtime`], and the core reaches timing / delay / yield through
//! [`runtime`].

extern crate alloc;

use alloc::boxed::Box;
use core::{
    sync::atomic::{AtomicPtr, Ordering},
    task::{Context, Poll},
};

/// A poll body provided by the driver core: invoked with a task context,
/// returns `Poll::Ready(())` when the operation is complete (or the task should
/// exit) and `Poll::Pending` otherwise. The OS glue drives it via its executor.
pub type PollFn<'a> = dyn FnMut(&mut Context<'_>) -> Poll<()> + 'a;

/// A pollable body that can be sent to another task (for background tasks).
pub type SendPollFn = dyn FnMut(&mut Context<'_>) -> Poll<()> + Send;

/// Returned by [`WifiRuntime::block_until`] when the deadline elapsed before the
/// poll body completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedOut;

/// OS runtime capabilities the Wi-Fi driver core needs.
///
/// The core itself depends on no concrete kernel runtime crate; it obtains
/// timing, delay and yield capabilities through this trait. The OS glue layer
/// implements and injects it.
///
/// `spawn_poll_task` starts the driver's background polling loops (RX/TX/AP).
pub trait WifiRuntime: Send + Sync + 'static {
    /// Monotonic clock in nanoseconds. Used for timeouts and elapsed-time math.
    fn now_nanos(&self) -> u64;

    /// Blocking delay for the given milliseconds (init/firmware power-up only).
    fn sleep_ms(&self, ms: u64);

    /// Yield the CPU to other tasks (while polling for hardware readiness).
    fn yield_now(&self);

    /// Start a named background polling task.
    ///
    /// `poll` is the core-provided poll body (no OS executor details): each call
    /// returns `Poll::Pending` while unfinished, `Poll::Ready(())` when the task
    /// should exit. The glue drives it with the kernel's executor (e.g.
    /// `block_on(poll_fn(...))`) and re-polls when the associated waker fires.
    fn spawn_poll_task(&self, name: &str, poll: Box<SendPollFn>);

    /// Block the current task until `poll` returns `Poll::Ready`, waiting at
    /// most `timeout_ms` milliseconds (`None` = unbounded). Returns [`TimedOut`]
    /// on timeout.
    fn block_until(&self, timeout_ms: Option<u64>, poll: &mut PollFn<'_>) -> Result<(), TimedOut>;
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
