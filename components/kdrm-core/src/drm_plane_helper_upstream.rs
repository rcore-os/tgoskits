// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_plane_helper.c`.
//!
//! Legacy (non-atomic) plane helpers: primary-plane update and disable
//! implemented through the CRTC `set_config` hook. Functions are named and
//! ordered as in the C file.

// The Linux ports state the invariant of each C-ABI call once in the module
// header, as the GT ports do; per-block SAFETY prose is not repeated.
#![allow(non_snake_case)]

use core::mem::{size_of, zeroed};

use drm_kapi::{
    assert::warn_on,
    config::{EINVAL, ENOMEM, GFP_KERNEL},
};

use crate::{
    drm_color_mgmt_upstream::drm_drv_uses_atomic_modeset,
    drm_connector_types::{DrmConnector, DrmConnectorListIter},
    drm_crtc_types::{DrmCrtc, DrmCrtcState, DrmModeSet},
    drm_device_types::DrmDevice,
    drm_framebuffer_types::DrmFramebuffer,
    drm_modeset_lock_types::DrmModesetAcquireCtx,
    drm_plane_types::{DrmPlane, DrmPlaneState},
    drm_rect_types::DrmRect,
};

/// `DRM_MODE_ROTATE_0` (include/uapi/drm/drm_mode.h).
const DRM_MODE_ROTATE_0: u32 = 1 << 0;
/// `DRM_PLANE_NO_SCALING` (include/drm/drm_atomic_helper.h).
const DRM_PLANE_NO_SCALING: i32 = 1 << 16;

unsafe extern "C" {
    // upstream: drm_atomic_helper.c drm_atomic_helper_check_plane_state()
    fn drm_atomic_helper_check_plane_state(
        plane_state: *mut DrmPlaneState,
        crtc_state: *const DrmCrtcState,
        min_scale: i32,
        max_scale: i32,
        can_position: bool,
        can_update_disabled: bool,
    ) -> i32;
    // upstream: drm_connector.c drm_connector_list_iter_begin/next/end()
    fn drm_connector_list_iter_begin(dev: *mut DrmDevice, iter: *mut DrmConnectorListIter);
    fn drm_connector_list_iter_next(iter: *mut DrmConnectorListIter) -> *mut DrmConnector;
    fn drm_connector_list_iter_end(iter: *mut DrmConnectorListIter);
    // upstream: kernel/locking/ww_mutex.c ww_mutex_is_locked() (via drm_modeset_is_locked)
    fn ww_mutex_is_locked(lock: *mut drm_kapi::ww_mutex::WwMutex) -> bool;
}

// upstream: include/drm/drm_rect.h drm_rect_width()
#[inline]
fn drm_rect_width(r: &DrmRect) -> i32 {
    r.x2 - r.x1
}

// upstream: include/drm/drm_rect.h drm_rect_height()
#[inline]
fn drm_rect_height(r: &DrmRect) -> i32 {
    r.y2 - r.y1
}

// upstream: include/drm/drm_modeset_lock.h drm_modeset_is_locked()
#[inline]
/// # Safety
/// - `lock` must be non-null, properly aligned, and point to an initialized `DrmModesetLock`.
/// - `lock` must remain live for the duration of the call.
unsafe fn drm_modeset_is_locked(lock: *mut crate::drm_modeset_lock_types::DrmModesetLock) -> bool {
    // SAFETY: `lock` is non-null, aligned, and points to an initialized `DrmModesetLock`.
    // Computing `core::ptr::addr_of_mut!((*lock).mutex)` yields a valid pointer to the embedded
    // `WwMutex`, which `ww_mutex_is_locked` inspects without modifying or moving it.
    unsafe { ww_mutex_is_locked(core::ptr::addr_of_mut!((*lock).mutex)) }
}

/// Counts the connectors that currently feed `crtc`, copying up to
/// `num_connectors` of them into `connector_list` when it is non-null.
// upstream: drm_plane_helper.c get_connectors_for_crtc()
/// # Safety
/// - `crtc` must be non-null, properly aligned, and point to an initialized `DrmCrtc`.
/// - `(*crtc).dev` must point to an initialized `DrmDevice`.
/// - The caller must hold `mode_config.connection_mutex` on `(*crtc).dev` to protect the connector list
///   and encoder routing against concurrent modification.
/// - If `connector_list` is non-null, `num_connectors` must be non-negative and `connector_list` must
///   point to writable, unaliased memory with capacity for at least `num_connectors as usize` pointers.
unsafe fn get_connectors_for_crtc(
    crtc: *mut DrmCrtc,
    mut connector_list: *mut *mut DrmConnector,
    num_connectors: i32,
) -> i32 {
    // SAFETY: `crtc` and `(*crtc).dev` are valid per caller preconditions. `DrmConnectorListIter` is
    // zero-initialized and driven through `drm_connector_list_iter_*` which safely traverses `dev.mode_config.connector_list`.
    // For each connector whose encoder matches `crtc`, if `connector_list` is non-null and `count < num_connectors`,
    // writing the connector pointer is within the allocated bounds.
    unsafe {
        let dev = (*crtc).dev;
        let mut count: i32 = 0;

        // Note: Once we change the plane hooks to more fine-grained locking we
        // need to grab the connection_mutex here to be able to make these checks.
        warn_on(!drm_modeset_is_locked(core::ptr::addr_of_mut!(
            (*dev).mode_config.connection_mutex
        )));

        let mut iter: DrmConnectorListIter = zeroed();
        drm_connector_list_iter_begin(dev, &mut iter);
        loop {
            let connector = drm_connector_list_iter_next(&mut iter);
            if connector.is_null() {
                break;
            }
            let encoder = (*connector).encoder;
            if !encoder.is_null() && (*encoder).crtc == crtc {
                if !connector_list.is_null() && count < num_connectors {
                    *connector_list = connector;
                    connector_list = connector_list.add(1);
                }
                count += 1;
            }
        }
        drm_connector_list_iter_end(&mut iter);

        count
    }
}

/// Runs the atomic plane-state check on a temporary state built from the
/// legacy request, returning the clipped src/dst and visibility.
// upstream: drm_plane_helper.c drm_plane_helper_check_update()
/// # Safety
/// - `plane`, `crtc`, and `fb` must be non-null, properly aligned, and point to live initialized objects.
/// - `src` and `dst` must be non-null, properly aligned, point to initialized `DrmRect` values, and be writable.
/// - `visible` must be non-null, properly aligned, and point to writable `bool` storage.
/// - The caller must hold the necessary modeset locks for the CRTC and plane.
// Keeps the Linux function signature.
#[allow(clippy::too_many_arguments)]
unsafe fn drm_plane_helper_check_update(
    plane: *mut DrmPlane,
    crtc: *mut DrmCrtc,
    fb: *mut DrmFramebuffer,
    src: *mut DrmRect,
    dst: *mut DrmRect,
    rotation: u32,
    min_scale: i32,
    max_scale: i32,
    can_position: bool,
    can_update_disabled: bool,
    visible: *mut bool,
) -> i32 {
    // SAFETY: Stack-allocated `plane_state` and `crtc_state` are initialized via `zeroed()` and populated
    // with caller-verified `plane`, `crtc`, `fb`, and coordinate fields. `copy_nonoverlapping` safely copies
    // `(*crtc).mode` into `crtc_state.mode` (both are aligned and non-overlapping).
    // `drm_atomic_helper_check_plane_state` inspects and updates the temporary states; on success (return 0),
    // writing back into `*src`, `*dst`, and `*visible` satisfies non-null and mutability preconditions.
    unsafe {
        let mut plane_state: DrmPlaneState = zeroed();
        plane_state.plane = plane;
        plane_state.crtc = crtc;
        plane_state.fb = fb;
        plane_state.src_x = (*src).x1 as u32;
        plane_state.src_y = (*src).y1 as u32;
        plane_state.src_w = drm_rect_width(&*src) as u32;
        plane_state.src_h = drm_rect_height(&*src) as u32;
        plane_state.crtc_x = (*dst).x1;
        plane_state.crtc_y = (*dst).y1;
        plane_state.crtc_w = drm_rect_width(&*dst) as u32;
        plane_state.crtc_h = drm_rect_height(&*dst) as u32;
        plane_state.rotation = rotation;

        let mut crtc_state: DrmCrtcState = zeroed();
        crtc_state.crtc = crtc;
        crtc_state.enable = (*crtc).enabled;
        core::ptr::copy_nonoverlapping(
            core::ptr::addr_of!((*crtc).mode),
            core::ptr::addr_of_mut!(crtc_state.mode),
            1,
        );

        let ret = drm_atomic_helper_check_plane_state(
            &mut plane_state,
            &crtc_state,
            min_scale,
            max_scale,
            can_position,
            can_update_disabled,
        );
        if ret != 0 {
            return ret;
        }

        *src = plane_state.src;
        *dst = plane_state.dst;
        *visible = plane_state.visible;
        0
    }
}

/// Legacy primary-plane update: programs the CRTC through `set_config` with
/// the connectors currently attached to it.
// upstream: drm_plane_helper.c drm_plane_helper_update_primary()
#[unsafe(no_mangle)]
/// # Safety
/// - `plane` must be non-null, properly aligned, and point to an initialized `DrmPlane` whose `dev` and `funcs`
///   fields are valid. If the plane is determined to be invisible, `(*(*plane).funcs).disable_plane` must be `Some`.
/// - `crtc` must be non-null, properly aligned, and point to an initialized `DrmCrtc` with valid `funcs` and
///   `mode` fields, and `(*(*crtc).funcs).set_config` must be `Some`.
/// - `fb` must be non-null, properly aligned, and point to a valid `DrmFramebuffer`.
/// - `ctx` may be null or point to a valid `DrmModesetAcquireCtx`.
/// - The caller must follow legacy DRM modeset locking rules and hold `(*(*plane).dev).mode_config.connection_mutex`.
/// - The underlying driver must not use atomic modesetting (`drm_drv_uses_atomic_modeset` must be false).
/// - At least one connector must currently be routed to `crtc`.
pub unsafe extern "C" fn drm_plane_helper_update_primary(
    plane: *mut DrmPlane,
    crtc: *mut DrmCrtc,
    fb: *mut DrmFramebuffer,
    crtc_x: i32,
    crtc_y: i32,
    crtc_w: u32,
    crtc_h: u32,
    src_x: u32,
    src_y: u32,
    src_w: u32,
    src_h: u32,
    ctx: *mut DrmModesetAcquireCtx,
) -> i32 {
    // SAFETY: `plane`, `crtc`, and `fb` are live, aligned objects under modeset locks.
    // Stack-allocated `set`, `src`, and `dest` rects are initialized and checked through `drm_plane_helper_check_update`.
    // If not visible, delegating to `disable_plane` is valid per plane vtable contract.
    // Otherwise, `connector_list` is allocated via `kzalloc` with size `num_connectors * sizeof(*mut DrmConnector)`,
    // checked against null, populated via `get_connectors_for_crtc`, passed to `set_config`, and freed via `kfree`.
    unsafe {
        let mut set: DrmModeSet = zeroed();
        set.crtc = crtc;
        set.fb = fb;
        set.mode = core::ptr::addr_of_mut!((*crtc).mode);
        set.x = src_x >> 16;
        set.y = src_y >> 16;

        let mut src = DrmRect {
            x1: src_x as i32,
            y1: src_y as i32,
            x2: src_x.wrapping_add(src_w) as i32,
            y2: src_y.wrapping_add(src_h) as i32,
        };
        let mut dest = DrmRect {
            x1: crtc_x,
            y1: crtc_y,
            x2: crtc_x.wrapping_add(crtc_w as i32),
            y2: crtc_y.wrapping_add(crtc_h as i32),
        };

        let dev = (*plane).dev;
        let mut visible = false;

        if warn_on(drm_drv_uses_atomic_modeset(dev)) {
            return -EINVAL;
        }

        let ret = drm_plane_helper_check_update(
            plane,
            crtc,
            fb,
            &mut src,
            &mut dest,
            DRM_MODE_ROTATE_0,
            DRM_PLANE_NO_SCALING,
            DRM_PLANE_NO_SCALING,
            false,
            false,
            &mut visible,
        );
        if ret != 0 {
            return ret;
        }

        if !visible {
            // Primary plane isn't visible. Note that unless a driver provides
            // their own disable function, this will just wind up returning
            // -EINVAL to userspace.
            return (*(*plane).funcs).disable_plane.expect("disable_plane")(plane, ctx);
        }

        // Find current connectors for CRTC
        let num_connectors = get_connectors_for_crtc(crtc, core::ptr::null_mut(), 0);
        drm_kapi::BUG_ON!(num_connectors == 0);

        // kzalloc_objs(*connector_list, num_connectors)
        let connector_list = drm_kapi::memory::kzalloc(
            num_connectors as usize * size_of::<*mut DrmConnector>(),
            GFP_KERNEL,
        ) as *mut *mut DrmConnector;
        if connector_list.is_null() {
            return -ENOMEM;
        }
        get_connectors_for_crtc(crtc, connector_list, num_connectors);
        set.connectors = connector_list;
        set.num_connectors = num_connectors as usize;

        // We call set_config() directly here rather than using
        // drm_mode_set_config_internal. We're reprogramming the same connectors
        // that were already in use, so we shouldn't need the extra cross-CRTC
        // fb refcounting to accommodate stealing connectors. drm_mode_setplane()
        // already handles the basic refcounting for the framebuffers involved in
        // this operation.
        let ret = (*(*crtc).funcs).set_config.expect("set_config")(&mut set, ctx);
        drm_kapi::memory::kfree(connector_list);
        ret
    }
}

/// Primary planes cannot be disabled through the legacy helper.
// upstream: drm_plane_helper.c drm_plane_helper_disable_primary()
#[unsafe(no_mangle)]
/// # Safety
/// - `plane` must be non-null, properly aligned, and point to an initialized `DrmPlane` whose `dev` field
///   points to a valid `DrmDevice`.
/// - `_ctx` is ignored and may be null or any pointer.
pub unsafe extern "C" fn drm_plane_helper_disable_primary(
    plane: *mut DrmPlane,
    _ctx: *mut DrmModesetAcquireCtx,
) -> i32 {
    // SAFETY: `plane` is non-null, properly aligned, and points to an initialized `DrmPlane`;
    // reading `(*plane).dev` accesses an initialized field, and `drm_drv_uses_atomic_modeset` checks driver flags.
    unsafe {
        let dev = (*plane).dev;
        warn_on(drm_drv_uses_atomic_modeset(dev));
        -EINVAL
    }
}

/// Destroys a plane allocated by the legacy helpers.
// upstream: drm_plane_helper.c drm_plane_helper_destroy()
#[unsafe(no_mangle)]
/// # Safety
/// - `plane` must be non-null, properly aligned, and point to a heap-allocated `DrmPlane` previously initialized
///   by legacy helpers.
/// - The caller transfers exclusive ownership of `plane` to this function; `plane` must not be accessed
///   or freed again after this call.
/// - Any CRTC or DRM configuration referencing `plane` must have been detached before destruction.
pub unsafe extern "C" fn drm_plane_helper_destroy(plane: *mut DrmPlane) {
    // SAFETY: `plane` is non-null, properly aligned, and points to a heap-allocated `DrmPlane` whose ownership
    // is transferred by the caller. `drm_plane_cleanup` tears down registered DRM plane resources,
    // and `kfree` frees the underlying heap allocation.
    unsafe {
        crate::drm_plane_upstream::drm_plane_cleanup(plane);
        drm_kapi::memory::kfree(plane);
    }
}

#[cfg(test)]
mod tests {
    use core::mem::zeroed;

    use super::*;
    use crate::test_support::TestDrm;

    #[test]
    fn primary_plane_cannot_be_disabled_through_the_legacy_helper() {
        let mut drm = TestDrm::new(0);
        // SAFETY: DrmPlane is an FFI struct composed of POD/raw pointers where all-zero bit pattern represents valid empty/zero-initialized state.
        let mut plane: DrmPlane = unsafe { zeroed() };
        plane.dev = drm.ptr();
        assert_eq!(
            // SAFETY: 'plane' is a locally stack-allocated DrmPlane initialized with a valid dev pointer, and null ctx is accepted.
            unsafe { drm_plane_helper_disable_primary(&mut plane, core::ptr::null_mut()) },
            -EINVAL
        );
    }
}
