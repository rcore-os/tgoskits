// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_atomic_state_helper.c`.
//!
//! State reset, duplicate, and destroy helpers for CRTCs, planes, connectors,
//! and private objects. Bridge and writeback state is outside this component's
//! supported scope and is rejected by atomic validation.

use core::mem::size_of;

use drm_kapi::{assert::warn_on, config::GFP_KERNEL, types::DmaFence};

use crate::{
    drm_atomic_types::{DrmAtomicCommit, DrmCrtcCommit, DrmPrivateObj, DrmPrivateState},
    drm_connector_types::{DrmConnector, DrmConnectorState},
    drm_connector_upstream::{drm_connector_get, drm_connector_put},
    drm_crtc_types::{DrmCrtc, DrmCrtcState},
    drm_framebuffer_types::DrmFramebuffer,
    drm_mode_object_upstream::drm_object_property_get_default_value,
    drm_plane_types::{DrmPlane, DrmPlaneState},
    drm_property_types::DrmPropertyBlob,
    drm_vblank_upstream::{drm_crtc_vblank_reset, drm_dev_has_vblank},
    uapi_drm_consts::DRM_MODE_ROTATE_0,
};

pub const DRM_BLEND_ALPHA_OPAQUE: u16 = 0xffff;
pub const DRM_MODE_BLEND_PREMULTI: u16 = 0;

unsafe extern "C" {
    fn drm_property_blob_get(blob: *mut DrmPropertyBlob) -> *mut DrmPropertyBlob;
    fn drm_property_blob_put(blob: *mut DrmPropertyBlob);
    fn drm_mode_object_get(obj: *mut crate::drm_mode_object_types::DrmModeObject);
    fn drm_mode_object_put(obj: *mut crate::drm_mode_object_types::DrmModeObject);
    fn dma_fence_put(fence: *mut DmaFence);
    fn __drm_crtc_commit_free(kref: *mut drm_kapi::types::Kref);
    fn drm_atomic_get_old_connector_state(
        state: *const DrmAtomicCommit,
        connector: *const DrmConnector,
    ) -> *mut DrmConnectorState;
    fn drm_atomic_get_new_connector_state(
        state: *const DrmAtomicCommit,
        connector: *const DrmConnector,
    ) -> *mut DrmConnectorState;
    fn drm_atomic_get_new_crtc_state(
        state: *const DrmAtomicCommit,
        crtc: *const DrmCrtc,
    ) -> *mut DrmCrtcState;
}

/// # Safety
///
/// If non-null, `fb` must point to a live `DrmFramebuffer`.
#[inline]
unsafe fn drm_framebuffer_get(fb: *mut DrmFramebuffer) {
    if !fb.is_null() {
        // SAFETY: `fb` is non-null and points to a live `DrmFramebuffer` guaranteed by caller.
        unsafe {
            drm_mode_object_get(core::ptr::addr_of_mut!((*fb).base));
        }
    }
}

/// # Safety
///
/// If non-null, `fb` must point to a live `DrmFramebuffer`.
#[inline]
unsafe fn drm_framebuffer_put(fb: *mut DrmFramebuffer) {
    if !fb.is_null() {
        // SAFETY: `fb` is non-null and points to a live `DrmFramebuffer` guaranteed by caller.
        unsafe {
            drm_mode_object_put(core::ptr::addr_of_mut!((*fb).base));
        }
    }
}

/// # Safety
///
/// If non-null, `commit` must point to a live `DrmCrtcCommit`.
#[inline]
pub unsafe fn drm_crtc_commit_put(commit: *mut DrmCrtcCommit) {
    if !commit.is_null() {
        // SAFETY: `commit` is non-null and points to a live `DrmCrtcCommit` guaranteed by caller.
        unsafe {
            drm_kapi::memory::kref_put(
                core::ptr::addr_of_mut!((*commit).r#ref),
                __drm_crtc_commit_free,
            );
        }
    }
}

/// # Safety
///
/// `state` must point to a valid `DrmCrtcState`.
#[inline]
pub unsafe fn drm_atomic_crtc_effectively_active(state: *const DrmCrtcState) -> bool {
    // SAFETY: caller guarantees `state` points to a valid, initialized `DrmCrtcState`.
    unsafe { (*state).active || (*state).self_refresh_active }
}

// upstream: drm_atomic_state_helper.c __drm_atomic_helper_crtc_state_reset()
/// # Safety
///
/// `crtc_state` must point to valid, allocated `DrmCrtcState` memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_crtc_state_reset(
    crtc_state: *mut DrmCrtcState,
    crtc: *mut DrmCrtc,
) {
    // SAFETY: caller guarantees `crtc_state` points to valid, allocated `DrmCrtcState` memory.
    unsafe {
        (*crtc_state).crtc = crtc;
        (*crtc_state).background_color = 0xffff_0000_0000_0000; // DRM_ARGB64_PREP(0xffff, 0, 0, 0)
    }
}

// upstream: drm_atomic_state_helper.c __drm_atomic_helper_crtc_reset()
/// # Safety
///
/// `crtc` must point to a valid `DrmCrtc`, and `crtc_state` must be null or point to a valid `DrmCrtcState`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_crtc_reset(
    crtc: *mut DrmCrtc,
    crtc_state: *mut DrmCrtcState,
) {
    // SAFETY: caller guarantees `crtc` points to a live `DrmCrtc`, and `crtc_state` if non-null is valid.
    unsafe {
        if !crtc_state.is_null() {
            __drm_atomic_helper_crtc_state_reset(crtc_state, crtc);
        }
        if drm_dev_has_vblank((*crtc).dev) {
            drm_crtc_vblank_reset(crtc);
        }
        (*crtc).state = crtc_state;
    }
}

// upstream: drm_atomic_state_helper.c drm_atomic_helper_crtc_reset()
/// # Safety
///
/// `crtc` must point to a valid `DrmCrtc` with initialized `funcs`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_helper_crtc_reset(crtc: *mut DrmCrtc) {
    // SAFETY: caller guarantees `crtc` points to a valid `DrmCrtc`; old state is destroyed before assigning fresh state.
    unsafe {
        let crtc_state =
            drm_kapi::memory::kzalloc(size_of::<DrmCrtcState>(), GFP_KERNEL) as *mut DrmCrtcState;
        if !(*crtc).state.is_null()
            && let Some(destroy) = (*(*crtc).funcs).atomic_destroy_state
        {
            destroy(crtc, (*crtc).state);
        }
        __drm_atomic_helper_crtc_reset(crtc, crtc_state);
    }
}

// upstream: drm_atomic_state_helper.c __drm_atomic_helper_crtc_duplicate_state()
/// # Safety
///
/// `crtc` must point to a valid `DrmCrtc` with non-null `state`, and `state` must point to valid `DrmCrtcState` storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_crtc_duplicate_state(
    crtc: *mut DrmCrtc,
    state: *mut DrmCrtcState,
) {
    // SAFETY: caller guarantees `(*crtc).state` and `state` are valid, non-overlapping `DrmCrtcState` pointers.
    unsafe {
        core::ptr::copy_nonoverlapping((*crtc).state, state, 1);

        if !(*state).mode_blob.is_null() {
            drm_property_blob_get((*state).mode_blob);
        }
        if !(*state).degamma_lut.is_null() {
            drm_property_blob_get((*state).degamma_lut);
        }
        if !(*state).ctm.is_null() {
            drm_property_blob_get((*state).ctm);
        }
        if !(*state).gamma_lut.is_null() {
            drm_property_blob_get((*state).gamma_lut);
        }
        (*state).set_mode_changed(false);
        (*state).set_active_changed(false);
        (*state).set_planes_changed(false);
        (*state).set_connectors_changed(false);
        (*state).set_color_mgmt_changed(false);
        (*state).set_zpos_changed(false);
        (*state).commit = core::ptr::null_mut();
        (*state).event = core::ptr::null_mut();
        (*state).async_flip = false;

        // Self refresh should be canceled when a new update is available
        (*state).active = drm_atomic_crtc_effectively_active(state);
        (*state).self_refresh_active = false;
    }
}

// upstream: drm_atomic_state_helper.c drm_atomic_helper_crtc_duplicate_state()
/// # Safety
///
/// `crtc` must point to a valid `DrmCrtc` with non-null `state`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_helper_crtc_duplicate_state(
    crtc: *mut DrmCrtc,
) -> *mut DrmCrtcState {
    // SAFETY: caller guarantees `crtc` points to a valid `DrmCrtc`; its state is copied to newly allocated memory.
    unsafe {
        if warn_on((*crtc).state.is_null()) {
            return core::ptr::null_mut();
        }
        let state =
            drm_kapi::memory::kmalloc(size_of::<DrmCrtcState>(), GFP_KERNEL) as *mut DrmCrtcState;
        if !state.is_null() {
            __drm_atomic_helper_crtc_duplicate_state(crtc, state);
        }
        state
    }
}

// upstream: drm_atomic_state_helper.c __drm_atomic_helper_crtc_destroy_state()
/// # Safety
///
/// `state` must point to a valid `DrmCrtcState`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_crtc_destroy_state(state: *mut DrmCrtcState) {
    // SAFETY: caller guarantees `state` points to a valid `DrmCrtcState` whose resources are released.
    unsafe {
        if !(*state).commit.is_null() {
            if !(*state).event.is_null() && (*(*state).commit).abort_completion {
                drm_crtc_commit_put((*state).commit);
            }

            drm_kapi::memory::kfree((*(*state).commit).event);
            (*(*state).commit).event = core::ptr::null_mut();

            drm_crtc_commit_put((*state).commit);
        }

        drm_property_blob_put((*state).mode_blob);
        drm_property_blob_put((*state).degamma_lut);
        drm_property_blob_put((*state).ctm);
        drm_property_blob_put((*state).gamma_lut);
    }
}

// upstream: drm_atomic_state_helper.c drm_atomic_helper_crtc_destroy_state()
/// # Safety
///
/// `state` must point to a valid `DrmCrtcState` previously allocated with kmalloc/kzalloc.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_helper_crtc_destroy_state(
    _crtc: *mut DrmCrtc,
    state: *mut DrmCrtcState,
) {
    // SAFETY: `state` is cleaned up via `__drm_atomic_helper_crtc_destroy_state` and then freed.
    unsafe {
        __drm_atomic_helper_crtc_destroy_state(state);
        drm_kapi::memory::kfree(state);
    }
}

// upstream: drm_atomic_state_helper.c __drm_atomic_helper_plane_state_reset()
/// # Safety
///
/// `plane_state` must point to valid `DrmPlaneState` storage, and `plane` must point to a valid `DrmPlane`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_plane_state_reset(
    plane_state: *mut DrmPlaneState,
    plane: *mut DrmPlane,
) {
    // SAFETY: caller guarantees `plane_state` and `plane` point to valid DRM structures.
    unsafe {
        let mut val: u64 = 0;

        (*plane_state).plane = plane;
        (*plane_state).rotation = DRM_MODE_ROTATE_0;

        (*plane_state).alpha = DRM_BLEND_ALPHA_OPAQUE;
        (*plane_state).pixel_blend_mode = DRM_MODE_BLEND_PREMULTI;

        if !(*plane).color_encoding_property.is_null()
            && drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*plane).base),
                (*plane).color_encoding_property,
                &mut val,
            ) == 0
        {
            (*plane_state).color_encoding = val as u32;
        }

        if !(*plane).color_range_property.is_null()
            && drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*plane).base),
                (*plane).color_range_property,
                &mut val,
            ) == 0
        {
            (*plane_state).color_range = val as u32;
        }

        if !(*plane).color_pipeline_property.is_null() {
            // default is always NULL, i.e., bypass
            (*plane_state).color_pipeline = core::ptr::null_mut();
        }

        if !(*plane).zpos_property.is_null()
            && drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*plane).base),
                (*plane).zpos_property,
                &mut val,
            ) == 0
        {
            (*plane_state).zpos = val as u32;
            (*plane_state).normalized_zpos = val as u32;
        }

        if !(*plane).hotspot_x_property.is_null()
            && drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*plane).base),
                (*plane).hotspot_x_property,
                &mut val,
            ) == 0
        {
            (*plane_state).hotspot_x = val as i32;
        }

        if !(*plane).hotspot_y_property.is_null()
            && drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*plane).base),
                (*plane).hotspot_y_property,
                &mut val,
            ) == 0
        {
            (*plane_state).hotspot_y = val as i32;
        }
    }
}

// upstream: drm_atomic_state_helper.c __drm_atomic_helper_plane_reset()
/// # Safety
///
/// `plane` must point to a valid `DrmPlane`, and `plane_state` must be null or point to valid `DrmPlaneState` storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_plane_reset(
    plane: *mut DrmPlane,
    plane_state: *mut DrmPlaneState,
) {
    // SAFETY: caller guarantees `plane` is valid; `plane_state` if non-null is initialized.
    unsafe {
        if !plane_state.is_null() {
            __drm_atomic_helper_plane_state_reset(plane_state, plane);
        }
        (*plane).state = plane_state;
    }
}

// upstream: drm_atomic_state_helper.c drm_atomic_helper_plane_reset()
/// # Safety
///
/// `plane` must point to a valid `DrmPlane`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_helper_plane_reset(plane: *mut DrmPlane) {
    // SAFETY: caller guarantees `plane` is a valid pointer; prior state is destroyed before allocating fresh state.
    unsafe {
        if !(*plane).state.is_null() {
            __drm_atomic_helper_plane_destroy_state((*plane).state);
        }

        drm_kapi::memory::kfree((*plane).state);
        let plane_state =
            drm_kapi::memory::kzalloc(size_of::<DrmPlaneState>(), GFP_KERNEL) as *mut DrmPlaneState;
        (*plane).state = plane_state;
        if !plane_state.is_null() {
            __drm_atomic_helper_plane_reset(plane, plane_state);
        }
    }
}

// upstream: drm_atomic_state_helper.c __drm_atomic_helper_plane_duplicate_state()
/// # Safety
///
/// `plane` must point to a valid `DrmPlane` with non-null `state`, and `state` must point to valid `DrmPlaneState` storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_plane_duplicate_state(
    plane: *mut DrmPlane,
    state: *mut DrmPlaneState,
) {
    // SAFETY: caller guarantees `(*plane).state` and `state` are valid, non-overlapping `DrmPlaneState` pointers.
    unsafe {
        core::ptr::copy_nonoverlapping((*plane).state, state, 1);

        if !(*state).fb.is_null() {
            drm_framebuffer_get((*state).fb);
        }

        (*state).fence = core::ptr::null_mut();
        (*state).commit = core::ptr::null_mut();
        (*state).fb_damage_clips = core::ptr::null_mut();
        (*state).set_color_mgmt_changed(false);
    }
}

// upstream: drm_atomic_state_helper.c drm_atomic_helper_plane_duplicate_state()
/// # Safety
///
/// `plane` must point to a valid `DrmPlane` with non-null `state`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_helper_plane_duplicate_state(
    plane: *mut DrmPlane,
) -> *mut DrmPlaneState {
    // SAFETY: caller guarantees `plane` points to a valid `DrmPlane`; its state is copied to newly allocated memory.
    unsafe {
        if warn_on((*plane).state.is_null()) {
            return core::ptr::null_mut();
        }

        let state =
            drm_kapi::memory::kmalloc(size_of::<DrmPlaneState>(), GFP_KERNEL) as *mut DrmPlaneState;
        if !state.is_null() {
            __drm_atomic_helper_plane_duplicate_state(plane, state);
        }
        state
    }
}

// upstream: drm_atomic_state_helper.c __drm_atomic_helper_plane_destroy_state()
/// # Safety
///
/// `state` must point to a valid `DrmPlaneState`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_plane_destroy_state(state: *mut DrmPlaneState) {
    // SAFETY: caller guarantees `state` points to a valid `DrmPlaneState` whose resources are released.
    unsafe {
        if !(*state).fb.is_null() {
            drm_framebuffer_put((*state).fb);
        }

        if !(*state).fence.is_null() {
            dma_fence_put((*state).fence);
        }

        if !(*state).commit.is_null() {
            drm_crtc_commit_put((*state).commit);
        }

        drm_property_blob_put((*state).fb_damage_clips);
    }
}

// upstream: drm_atomic_state_helper.c drm_atomic_helper_plane_destroy_state()
/// # Safety
///
/// `state` must point to a valid `DrmPlaneState` previously allocated with kmalloc/kzalloc.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_helper_plane_destroy_state(
    _plane: *mut DrmPlane,
    state: *mut DrmPlaneState,
) {
    // SAFETY: `state` is cleaned up via `__drm_atomic_helper_plane_destroy_state` and then freed.
    unsafe {
        __drm_atomic_helper_plane_destroy_state(state);
        drm_kapi::memory::kfree(state);
    }
}

// upstream: drm_atomic_state_helper.c __drm_atomic_helper_connector_state_reset()
/// # Safety
///
/// `conn_state` must point to valid `DrmConnectorState` storage, and `connector` must point to a valid `DrmConnector`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_connector_state_reset(
    conn_state: *mut DrmConnectorState,
    connector: *mut DrmConnector,
) {
    // SAFETY: caller guarantees `conn_state` and `connector` point to valid DRM structures.
    unsafe {
        (*conn_state).connector = connector;
    }
}

// upstream: drm_atomic_state_helper.c __drm_atomic_helper_connector_reset()
/// # Safety
///
/// `connector` must point to a valid `DrmConnector`, and `conn_state` must be null or point to valid `DrmConnectorState` storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_connector_reset(
    connector: *mut DrmConnector,
    conn_state: *mut DrmConnectorState,
) {
    // SAFETY: caller guarantees `connector` is valid; `conn_state` if non-null is initialized.
    unsafe {
        if !conn_state.is_null() {
            __drm_atomic_helper_connector_state_reset(conn_state, connector);
        }
        (*connector).state = conn_state;
    }
}

// upstream: drm_atomic_state_helper.c drm_atomic_helper_connector_reset()
/// # Safety
///
/// `connector` must point to a valid `DrmConnector`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_helper_connector_reset(connector: *mut DrmConnector) {
    // SAFETY: caller guarantees `connector` is a valid pointer; prior state is destroyed before allocating fresh state.
    unsafe {
        let conn_state = drm_kapi::memory::kzalloc(size_of::<DrmConnectorState>(), GFP_KERNEL)
            as *mut DrmConnectorState;

        if !(*connector).state.is_null() {
            __drm_atomic_helper_connector_destroy_state((*connector).state);
        }

        drm_kapi::memory::kfree((*connector).state);
        __drm_atomic_helper_connector_reset(connector, conn_state);
    }
}

// upstream: drm_atomic_state_helper.c drm_atomic_helper_connector_tv_margins_reset()
/// # Safety
///
/// `connector` must point to a valid `DrmConnector` with non-null `state`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_helper_connector_tv_margins_reset(
    connector: *mut DrmConnector,
) {
    // SAFETY: caller guarantees `connector` and `(*connector).state` are valid pointers.
    unsafe {
        let cmdline = core::ptr::addr_of_mut!((*connector).cmdline_mode);
        let state = (*connector).state;

        (*state).tv.margins.left = (*cmdline).tv_margins.left;
        (*state).tv.margins.right = (*cmdline).tv_margins.right;
        (*state).tv.margins.top = (*cmdline).tv_margins.top;
        (*state).tv.margins.bottom = (*cmdline).tv_margins.bottom;
    }
}

// upstream: drm_atomic_state_helper.c drm_atomic_helper_connector_tv_reset()
/// # Safety
///
/// `connector` must point to a valid `DrmConnector` with non-null `dev` and `state`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_helper_connector_tv_reset(connector: *mut DrmConnector) {
    // SAFETY: caller guarantees `connector`, its device, and its state are valid pointers.
    unsafe {
        let dev = (*connector).dev;
        let cmdline = core::ptr::addr_of_mut!((*connector).cmdline_mode);
        let state = (*connector).state;
        let mut val: u64 = 0;

        let mut prop = (*dev).mode_config.tv_mode_property;
        if !prop.is_null()
            && drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*connector).base),
                prop,
                &mut val,
            ) == 0
        {
            (*state).tv.mode = val as u32;
        }

        if (*cmdline).tv_mode_specified {
            (*state).tv.mode = (*cmdline).tv_mode;
        }

        prop = (*dev).mode_config.tv_select_subconnector_property;
        if !prop.is_null()
            && drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*connector).base),
                prop,
                &mut val,
            ) == 0
        {
            (*state).tv.select_subconnector = val as u32;
        }

        prop = (*dev).mode_config.tv_subconnector_property;
        if !prop.is_null()
            && drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*connector).base),
                prop,
                &mut val,
            ) == 0
        {
            (*state).tv.subconnector = val as u32;
        }

        prop = (*dev).mode_config.tv_brightness_property;
        if !prop.is_null()
            && drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*connector).base),
                prop,
                &mut val,
            ) == 0
        {
            (*state).tv.brightness = val as u32;
        }

        prop = (*dev).mode_config.tv_contrast_property;
        if !prop.is_null()
            && drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*connector).base),
                prop,
                &mut val,
            ) == 0
        {
            (*state).tv.contrast = val as u32;
        }

        prop = (*dev).mode_config.tv_flicker_reduction_property;
        if !prop.is_null()
            && drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*connector).base),
                prop,
                &mut val,
            ) == 0
        {
            (*state).tv.flicker_reduction = val as u32;
        }

        prop = (*dev).mode_config.tv_overscan_property;
        if !prop.is_null()
            && drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*connector).base),
                prop,
                &mut val,
            ) == 0
        {
            (*state).tv.overscan = val as u32;
        }

        prop = (*dev).mode_config.tv_saturation_property;
        if !prop.is_null()
            && drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*connector).base),
                prop,
                &mut val,
            ) == 0
        {
            (*state).tv.saturation = val as u32;
        }

        prop = (*dev).mode_config.tv_hue_property;
        if !prop.is_null()
            && drm_object_property_get_default_value(
                core::ptr::addr_of_mut!((*connector).base),
                prop,
                &mut val,
            ) == 0
        {
            (*state).tv.hue = val as u32;
        }

        drm_atomic_helper_connector_tv_margins_reset(connector);
    }
}

// upstream: drm_atomic_state_helper.c drm_atomic_helper_connector_tv_check()
/// # Safety
///
/// `connector` and `state` must point to valid DRM structures participating in an atomic transaction.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_helper_connector_tv_check(
    connector: *mut DrmConnector,
    state: *mut DrmAtomicCommit,
) -> i32 {
    // SAFETY: caller guarantees `connector` and `state` are valid atomic structures.
    unsafe {
        let old_conn_state = drm_atomic_get_old_connector_state(state, connector);
        let new_conn_state = drm_atomic_get_new_connector_state(state, connector);

        let crtc = (*new_conn_state).crtc;
        if crtc.is_null() {
            return 0;
        }

        let crtc_state = drm_atomic_get_new_crtc_state(state, crtc);
        if crtc_state.is_null() {
            return -drm_kapi::config::EINVAL;
        }

        if (*old_conn_state).tv.mode != (*new_conn_state).tv.mode {
            (*crtc_state).set_mode_changed(true);
        }

        if (*old_conn_state).tv.margins.left != (*new_conn_state).tv.margins.left
            || (*old_conn_state).tv.margins.right != (*new_conn_state).tv.margins.right
            || (*old_conn_state).tv.margins.top != (*new_conn_state).tv.margins.top
            || (*old_conn_state).tv.margins.bottom != (*new_conn_state).tv.margins.bottom
            || (*old_conn_state).tv.mode != (*new_conn_state).tv.mode
            || (*old_conn_state).tv.brightness != (*new_conn_state).tv.brightness
            || (*old_conn_state).tv.contrast != (*new_conn_state).tv.contrast
            || (*old_conn_state).tv.flicker_reduction != (*new_conn_state).tv.flicker_reduction
            || (*old_conn_state).tv.overscan != (*new_conn_state).tv.overscan
            || (*old_conn_state).tv.saturation != (*new_conn_state).tv.saturation
            || (*old_conn_state).tv.hue != (*new_conn_state).tv.hue
        {
            (*crtc_state).set_connectors_changed(true);
        }

        0
    }
}

// upstream: drm_atomic_state_helper.c __drm_atomic_helper_connector_duplicate_state()
/// # Safety
///
/// `connector` must point to a valid `DrmConnector` with non-null `state`, and `state` must point to valid allocated `DrmConnectorState` storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_connector_duplicate_state(
    connector: *mut DrmConnector,
    state: *mut DrmConnectorState,
) {
    // SAFETY: caller guarantees `(*connector).state` and `state` are valid, non-overlapping pointers.
    unsafe {
        core::ptr::copy_nonoverlapping((*connector).state, state, 1);
        if !(*state).crtc.is_null() {
            drm_connector_get(connector);
        }
        (*state).commit = core::ptr::null_mut();

        if !(*state).hdr_output_metadata.is_null() {
            drm_property_blob_get((*state).hdr_output_metadata);
        }

        // Don't copy over a writeback job, they are used only once
        (*state).writeback_job = core::ptr::null_mut();
    }
}

// upstream: drm_atomic_state_helper.c drm_atomic_helper_connector_duplicate_state()
/// # Safety
///
/// `connector` must point to a valid `DrmConnector` with non-null `state`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_helper_connector_duplicate_state(
    connector: *mut DrmConnector,
) -> *mut DrmConnectorState {
    // SAFETY: caller guarantees `connector` is a valid pointer; its state is copied to newly allocated memory.
    unsafe {
        if warn_on((*connector).state.is_null()) {
            return core::ptr::null_mut();
        }

        let state = drm_kapi::memory::kmalloc(size_of::<DrmConnectorState>(), GFP_KERNEL)
            as *mut DrmConnectorState;
        if !state.is_null() {
            __drm_atomic_helper_connector_duplicate_state(connector, state);
        }

        state
    }
}

// upstream: drm_atomic_state_helper.c __drm_atomic_helper_connector_destroy_state()
/// # Safety
///
/// `state` must point to a valid `DrmConnectorState`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_connector_destroy_state(
    state: *mut DrmConnectorState,
) {
    // SAFETY: caller guarantees `state` points to a valid `DrmConnectorState` whose resources are released.
    unsafe {
        if !(*state).crtc.is_null() {
            drm_connector_put((*state).connector);
        }

        if !(*state).commit.is_null() {
            drm_crtc_commit_put((*state).commit);
        }

        assert!(
            (*state).writeback_job.is_null(),
            "unsupported writeback job reached connector-state destruction"
        );

        drm_property_blob_put((*state).hdr_output_metadata);
    }
}

// upstream: drm_atomic_state_helper.c drm_atomic_helper_connector_destroy_state()
/// # Safety
///
/// `state` must point to a valid `DrmConnectorState` previously allocated via kmalloc/kzalloc.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_helper_connector_destroy_state(
    _connector: *mut DrmConnector,
    state: *mut DrmConnectorState,
) {
    // SAFETY: `state` is cleaned up via `__drm_atomic_helper_connector_destroy_state` and then freed.
    unsafe {
        __drm_atomic_helper_connector_destroy_state(state);
        drm_kapi::memory::kfree(state);
    }
}

// upstream: drm_atomic_state_helper.c __drm_atomic_helper_private_obj_create_state()
/// # Safety
///
/// `obj` must point to a valid `DrmPrivateObj`, and `state` must be null or point to valid `DrmPrivateState`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_private_obj_create_state(
    obj: *mut DrmPrivateObj,
    state: *mut DrmPrivateState,
) {
    // SAFETY: caller guarantees `obj` is valid; `state` if non-null is linked to `obj`.
    unsafe {
        if !state.is_null() {
            (*state).obj = obj;
        }
        (*obj).state = state;
    }
}

// upstream: drm_atomic_state_helper.c __drm_atomic_helper_private_obj_duplicate_state()
/// # Safety
///
/// `obj` must point to a valid `DrmPrivateObj` with non-null `state`, and `state` must point to valid allocated `DrmPrivateState` storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_atomic_helper_private_obj_duplicate_state(
    obj: *mut DrmPrivateObj,
    state: *mut DrmPrivateState,
) {
    // SAFETY: caller guarantees `(*obj).state` and `state` are valid, non-overlapping pointers.
    unsafe {
        core::ptr::copy_nonoverlapping((*obj).state, state, 1);
    }
}
