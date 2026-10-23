// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_vblank_helper.c` (Virtio V0).

use drm_kapi::locks::{spin_lock_irq, spin_unlock_irq};

use crate::{
    drm_atomic_types::DrmAtomicCommit, drm_atomic_upstream::drm_atomic_get_new_crtc_state,
    drm_crtc_types::DrmCrtc,
};

// upstream: drivers/gpu/drm/drm_vblank.c
unsafe extern "C" {
    fn drm_crtc_vblank_get(crtc: *mut DrmCrtc) -> i32;
    fn drm_crtc_arm_vblank_event(
        crtc: *mut DrmCrtc,
        event: *mut crate::drm_vblank_types::DrmPendingVblankEvent,
    );
    fn drm_crtc_send_vblank_event(
        crtc: *mut DrmCrtc,
        event: *mut crate::drm_vblank_types::DrmPendingVblankEvent,
    );
    fn drm_crtc_vblank_on(crtc: *mut DrmCrtc);
    fn drm_crtc_vblank_off(crtc: *mut DrmCrtc);
    fn drm_crtc_vblank_start_timer(crtc: *mut DrmCrtc) -> i32;
    fn drm_crtc_vblank_cancel_timer(crtc: *mut DrmCrtc);
    fn drm_crtc_vblank_get_vblank_timeout(crtc: *mut DrmCrtc, timestamp: *mut i64);
}

// upstream: drivers/gpu/drm/drm_vblank_helper.c drm_crtc_vblank_atomic_flush()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `crtc` must be non-null, properly aligned, and point to an initialized `DrmCrtc` whose `dev`
///   field points to a live `DrmDevice` with an initialized `event_lock`.
/// - `state` must point to a live `DrmAtomicCommit` that contains an active `new_crtc_state` for `crtc`.
/// - Caller must hold the modeset commit lock or ensure no concurrent modifications to `state` and `crtc`.
pub unsafe extern "C" fn drm_crtc_vblank_atomic_flush(
    crtc: *mut DrmCrtc,
    state: *mut DrmAtomicCommit,
) {
    // SAFETY:
    // - `crtc` points to an initialized `DrmCrtc` and `(*crtc).dev` points to an initialized `DrmDevice`
    //   per caller contract.
    // - `drm_atomic_get_new_crtc_state` retrieves the new CRTC state for `crtc` from `state`.
    // - Caller guarantees that `state` contains `crtc`, so `crtc_state` is non-null.
    // - `spin_lock_irq` / `spin_unlock_irq` protect access to `(*dev).event_lock` and the event pointer.
    // - If `event` is non-null, it is consumed and armed/sent via `drm_crtc_arm_vblank_event` or
    //   `drm_crtc_send_vblank_event`.
    unsafe {
        let dev = (*crtc).dev;
        let crtc_state = drm_atomic_get_new_crtc_state(state, crtc);
        let event = (*crtc_state).event;
        spin_lock_irq(&mut (*dev).event_lock);
        (*crtc_state).event = core::ptr::null_mut();
        if !event.is_null() {
            if drm_crtc_vblank_get(crtc) == 0 {
                drm_crtc_arm_vblank_event(crtc, event.cast());
            } else {
                drm_crtc_send_vblank_event(crtc, event.cast());
            }
        }
        spin_unlock_irq(&mut (*dev).event_lock);
    }
}

// upstream: drivers/gpu/drm/drm_vblank_helper.c drm_crtc_vblank_atomic_enable()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `crtc` must be non-null, properly aligned, and point to an initialized `DrmCrtc`.
/// - `_state` is unused and may be any pointer or null.
pub unsafe extern "C" fn drm_crtc_vblank_atomic_enable(
    crtc: *mut DrmCrtc,
    _state: *mut DrmAtomicCommit,
) {
    // SAFETY: `crtc` is non-null, properly aligned, and points to an initialized `DrmCrtc` per caller
    // contract. `drm_crtc_vblank_on` enables vblank tracking for this CRTC.
    unsafe { drm_crtc_vblank_on(crtc) };
}

// upstream: drivers/gpu/drm/drm_vblank_helper.c drm_crtc_vblank_atomic_disable()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `crtc` must be non-null, properly aligned, and point to an initialized `DrmCrtc`.
/// - `_state` is unused and may be any pointer or null.
pub unsafe extern "C" fn drm_crtc_vblank_atomic_disable(
    crtc: *mut DrmCrtc,
    _state: *mut DrmAtomicCommit,
) {
    // SAFETY: `crtc` is non-null, properly aligned, and points to an initialized `DrmCrtc` per caller
    // contract. `drm_crtc_vblank_off` disables vblank tracking for this CRTC.
    unsafe { drm_crtc_vblank_off(crtc) };
}

// upstream: drivers/gpu/drm/drm_vblank_helper.c drm_crtc_vblank_helper_enable_vblank_timer()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be non-null, properly aligned, and point to an initialized `DrmCrtc` with vblank
/// state allocated.
pub unsafe extern "C" fn drm_crtc_vblank_helper_enable_vblank_timer(crtc: *mut DrmCrtc) -> i32 {
    // SAFETY: `crtc` is non-null, aligned, and points to an initialized `DrmCrtc` per caller contract.
    // `drm_crtc_vblank_start_timer` starts the high-resolution vblank simulation timer for this CRTC.
    unsafe { drm_crtc_vblank_start_timer(crtc) }
}

// upstream: drivers/gpu/drm/drm_vblank_helper.c drm_crtc_vblank_helper_disable_vblank_timer()
#[unsafe(no_mangle)]
/// # Safety
///
/// `crtc` must be non-null, properly aligned, and point to an initialized `DrmCrtc`.
pub unsafe extern "C" fn drm_crtc_vblank_helper_disable_vblank_timer(crtc: *mut DrmCrtc) {
    // SAFETY: `crtc` is non-null, aligned, and points to an initialized `DrmCrtc` per caller contract.
    // `drm_crtc_vblank_cancel_timer` cancels any pending software vblank timer on this CRTC.
    unsafe { drm_crtc_vblank_cancel_timer(crtc) };
}

// upstream: drivers/gpu/drm/drm_vblank_helper.c drm_crtc_vblank_helper_get_vblank_timestamp_from_timer()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `crtc` must be non-null, properly aligned, and point to an initialized `DrmCrtc`.
/// - `vblank_time` must be non-null, properly aligned for `i64`, and point to writable storage
///   with no conflicting active borrows.
/// - `_max_error` and `_in_vblank_irq` are unused by the underlying timer query.
pub unsafe extern "C" fn drm_crtc_vblank_helper_get_vblank_timestamp_from_timer(
    crtc: *mut DrmCrtc,
    _max_error: *mut i32,
    vblank_time: *mut i64,
    _in_vblank_irq: bool,
) -> bool {
    // SAFETY: `crtc` points to an initialized `DrmCrtc` and `vblank_time` is an aligned, writable
    // pointer to `i64` storage as guaranteed by caller contract.
    unsafe { drm_crtc_vblank_get_vblank_timeout(crtc, vblank_time) };
    true
}
