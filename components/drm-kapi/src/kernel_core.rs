// SPDX-License-Identifier: MIT
//! Linux core services shared by the DRM implementation.
//!
//! Device, PCI, power-management, and bus operations are deliberately absent;
//! those belong to a concrete driver or operating-system adapter.

use alloc::vec::Vec;
use core::ffi::{c_int, c_void};

use spin::Mutex;

/// Managed DRM actions, executed newest first when the owning DRM device is
/// released. Each record stores the device pointer, callback, and payload.
static DRMM_ACTIONS: Mutex<Vec<(usize, usize, usize)>> = Mutex::new(Vec::new());

/// Linux `drmm_add_action_or_reset()`. If the action cannot be recorded, it
/// runs immediately and the allocation error is returned, as in Linux.
///
/// # Safety
/// `drm` must be a live device identity and `action` must remain callable with
/// `drm` and `data` until device release; `data` must remain valid for that
/// callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drmm_add_action_or_reset(
    drm: *mut c_void,
    action: unsafe extern "C" fn(*mut c_void, *mut c_void),
    data: *mut c_void,
) -> c_int {
    if drm.is_null() {
        return -crate::config::EINVAL;
    }
    let mut actions = DRMM_ACTIONS.lock();
    if actions.try_reserve(1).is_err() {
        drop(actions);
        // SAFETY: The caller guarantees that `action` is callable with `drm` and `data`.
        // On reservation failure, Linux drmm semantics require immediate execution of the cleanup action.
        unsafe { action(drm, data) };
        return -crate::config::ENOMEM;
    }
    actions.push((drm as usize, action as usize, data as usize));
    0
}

/// Run and remove the actions registered for `drm`, newest first. Called by
/// the DRM device release path together with Linux's `drmm_release_all()`.
///
/// # Safety
/// `drm` must identify a live device whose release has begun; no concurrent
/// registration or second release may use that device identity.
pub unsafe fn drmm_run_actions(drm: *mut c_void) {
    let mut pending = {
        let mut actions = DRMM_ACTIONS.lock();
        let mut mine = Vec::new();
        let mut index = 0;
        while index < actions.len() {
            if actions[index].0 == drm as usize {
                mine.push(actions.remove(index));
            } else {
                index += 1;
            }
        }
        mine
    };
    while let Some((_, action, data)) = pending.pop() {
        // SAFETY: function pointer values originate from the callbacks passed
        // to `drmm_add_action_or_reset` and remain valid until device release.
        let action: unsafe extern "C" fn(*mut c_void, *mut c_void) =
            unsafe { core::mem::transmute(action) };
        // SAFETY: `action` was registered via `drmm_add_action_or_reset` with `data`.
        // The caller guarantees `drm` is live during device release, and `data` remains
        // valid for this callback until release completes.
        unsafe { action(drm, data as *mut c_void) };
    }
}

/// Linux `synchronize_rcu()`, implemented by the shared RCU mechanism.
#[unsafe(export_name = "synchronize_rcu")]
pub extern "C" fn synchronize_rcu() {
    crate::rcu::cond_synchronize_rcu(crate::rcu::get_state_synchronize_rcu());
}
