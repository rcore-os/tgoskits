// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_modeset_helper.c`.
//!
//! Source-faithful transcription of Linux 7.2.3. Plane, CRTC, probe, client
//! and atomic-helper callees are declared as C ABI references and resolved by
//! sibling modules in this shared crate.

use core::{
    ffi::{c_char, c_void},
    mem::{offset_of, size_of},
};

use drm_kapi::{
    assert::warn_on,
    config::{EINVAL, is_err, ptr_err},
    list::{list_move_tail, list_splice},
    locks::{spin_lock_irq_raw, spin_unlock_irq_raw},
    memory::kfree,
    print::{DrmLogLevel, drm_log_at},
    types::ListHead,
};

use crate::{
    drm_atomic_types::DrmAtomicCommit,
    drm_connector_types::DrmConnector,
    drm_crtc_types::{DrmCrtc, DrmCrtcFuncs},
    drm_device_types::DrmDevice,
    drm_fourcc_types::DrmFormatInfo,
    drm_framebuffer_types::DrmFramebuffer,
    drm_plane_types::{DrmPlane, DrmPlaneFuncs},
    uapi_drm_consts::{
        DRM_FORMAT_ARGB8888, DRM_FORMAT_XRGB8888, DRM_MODE_CONNECTOR_DSI, DRM_MODE_CONNECTOR_EDP,
        DRM_MODE_CONNECTOR_LVDS,
    },
    uapi_drm_mode_types::DrmModeFbCmd2,
};

// upstream: include/drm/drm_plane.h enum drm_plane_type: DRM_PLANE_TYPE_PRIMARY
const DRM_PLANE_TYPE_PRIMARY: u32 = 1;

// Plane, CRTC, probe, client and atomic-helper callees are implemented by
// sibling DRM modules.
unsafe extern "C" {
    fn drm_plane_helper_update_primary(
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
        ctx: *mut crate::drm_modeset_lock_types::DrmModesetAcquireCtx,
    ) -> i32;
    fn drm_plane_helper_disable_primary(
        plane: *mut DrmPlane,
        ctx: *mut crate::drm_modeset_lock_types::DrmModesetAcquireCtx,
    ) -> i32;
    fn drm_plane_helper_destroy(plane: *mut DrmPlane);
    fn __drm_universal_plane_alloc(
        dev: *mut DrmDevice,
        size: usize,
        offset: usize,
        possible_crtcs: u32,
        funcs: *const DrmPlaneFuncs,
        formats: *const u32,
        format_count: u32,
        format_modifiers: *const u64,
        plane_type: u32,
        name: *const c_char,
        ...
    ) -> *mut c_void;
    fn drm_plane_cleanup(plane: *mut DrmPlane);
    fn drm_crtc_init_with_planes(
        dev: *mut DrmDevice,
        crtc: *mut DrmCrtc,
        primary: *mut DrmPlane,
        cursor: *mut DrmPlane,
        funcs: *const DrmCrtcFuncs,
        name: *const c_char,
        ...
    ) -> i32;
    fn drm_kms_helper_poll_disable(dev: *mut DrmDevice);
    fn drm_kms_helper_poll_enable(dev: *mut DrmDevice);
    fn drm_client_dev_suspend(dev: *mut DrmDevice);
    fn drm_client_dev_resume(dev: *mut DrmDevice);
    fn drm_atomic_helper_suspend(dev: *mut DrmDevice) -> *mut DrmAtomicCommit;
    fn drm_atomic_helper_resume(dev: *mut DrmDevice, state: *mut DrmAtomicCommit) -> i32;
}

// upstream: drm_modeset_helper.c safe_modeset_formats[]
static SAFE_MODESET_FORMATS: [u32; 2] = [DRM_FORMAT_XRGB8888, DRM_FORMAT_ARGB8888];

// upstream: drm_modeset_helper.c primary_plane_funcs (DRM_PLANE_NON_ATOMIC_FUNCS)
static PRIMARY_PLANE_FUNCS: DrmPlaneFuncs = DrmPlaneFuncs {
    update_plane: Some(drm_plane_helper_update_primary),
    disable_plane: Some(drm_plane_helper_disable_primary),
    destroy: Some(drm_plane_helper_destroy),
    reset: None,
    set_property: None,
    atomic_duplicate_state: None,
    atomic_destroy_state: None,
    atomic_set_property: None,
    atomic_get_property: None,
    late_register: None,
    early_unregister: None,
    atomic_print_state: None,
    format_mod_supported: None,
    format_mod_supported_async: None,
};

// upstream: drm_modeset_helper.c drm_helper_move_panel_connectors_to_head()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must be non-null, properly aligned, and point to a valid, initialized `DrmDevice`.
/// - `(*dev).mode_config.connector_list` must be an initialized circular doubly-linked list where
///   every element is embedded in a live `DrmConnector` at offset `offset_of!(DrmConnector, head)`.
/// - `(*dev).mode_config.connector_list_lock` must be an initialized spinlock that is not already held
///   by the current CPU in hardirq context.
/// - The caller must not hold mutable aliases to connectors in `connector_list` that would conflict
///   with list relinking under `connector_list_lock`.
pub unsafe extern "C" fn drm_helper_move_panel_connectors_to_head(dev: *mut DrmDevice) {
    let mut panel_list = ListHead {
        next: core::ptr::null_mut(),
        prev: core::ptr::null_mut(),
    };
    let pl = core::ptr::addr_of_mut!(panel_list);
    // SAFETY: `pl` points to a local, correctly aligned, unaliased `ListHead` on the stack;
    // assigning its `next` and `prev` pointers to itself forms a valid empty circular list head.
    unsafe {
        (*pl).next = pl;
        (*pl).prev = pl;
    }

    // SAFETY: `dev` is guaranteed non-null, aligned, and valid by the function safety contract;
    // computing the address of `mode_config.connector_list_lock` performs no dereference beyond the struct field.
    let lock = unsafe { core::ptr::addr_of_mut!((*dev).mode_config.connector_list_lock) };
    // SAFETY: `lock` points to the initialized `connector_list_lock` in `dev.mode_config`;
    // acquiring it disables local IRQs and provides exclusive access to `dev.mode_config.connector_list`.
    unsafe { spin_lock_irq_raw(lock) };

    // SAFETY: `dev` is valid and points to initialized memory; computing the address of `connector_list`
    // yields a valid pointer to the device's connector list head.
    let head = unsafe { core::ptr::addr_of_mut!((*dev).mode_config.connector_list) };
    // SAFETY: `head` is valid and initialized, and `connector_list_lock` is held; reading `(*head).next`
    // safely retrieves the first node of the list.
    let mut link = unsafe { (*head).next };
    while link != head {
        // list_for_each_entry_safe(connector, tmp, &connector_list, head)
        // SAFETY: `link` points to a valid `ListHead` in the list while `connector_list_lock` is held;
        // caching `(*link).next` upfront ensures safe forward traversal even if `link` is relocated.
        let next = unsafe { (*link).next };
        // SAFETY: All nodes in `connector_list` are embedded in live `DrmConnector` instances at offset
        // `offset_of!(DrmConnector, head)`; subtracting the offset computes the valid outer struct pointer.
        let connector =
            unsafe { link.cast::<u8>().sub(offset_of!(DrmConnector, head)) as *mut DrmConnector };
        // SAFETY: `connector` points to a live, initialized `DrmConnector`; reading `connector_type`
        // is protected against concurrent modification by `connector_list_lock`.
        let ct = unsafe { (*connector).connector_type } as u32;
        if ct == DRM_MODE_CONNECTOR_LVDS
            || ct == DRM_MODE_CONNECTOR_EDP
            || ct == DRM_MODE_CONNECTOR_DSI
        {
            // SAFETY: `(*connector).head` is a valid node in `connector_list` and `pl` is the valid local
            // temporary list head; `connector_list_lock` guarantees exclusive list mutation.
            unsafe { list_move_tail(core::ptr::addr_of_mut!((*connector).head), pl) };
        }
        link = next;
    }

    // SAFETY: `pl` is the local temporary list and `head` is the device's connector list head;
    // `connector_list_lock` is held, ensuring atomic insertion of all relocated panel connectors at `head`.
    unsafe { list_splice(pl, head) };
    // SAFETY: `lock` was acquired with `spin_lock_irq_raw` above and is now released, restoring local IRQ state.
    unsafe { spin_unlock_irq_raw(lock) };
}

// upstream: drm_modeset_helper.c drm_helper_mode_fill_fb_struct()
#[unsafe(no_mangle)]
/// # Safety
/// - `fb` must be non-null, properly aligned, and point to writable memory with capacity for `DrmFramebuffer`.
///   The memory must be already initialized or zeroed to satisfy Rust reference validity rules for `&mut DrmFramebuffer`.
/// - `mode_cmd` must be non-null, properly aligned, point to an initialized `DrmModeFbCmd2`, and not be mutated concurrently.
/// - `dev` and `info` are stored directly into `fb`; if non-null, they must remain valid for the lifetime of `fb`.
/// - No concurrent access to `fb` may occur during this call.
pub unsafe extern "C" fn drm_helper_mode_fill_fb_struct(
    dev: *mut DrmDevice,
    fb: *mut DrmFramebuffer,
    info: *const DrmFormatInfo,
    mode_cmd: *const DrmModeFbCmd2,
) {
    // SAFETY: `fb` is non-null, properly aligned, and points to unaliased writable memory for `DrmFramebuffer`
    // with no concurrent access for the duration of the mutable borrow.
    let fb = unsafe { &mut *fb };
    // SAFETY: `mode_cmd` is non-null, properly aligned, points to an initialized `DrmModeFbCmd2`, and is not
    // concurrently modified.
    let mode_cmd = unsafe { &*mode_cmd };

    fb.dev = dev;
    fb.format = info as *mut DrmFormatInfo;
    fb.width = mode_cmd.width;
    fb.height = mode_cmd.height;
    for i in 0..4 {
        fb.pitches[i] = mode_cmd.pitches[i];
        fb.offsets[i] = mode_cmd.offsets[i];
    }
    fb.modifier = mode_cmd.modifier[0];
    fb.flags = mode_cmd.flags as i32;
}

// upstream: drm_modeset_helper.c drm_crtc_init()
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must be non-null, properly aligned, and point to an initialized `DrmDevice`.
/// - `crtc` must be non-null, properly aligned, point to unaliased writable memory allocated for a `DrmCrtc`,
///   and not be accessed concurrently during initialization.
/// - `funcs` must be non-null, properly aligned, and point to a valid `DrmCrtcFuncs` vtable that remains live
///   for the lifetime of `crtc`.
pub unsafe extern "C" fn drm_crtc_init(
    dev: *mut DrmDevice,
    crtc: *mut DrmCrtc,
    funcs: *const DrmCrtcFuncs,
) -> i32 {
    // SAFETY: `dev` is a valid `DrmDevice`; `PRIMARY_PLANE_FUNCS` and `SAFE_MODESET_FORMATS` are static,
    // properly aligned data structures valid for read access; null name and null format modifiers are accepted.
    let primary = unsafe {
        __drm_universal_plane_alloc(
            dev,
            size_of::<DrmPlane>(),
            0,
            0,
            &PRIMARY_PLANE_FUNCS,
            SAFE_MODESET_FORMATS.as_ptr(),
            SAFE_MODESET_FORMATS.len() as u32,
            core::ptr::null(),
            DRM_PLANE_TYPE_PRIMARY,
            core::ptr::null(),
        )
    } as *mut DrmPlane;
    if is_err(primary) {
        return ptr_err(primary);
    }

    // SAFETY: `primary` was successfully allocated above (verified by `!is_err`), returning a uniquely owned,
    // aligned, initialized `DrmPlane` pointer.
    unsafe { (*primary).format_default = true };

    // SAFETY: `dev`, `crtc`, and `funcs` are valid per caller contract; `primary` is the newly allocated plane;
    // null cursor plane and null name are accepted by `drm_crtc_init_with_planes`.
    let ret = unsafe {
        drm_crtc_init_with_planes(
            dev,
            crtc,
            primary,
            core::ptr::null_mut(),
            funcs,
            core::ptr::null(),
        )
    };
    if ret != 0 {
        // SAFETY: Initialization failed, so ownership of `primary` was not retained by `crtc`;
        // `primary` is cleaned up via `drm_plane_cleanup` and its allocated memory is freed via `kfree`.
        unsafe {
            drm_plane_cleanup(primary);
            kfree(primary);
        }
        return ret;
    }

    0
}

// upstream: drm_modeset_helper.c drm_mode_config_helper_suspend()
#[unsafe(no_mangle)]
/// # Safety
/// - If `dev` is null, the function is a no-op and returns 0.
/// - If non-null, `dev` must be properly aligned and point to an initialized `DrmDevice`.
/// - The caller must hold the appropriate top-level device locks to prevent concurrent modeset
///   or suspend/resume operations on `dev`.
pub unsafe extern "C" fn drm_mode_config_helper_suspend(dev: *mut DrmDevice) -> i32 {
    if dev.is_null() {
        return 0;
    }

    // SAFETY: `dev` is non-null (checked above) and points to an initialized `DrmDevice`;
    // reading `mode_config.poll_enabled` is valid and aligned.
    if unsafe { (*dev).mode_config.poll_enabled } {
        // SAFETY: `dev` is a valid `DrmDevice`; disables KMS output polling work before suspend.
        unsafe { drm_kms_helper_poll_disable(dev) };
    }

    // SAFETY: `dev` is a valid, live `DrmDevice`; suspends in-kernel DRM clients (such as fbdev).
    unsafe { drm_client_dev_suspend(dev) };
    // SAFETY: `dev` is a valid `DrmDevice`; `drm_atomic_helper_suspend` allocates and returns
    // an atomic commit state duplicating current display configuration, or an err_ptr on failure.
    let state = unsafe { drm_atomic_helper_suspend(dev) };
    if is_err(state) {
        // SAFETY: Rollback on suspend failure: resumes in-kernel DRM clients on the valid `dev`.
        unsafe { drm_client_dev_resume(dev) };
        // SAFETY: `dev` is valid and non-null; re-checks `poll_enabled` to determine whether to restore polling.
        if unsafe { (*dev).mode_config.poll_enabled } {
            // SAFETY: Re-enables KMS output polling work on rollback.
            unsafe { drm_kms_helper_poll_enable(dev) };
        }
        return ptr_err(state);
    }

    // SAFETY: `dev` is valid and non-null; stores the successfully allocated `state` into `suspend_state`
    // for retrieval during resume.
    unsafe { (*dev).mode_config.suspend_state = state };
    0
}

// upstream: drm_modeset_helper.c drm_mode_config_helper_resume()
#[unsafe(no_mangle)]
/// # Safety
/// - If `dev` is null, the function is a no-op and returns 0.
/// - If non-null, `dev` must be properly aligned and point to an initialized `DrmDevice`.
/// - `(*dev).mode_config.suspend_state` should contain the state pointer returned by a previous
///   successful call to `drm_mode_config_helper_suspend` (if null, the function warns and returns `-EINVAL`).
/// - The caller must ensure no concurrent modeset or suspend/resume operations are executing on `dev`.
pub unsafe extern "C" fn drm_mode_config_helper_resume(dev: *mut DrmDevice) -> i32 {
    if dev.is_null() {
        return 0;
    }

    // SAFETY: `dev` is non-null (checked above) and points to an initialized `DrmDevice`;
    // reading `suspend_state` checks whether a prior suspend state was recorded.
    if warn_on(unsafe { (*dev).mode_config.suspend_state.is_null() }) {
        return -EINVAL;
    }

    // SAFETY: `dev` is valid and non-null; reads the recorded non-null `suspend_state` pointer.
    let state = unsafe { (*dev).mode_config.suspend_state };
    // SAFETY: `dev` and `state` are valid pointers; `drm_atomic_helper_resume` restores the saved
    // atomic configuration and consumes/frees `state`.
    let ret = unsafe { drm_atomic_helper_resume(dev, state) };
    if ret != 0 {
        // DRM_ERROR("Failed to resume (%d)\n", ret)
        drm_log_at(
            DrmLogLevel::Error,
            "drm",
            file!(),
            line!(),
            &alloc::format!("*ERROR* Failed to resume ({})", ret),
        );
    }

    // SAFETY: `dev` is valid and non-null; clears `suspend_state` to prevent accidental double-resume.
    unsafe { (*dev).mode_config.suspend_state = core::ptr::null_mut() };
    // SAFETY: `dev` is a valid, live `DrmDevice`; resumes in-kernel DRM clients.
    unsafe { drm_client_dev_resume(dev) };
    // SAFETY: `dev` is valid and non-null; checks whether KMS output polling was previously enabled.
    if unsafe { (*dev).mode_config.poll_enabled } {
        // SAFETY: Re-enables KMS output polling work after resume.
        unsafe { drm_kms_helper_poll_enable(dev) };
    }
    ret
}
