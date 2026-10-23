// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Linux task-signal predicate adapter.
//!
//! The process signal owner lives above this crate. It installs the callback
//! that maps Linux task state and pending-signal policy to the native task;
//! calling the LinuxKPI entry point before that owner is installed fails
//! closed rather than treating a pending signal as absent.

use core::{
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

pub type SignalPendingState = unsafe extern "C" fn(state: i32, task: *mut c_void) -> bool;

static SIGNAL_PENDING_STATE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

/// Install the process owner's exact Linux `signal_pending_state()` policy.
/// Reinstalling the same provider is harmless; replacing it is rejected.
pub fn install_signal_pending_state(provider: SignalPendingState) -> Result<(), &'static str> {
    let candidate = provider as *mut c_void;
    match SIGNAL_PENDING_STATE.compare_exchange(
        ptr::null_mut(),
        candidate,
        Ordering::Release,
        Ordering::Acquire,
    ) {
        Ok(_) => Ok(()),
        Err(existing) if existing == candidate => Ok(()),
        Err(_) => Err("Linux signal predicate provider already installed"),
    }
}

/// Linux C ABI used by source-order DRM callers.
///
/// # Safety
/// `task` must be a live opaque task identity accepted by the installed signal
/// provider; it is passed through without dereferencing by this crate.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn signal_pending_state(state: i32, task: *mut c_void) -> bool {
    let provider = SIGNAL_PENDING_STATE.load(Ordering::Acquire);
    assert!(
        !provider.is_null(),
        "Linux signal policy must be installed before upstream GT waits"
    );
    assert!(!task.is_null(), "signal_pending_state received a null task");
    // SAFETY: `provider` was stored from a valid non-null `SignalPendingState` function pointer
    // in `install_signal_pending_state` and asserted non-null above. Function pointer and pointer types
    // have identical size and layout on all supported platforms.
    let provider: SignalPendingState = unsafe { core::mem::transmute(provider) };
    // SAFETY: `provider` is a valid host-installed function pointer. `task` is non-null and the
    // caller guaranteed it is a live opaque task identity accepted by the installed provider.
    unsafe { provider(state, task) }
}

/// Linux `signal_pending()` for translated interruptible execbuffer waits.
///
/// # Safety
/// `task` must be a live opaque task identity accepted by the installed signal
/// provider; it is passed through without dereferencing by this crate.
pub unsafe fn signal_pending(task: *mut c_void) -> bool {
    // SAFETY: Propagates the caller's guarantee that `task` is a live opaque task identity accepted
    // by the installed signal provider to `signal_pending_state`.
    unsafe { signal_pending_state(crate::wait::TASK_INTERRUPTIBLE as i32, task) }
}

/// Check the current task using its opaque scheduler identity. The host callback
/// must treat the pointer-shaped token as an integer identity, never dereference it.
pub fn signal_pending_current() -> bool {
    let Some(task) = crate::task_runtime::current_task_id()
        .and_then(|task| usize::try_from(task).ok())
        .filter(|task| *task != 0)
    else {
        return false;
    };
    let task_ptr = task as *mut c_void;
    // SAFETY: `task_ptr` is derived from the non-zero integer ID of the currently executing task,
    // which is valid and live for the duration of this call and matches the opaque token contract.
    unsafe { signal_pending(task_ptr) }
}
