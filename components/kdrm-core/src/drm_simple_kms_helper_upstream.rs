// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.

//! Simple KMS display pipeline helper.
//!
//! Provides a unified display abstraction bundling a single primary plane,
//! CRTC, and encoder into an integrated display pipeline.

#![allow(non_snake_case)]

use core::{
    ffi::{c_int, c_void},
    mem::offset_of,
    ptr,
};

use drm_kapi::assert::warn_on;

use crate::{
    drm_atomic_types::DrmAtomicCommit,
    drm_connector_types::DrmConnector,
    drm_crtc_types::{DrmCrtc, DrmCrtcFuncs, DrmCrtcState},
    drm_device_types::DrmDevice,
    drm_encoder_types::{DrmEncoder, DrmEncoderFuncs},
    drm_mode_object_upstream::DRIVER_GEM,
    drm_modes_types::{DrmDisplayMode, DrmModeStatus, MODE_OK},
    drm_modeset_helper_vtables_types::{DrmCrtcHelperFuncs, DrmPlaneHelperFuncs},
    drm_plane_types::{DRM_PLANE_TYPE_PRIMARY, DrmPlane, DrmPlaneFuncs, DrmPlaneState},
    drm_simple_kms_helper_types_upstream::{DrmSimpleDisplayPipe, DrmSimpleDisplayPipeFuncs},
    uapi_drm_consts::DRM_FORMAT_MOD_LINEAR,
};

const DRM_PLANE_NO_SCALING: i32 = 1 << 16;

static SIMPLE_ENCODER_FUNCS: DrmEncoderFuncs = DrmEncoderFuncs {
    reset: None,
    destroy: Some(crate::drm_encoder_upstream::drm_encoder_cleanup),
    late_register: None,
    early_unregister: None,
    debugfs_init: None,
};

static SIMPLE_CRTC_HELPER_FUNCS: DrmCrtcHelperFuncs = DrmCrtcHelperFuncs {
    dpms: None,
    prepare: None,
    commit: None,
    mode_valid: Some(crtc_validate_mode),
    mode_fixup: None,
    mode_set: None,
    mode_set_nofb: None,
    mode_set_base: None,
    disable: None,
    atomic_check: Some(crtc_atomic_check),
    atomic_begin: None,
    atomic_flush: Some(crate::drm_vblank_helper_upstream::drm_crtc_vblank_atomic_flush),
    atomic_enable: Some(crtc_atomic_enable),
    atomic_disable: Some(crtc_atomic_disable),
    get_scanout_position: None,
    handle_vblank_timeout: None,
};

static SIMPLE_CRTC_FUNCS: DrmCrtcFuncs = DrmCrtcFuncs {
    reset: Some(crtc_reset_state),
    cursor_set: None,
    cursor_set2: None,
    cursor_move: None,
    gamma_set: None,
    destroy: Some(crate::drm_crtc_upstream::drm_crtc_cleanup),
    set_config: Some(crate::drm_atomic_helper_upstream::drm_atomic_helper_set_config),
    page_flip: Some(crate::drm_atomic_helper_upstream::drm_atomic_helper_page_flip),
    page_flip_target: None,
    set_property: None,
    atomic_duplicate_state: Some(crtc_duplicate_state),
    atomic_destroy_state: Some(crtc_destroy_state),
    atomic_set_property: None,
    atomic_get_property: None,
    late_register: None,
    early_unregister: None,
    set_crc_source: None,
    verify_crc_source: None,
    get_crc_sources: None,
    atomic_print_state: None,
    get_vblank_counter: None,
    enable_vblank: Some(crtc_enable_vblank),
    disable_vblank: Some(crtc_disable_vblank),
    get_vblank_timestamp: Some(
        crate::drm_vblank_helper_upstream::drm_crtc_vblank_helper_get_vblank_timestamp_from_timer,
    ),
};

static SIMPLE_PLANE_HELPER_FUNCS: DrmPlaneHelperFuncs = DrmPlaneHelperFuncs {
    prepare_fb: Some(plane_prepare_fb),
    cleanup_fb: Some(plane_cleanup_fb),
    begin_fb_access: Some(plane_begin_fb_access),
    end_fb_access: Some(plane_end_fb_access),
    atomic_check: Some(plane_atomic_check),
    atomic_update: Some(plane_atomic_update),
    atomic_enable: None,
    atomic_disable: None,
    atomic_async_check: None,
    atomic_async_update: None,
    get_scanout_buffer: None,
    panic_flush: None,
};

static SIMPLE_PLANE_FUNCS: DrmPlaneFuncs = DrmPlaneFuncs {
    update_plane: Some(crate::drm_atomic_helper_upstream::drm_atomic_helper_update_plane),
    disable_plane: Some(crate::drm_atomic_helper_upstream::drm_atomic_helper_disable_plane),
    destroy: Some(crate::drm_plane_upstream::drm_plane_cleanup),
    reset: Some(plane_reset),
    set_property: None,
    atomic_duplicate_state: Some(plane_duplicate_state),
    atomic_destroy_state: Some(plane_destroy_state),
    atomic_set_property: None,
    atomic_get_property: None,
    late_register: None,
    early_unregister: None,
    atomic_print_state: None,
    format_mod_supported: Some(plane_format_mod_supported),
    format_mod_supported_async: None,
};

/// Derives the outer [`DrmSimpleDisplayPipe`] reference pointer from its embedded CRTC.
///
/// # Safety
///
/// `crtc` must be a valid, aligned, non-null pointer referencing the embedded CRTC
/// within an active [`DrmSimpleDisplayPipe`].
#[inline]
unsafe fn display_pipe_from_crtc(crtc: *mut DrmCrtc) -> *mut DrmSimpleDisplayPipe {
    // SAFETY: DrmSimpleDisplayPipe is #[repr(C)] with DrmCrtc located at byte offset 0.
    crtc.cast()
}

/// Derives the outer [`DrmSimpleDisplayPipe`] reference pointer from its embedded Plane.
///
/// # Safety
///
/// `plane` must be a valid, aligned, non-null pointer referencing the embedded plane
/// within an active [`DrmSimpleDisplayPipe`].
#[inline]
unsafe fn display_pipe_from_plane(plane: *mut DrmPlane) -> *mut DrmSimpleDisplayPipe {
    // SAFETY: `plane` points to the plane field of an enclosing DrmSimpleDisplayPipe.
    // Subtracting the offset of the plane field yields the base pointer of the pipe.
    plane
        .cast::<u8>()
        .wrapping_sub(offset_of!(DrmSimpleDisplayPipe, plane))
        .cast()
}

/// Retrieves an optional reference to the display pipe's driver callbacks table.
///
/// # Safety
///
/// `pipe` must point to a live [`DrmSimpleDisplayPipe`] whose `funcs` field, if non-null,
/// references a valid static [`DrmSimpleDisplayPipeFuncs`] table.
#[inline]
unsafe fn display_pipe_callbacks<'a>(
    pipe: *mut DrmSimpleDisplayPipe,
) -> Option<&'a DrmSimpleDisplayPipeFuncs> {
    if pipe.is_null() {
        return None;
    }
    // SAFETY: `pipe` is verified non-null and valid. Reading `funcs` and converting to
    // an immutable reference is sound because `funcs` points to static immutable table data.
    unsafe { (*pipe).funcs.as_ref() }
}

/// Validates a requested display mode against driver pipeline constraints.
///
/// # Safety
///
/// `crtc` must point to the embedded CRTC of a valid display pipe. `mode` must point
/// to a valid display mode structure.
unsafe extern "C" fn crtc_validate_mode(
    crtc: *mut DrmCrtc,
    mode: *mut DrmDisplayMode,
) -> DrmModeStatus {
    // SAFETY: The caller supplies a valid CRTC embedded within a live display pipe.
    let pipe = unsafe { display_pipe_from_crtc(crtc) };
    // SAFETY: pipe is guaranteed valid for the lifetime of this callback.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    if let Some(hook) = funcs.and_then(|f| f.mode_valid) {
        // SAFETY: Delegating to the driver callback with verified pipe and mode pointers.
        unsafe { hook(pipe, mode) }
    } else {
        MODE_OK
    }
}

/// Validates primary plane configuration and enrolls planes into the atomic commit.
///
/// # Safety
///
/// Both `crtc` and `state` must be valid, aligned pointers within an active atomic commit context.
unsafe extern "C" fn crtc_atomic_check(crtc: *mut DrmCrtc, state: *mut DrmAtomicCommit) -> i32 {
    // SAFETY: State and CRTC pointers are managed and validated by DRM atomic core.
    let new_crtc_state =
        unsafe { crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(state, crtc) };

    // SAFETY: new_crtc_state is guaranteed non-null and valid by the atomic transaction.
    let is_enabled = unsafe { (*new_crtc_state).enable };
    if is_enabled {
        // SAFETY: Verifying primary plane dimensions against new active CRTC timing.
        let plane_check = unsafe {
            crate::drm_atomic_helper_upstream::drm_atomic_helper_check_crtc_primary_plane(
                new_crtc_state,
            )
        };
        if plane_check != 0 {
            return plane_check;
        }
    }

    // SAFETY: Adding all affected planes on this CRTC into the atomic update transaction.
    unsafe { crate::drm_atomic_upstream::drm_atomic_add_affected_planes(state, crtc) }
}

/// Activates the display pipeline and enables vertical blanking interrupts.
///
/// # Safety
///
/// `crtc` must be an embedded CRTC within a valid display pipe. `_state` is unused.
unsafe extern "C" fn crtc_atomic_enable(crtc: *mut DrmCrtc, _state: *mut DrmAtomicCommit) {
    // SAFETY: The CRTC is embedded inside an active DrmSimpleDisplayPipe.
    let pipe = unsafe { display_pipe_from_crtc(crtc) };
    // SAFETY: pipe pointer is valid for the duration of the modeset transaction.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    if let Some(enable_fn) = funcs.and_then(|f| f.enable) {
        // SAFETY: Reading valid current crtc state pointer.
        let crtc_state = unsafe { (*crtc).state };
        // SAFETY: Reading valid current plane state pointer from the embedded plane.
        let plane_state = unsafe { (*pipe).plane.state };
        // SAFETY: Forwarding hardware enable transition to the driver pipeline handler.
        unsafe { enable_fn(pipe, crtc_state, plane_state) };
    }

    // SAFETY: Enabling vblank handling for the newly activated CRTC.
    unsafe { crate::drm_vblank_upstream::drm_crtc_vblank_on(crtc) };
}

/// Disables vertical blanking interrupts and halts display pipeline output.
///
/// # Safety
///
/// `crtc` must be an embedded CRTC within a valid display pipe. `_state` is unused.
unsafe extern "C" fn crtc_atomic_disable(crtc: *mut DrmCrtc, _state: *mut DrmAtomicCommit) {
    // SAFETY: Turning off vblank events prior to hardware shutdown.
    unsafe { crate::drm_vblank_upstream::drm_crtc_vblank_off(crtc) };

    // SAFETY: Resolving enclosing pipe pointer from CRTC.
    let pipe = unsafe { display_pipe_from_crtc(crtc) };
    // SAFETY: pipe remains valid.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    if let Some(disable_fn) = funcs.and_then(|f| f.disable) {
        // SAFETY: Calling driver-supplied pipeline disable hook.
        unsafe { disable_fn(pipe) };
    }
}

/// Resets CRTC state, preferring the driver pipe's custom reset callback.
///
/// # Safety
///
/// `crtc` must point to a valid embedded CRTC inside a live display pipe.
unsafe extern "C" fn crtc_reset_state(crtc: *mut DrmCrtc) {
    // SAFETY: CRTC is embedded in a valid pipe.
    let pipe = unsafe { display_pipe_from_crtc(crtc) };
    // SAFETY: Retrieving callback table.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    if let Some(custom_reset) = funcs.and_then(|f| f.reset_crtc) {
        // SAFETY: Invoking custom reset logic provided by driver.
        unsafe { custom_reset(pipe) };
    } else {
        // SAFETY: Falling back to DRM atomic helper CRTC reset.
        unsafe { crate::drm_atomic_state_helper_upstream::drm_atomic_helper_crtc_reset(crtc) };
    }
}

/// Duplicates CRTC atomic state, preferring the driver pipe's custom hook.
///
/// # Safety
///
/// `crtc` must point to a valid embedded CRTC inside a live display pipe.
unsafe extern "C" fn crtc_duplicate_state(crtc: *mut DrmCrtc) -> *mut DrmCrtcState {
    // SAFETY: CRTC is embedded in a valid pipe.
    let pipe = unsafe { display_pipe_from_crtc(crtc) };
    // SAFETY: Retrieving callback table.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    if let Some(custom_dup) = funcs.and_then(|f| f.duplicate_crtc_state) {
        // SAFETY: Invoking custom state duplication.
        unsafe { custom_dup(pipe) }
    } else {
        // SAFETY: Falling back to default atomic helper state duplication.
        unsafe {
            crate::drm_atomic_state_helper_upstream::drm_atomic_helper_crtc_duplicate_state(crtc)
        }
    }
}

/// Destroys a previously duplicated CRTC state instance.
///
/// # Safety
///
/// `crtc` must point to a valid embedded CRTC inside a live display pipe, and `state` must
/// point to a state allocated for this CRTC.
unsafe extern "C" fn crtc_destroy_state(crtc: *mut DrmCrtc, state: *mut DrmCrtcState) {
    // SAFETY: CRTC is embedded in a valid pipe.
    let pipe = unsafe { display_pipe_from_crtc(crtc) };
    // SAFETY: Retrieving callback table.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    if let Some(custom_destroy) = funcs.and_then(|f| f.destroy_crtc_state) {
        // SAFETY: Delegating to custom state teardown.
        unsafe { custom_destroy(pipe, state) };
    } else {
        // SAFETY: Standard atomic helper CRTC state destruction.
        unsafe {
            crate::drm_atomic_state_helper_upstream::drm_atomic_helper_crtc_destroy_state(
                crtc, state,
            )
        };
    }
}

/// Enables vblank interrupt events on the pipeline.
///
/// # Safety
///
/// `crtc` must be an embedded CRTC within a valid display pipe.
unsafe extern "C" fn crtc_enable_vblank(crtc: *mut DrmCrtc) -> i32 {
    // SAFETY: CRTC is embedded in a valid pipe.
    let pipe = unsafe { display_pipe_from_crtc(crtc) };
    // SAFETY: Retrieving callback table.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    match funcs.and_then(|f| f.enable_vblank) {
        // SAFETY: Calling driver-provided enable_vblank callback.
        Some(enable_hook) => unsafe { enable_hook(pipe) },
        None => 0,
    }
}

/// Disables vblank interrupt events on the pipeline.
///
/// # Safety
///
/// `crtc` must be an embedded CRTC within a valid display pipe.
unsafe extern "C" fn crtc_disable_vblank(crtc: *mut DrmCrtc) {
    // SAFETY: CRTC is embedded in a valid pipe.
    let pipe = unsafe { display_pipe_from_crtc(crtc) };
    // SAFETY: Retrieving callback table.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    if let Some(disable_hook) = funcs.and_then(|f| f.disable_vblank) {
        // SAFETY: Calling driver-provided disable_vblank callback.
        unsafe { disable_hook(pipe) };
    }
}

/// Verifies plane coordinates and dimensions, then delegates to driver check logic.
///
/// # Safety
///
/// `plane` must point to the embedded plane of a valid display pipe. `state` must point
/// to an active atomic commit state.
unsafe extern "C" fn plane_atomic_check(plane: *mut DrmPlane, state: *mut DrmAtomicCommit) -> i32 {
    // SAFETY: Obtaining new plane state from the atomic transaction.
    let plane_state =
        unsafe { crate::drm_atomic_upstream::drm_atomic_get_new_plane_state(state, plane) };
    // SAFETY: Resolving outer pipeline from the embedded plane.
    let pipe = unsafe { display_pipe_from_plane(plane) };
    // SAFETY: Obtaining new state for the associated CRTC in the atomic transaction.
    let crtc_state = unsafe {
        crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(state, &mut (*pipe).crtc)
    };

    // Simple pipelines do not support scaling; require 1:1 scaling factor.
    // SAFETY: Pointers are valid and non-null within atomic check phase.
    let check_status = unsafe {
        crate::drm_atomic_helper_upstream::drm_atomic_helper_check_plane_state(
            plane_state,
            crtc_state,
            DRM_PLANE_NO_SCALING,
            DRM_PLANE_NO_SCALING,
            false,
            false,
        )
    };
    if check_status != 0 {
        return check_status;
    }

    // SAFETY: Reading visibility flag from validated plane state.
    let is_visible = unsafe { (*plane_state).visible };
    if !is_visible {
        return 0;
    }

    // SAFETY: Querying callback table.
    let funcs = unsafe { display_pipe_callbacks(pipe) };
    if let Some(driver_check) = funcs.and_then(|f| f.check) {
        // SAFETY: Invoking driver plane check hook.
        unsafe { driver_check(pipe, plane_state, crtc_state) }
    } else {
        0
    }
}

/// Updates primary plane hardware display scanout.
///
/// # Safety
///
/// `plane` must be the embedded plane of a live display pipe. `state` must point
/// to an active atomic commit state.
unsafe extern "C" fn plane_atomic_update(plane: *mut DrmPlane, state: *mut DrmAtomicCommit) {
    // SAFETY: Resolving enclosing pipe pointer from plane.
    let pipe = unsafe { display_pipe_from_plane(plane) };
    // SAFETY: Retrieving callback table.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    if let Some(update_hook) = funcs.and_then(|f| f.update) {
        // SAFETY: Fetching previous plane state for differential hardware update.
        let old_state =
            unsafe { crate::drm_atomic_upstream::drm_atomic_get_old_plane_state(state, plane) };
        // SAFETY: Forwarding update event to driver callback.
        unsafe { update_hook(pipe, old_state) };
    }
}

/// Prepares a framebuffer for display scanout.
///
/// # Safety
///
/// `plane` must be the embedded plane of a valid display pipe. `state` must point
/// to a valid plane state containing a framebuffer reference.
unsafe extern "C" fn plane_prepare_fb(plane: *mut DrmPlane, state: *mut DrmPlaneState) -> i32 {
    // SAFETY: Resolving enclosing pipe pointer from plane.
    let pipe = unsafe { display_pipe_from_plane(plane) };
    // SAFETY: Querying callback table.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    if let Some(custom_prepare) = funcs.and_then(|f| f.prepare_fb) {
        // SAFETY: Delegating to driver framebuffer preparation hook.
        return unsafe { custom_prepare(pipe, state) };
    }

    // SAFETY: Plane points to a valid struct whose dev field references an initialized DrmDevice.
    let dev = unsafe { (*plane).dev };
    // SAFETY: Dereferencing valid DrmDevice to inspect driver capability flags.
    let features = unsafe { (*dev).driver_features };
    if warn_on(features & DRIVER_GEM == 0) {
        return 0;
    }

    // Specifying cleanup_fb without a matching custom prepare_fb is a driver logic error.
    warn_on(funcs.is_some_and(|f| f.cleanup_fb.is_some()));

    // SAFETY: Defaulting to standard GEM plane framebuffer preparation helper.
    unsafe { crate::drm_gem_atomic_helper_upstream::drm_gem_plane_helper_prepare_fb(plane, state) }
}

/// Releases resources associated with a framebuffer scanout preparation.
///
/// # Safety
///
/// `plane` must be the embedded plane of a valid display pipe. `state` must point
/// to a valid plane state.
unsafe extern "C" fn plane_cleanup_fb(plane: *mut DrmPlane, state: *mut DrmPlaneState) {
    // SAFETY: Resolving enclosing pipe pointer from plane.
    let pipe = unsafe { display_pipe_from_plane(plane) };
    // SAFETY: Querying callback table.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    if let Some(custom_cleanup) = funcs.and_then(|f| f.cleanup_fb) {
        // SAFETY: Invoking custom cleanup callback.
        unsafe { custom_cleanup(pipe, state) };
    }
}

/// Prepares hardware or memory mapping prior to reading framebuffer content.
///
/// # Safety
///
/// `plane` must be the embedded plane of a valid display pipe. `state` must point
/// to a valid plane state.
unsafe extern "C" fn plane_begin_fb_access(plane: *mut DrmPlane, state: *mut DrmPlaneState) -> i32 {
    // SAFETY: Resolving enclosing pipe pointer from plane.
    let pipe = unsafe { display_pipe_from_plane(plane) };
    // SAFETY: Querying callback table.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    match funcs.and_then(|f| f.begin_fb_access) {
        // SAFETY: Invoking driver begin_fb_access hook.
        Some(begin_hook) => unsafe { begin_hook(pipe, state) },
        None => 0,
    }
}

/// Releases access guards after completing framebuffer access.
///
/// # Safety
///
/// `plane` must be the embedded plane of a valid display pipe. `state` must point
/// to a valid plane state.
unsafe extern "C" fn plane_end_fb_access(plane: *mut DrmPlane, state: *mut DrmPlaneState) {
    // SAFETY: Resolving enclosing pipe pointer from plane.
    let pipe = unsafe { display_pipe_from_plane(plane) };
    // SAFETY: Querying callback table.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    if let Some(end_hook) = funcs.and_then(|f| f.end_fb_access) {
        // SAFETY: Invoking driver end_fb_access hook.
        unsafe { end_hook(pipe, state) };
    }
}

/// Checks whether a given pixel format modifier is supported by the plane.
///
/// Simple display pipelines only accept linear framebuffer memory layouts.
///
/// # Safety
///
/// Safe to invoke with any parameters; parameters are not dereferenced.
unsafe extern "C" fn plane_format_mod_supported(
    _plane: *mut DrmPlane,
    _format: u32,
    modifier: u64,
) -> bool {
    modifier == DRM_FORMAT_MOD_LINEAR
}

/// Resets plane atomic state, preferring the driver pipe's custom hook.
///
/// # Safety
///
/// `plane` must point to the embedded plane of a valid display pipe.
unsafe extern "C" fn plane_reset(plane: *mut DrmPlane) {
    // SAFETY: Resolving enclosing pipe pointer from plane.
    let pipe = unsafe { display_pipe_from_plane(plane) };
    // SAFETY: Querying callback table.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    if let Some(custom_reset) = funcs.and_then(|f| f.reset_plane) {
        // SAFETY: Delegating to driver reset hook.
        unsafe { custom_reset(pipe) };
    } else {
        // SAFETY: Falling back to standard atomic helper plane reset.
        unsafe { crate::drm_atomic_state_helper_upstream::drm_atomic_helper_plane_reset(plane) };
    }
}

/// Duplicates plane atomic state, preferring the driver pipe's custom hook.
///
/// # Safety
///
/// `plane` must point to the embedded plane of a valid display pipe.
unsafe extern "C" fn plane_duplicate_state(plane: *mut DrmPlane) -> *mut DrmPlaneState {
    // SAFETY: Resolving enclosing pipe pointer from plane.
    let pipe = unsafe { display_pipe_from_plane(plane) };
    // SAFETY: Querying callback table.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    if let Some(custom_dup) = funcs.and_then(|f| f.duplicate_plane_state) {
        // SAFETY: Delegating to custom state duplication.
        unsafe { custom_dup(pipe) }
    } else {
        // SAFETY: Standard atomic plane duplicate state helper.
        unsafe {
            crate::drm_atomic_state_helper_upstream::drm_atomic_helper_plane_duplicate_state(plane)
        }
    }
}

/// Destroys a previously duplicated plane state instance.
///
/// # Safety
///
/// `plane` must point to the embedded plane of a valid display pipe, and `state` must
/// point to a state allocated for this plane.
unsafe extern "C" fn plane_destroy_state(plane: *mut DrmPlane, state: *mut DrmPlaneState) {
    // SAFETY: Resolving enclosing pipe pointer from plane.
    let pipe = unsafe { display_pipe_from_plane(plane) };
    // SAFETY: Querying callback table.
    let funcs = unsafe { display_pipe_callbacks(pipe) };

    if let Some(custom_destroy) = funcs.and_then(|f| f.destroy_plane_state) {
        // SAFETY: Delegating to custom state destruction.
        unsafe { custom_destroy(pipe, state) };
    } else {
        // SAFETY: Standard atomic plane state destructor.
        unsafe {
            crate::drm_atomic_state_helper_upstream::drm_atomic_helper_plane_destroy_state(
                plane, state,
            )
        };
    }
}

#[unsafe(no_mangle)]
/// Initializes a DRM encoder backed by the simple pipeline encoder operations.
///
/// # Safety
///
/// `dev` must point to an initialized [`DrmDevice`]. `encoder` must point to an
/// allocated [`DrmEncoder`] to be registered.
pub unsafe extern "C" fn drm_simple_encoder_init(
    dev: *mut DrmDevice,
    encoder: *mut DrmEncoder,
    encoder_type: c_int,
) -> i32 {
    // SAFETY: Passing valid pointers to DRM core encoder initialization routine
    // with the static simple encoder operations table.
    unsafe {
        crate::drm_encoder_upstream::drm_encoder_init(
            dev,
            encoder,
            &SIMPLE_ENCODER_FUNCS,
            encoder_type,
            ptr::null(),
        )
    }
}

#[unsafe(no_mangle)]
/// Allocates a managed DRM encoder embedded within a parent driver structure.
///
/// # Safety
///
/// `dev` must point to a live [`DrmDevice`]. `size` and `offset` must describe a valid
/// memory layout where `offset + size_of::<DrmEncoder>() <= size`.
pub unsafe extern "C" fn __drmm_simple_encoder_alloc(
    dev: *mut DrmDevice,
    size: usize,
    offset: usize,
    encoder_type: c_int,
) -> *mut c_void {
    // SAFETY: Delegating managed allocation to the DRM core encoder allocator.
    unsafe {
        crate::drm_encoder_upstream::__drmm_encoder_alloc(
            dev,
            size,
            offset,
            ptr::null(),
            encoder_type,
            ptr::null(),
        )
    }
}

#[unsafe(no_mangle)]
/// Attaches an external display bridge to the display pipe.
///
/// External bridges are unsupported in this lightweight display pipe abstraction.
///
/// # Safety
///
/// Safe to call with any pointer values.
pub unsafe extern "C" fn drm_simple_display_pipe_attach_bridge(
    _pipe: *mut DrmSimpleDisplayPipe,
    _bridge: *mut c_void,
) -> i32 {
    -drm_kapi::config::EOPNOTSUPP
}

#[unsafe(no_mangle)]
/// Initializes a complete unified simple display pipeline consisting of a primary plane,
/// CRTC, and encoder, optionally linking an output connector.
///
/// # Safety
///
/// - `dev` must point to an active, valid [`DrmDevice`].
/// - `pipe` must point to an allocated [`DrmSimpleDisplayPipe`] structure that will remain
///   pinned and valid for the lifetime of the DRM device modesetting subsystem.
/// - `funcs` must point to a valid [`DrmSimpleDisplayPipeFuncs`] callback table that outlives `pipe`.
/// - `formats` must point to an array of supported pixel format FourCC values with `format_count` items.
/// - `format_modifiers`, if non-null, must point to an array of modifiers matching `formats`.
/// - `connector`, if non-null, must point to an initialized [`DrmConnector`].
pub unsafe extern "C" fn drm_simple_display_pipe_init(
    dev: *mut DrmDevice,
    pipe: *mut DrmSimpleDisplayPipe,
    funcs: *const DrmSimpleDisplayPipeFuncs,
    formats: *const u32,
    format_count: u32,
    format_modifiers: *const u64,
    connector: *mut DrmConnector,
) -> i32 {
    // SAFETY: Storing callback table and connector pointers, and wiring the plane helper vtable.
    unsafe {
        (*pipe).connector = connector;
        (*pipe).funcs = funcs;
        (*pipe).plane.helper_private =
            &SIMPLE_PLANE_HELPER_FUNCS as *const DrmPlaneHelperFuncs as *mut DrmPlaneHelperFuncs;
    }

    // Register the underlying primary plane.
    // SAFETY: Initializing the primary plane within the display pipe.
    let plane_init_status = unsafe {
        crate::drm_plane_upstream::drm_universal_plane_init(
            dev,
            &mut (*pipe).plane,
            0,
            &SIMPLE_PLANE_FUNCS,
            formats,
            format_count,
            format_modifiers,
            DRM_PLANE_TYPE_PRIMARY,
            ptr::null(),
        )
    };
    if plane_init_status != 0 {
        return plane_init_status;
    }

    // Attach CRTC helper callbacks and register CRTC linked with the primary plane.
    // SAFETY: Setting helper vtable for the CRTC.
    unsafe {
        (*pipe).crtc.helper_private =
            &SIMPLE_CRTC_HELPER_FUNCS as *const DrmCrtcHelperFuncs as *mut DrmCrtcHelperFuncs;
    }

    // SAFETY: Initializing the CRTC with the embedded primary plane.
    let crtc_init_status = unsafe {
        crate::drm_crtc_upstream::drm_crtc_init_with_planes(
            dev,
            &mut (*pipe).crtc,
            &mut (*pipe).plane,
            ptr::null_mut(),
            &SIMPLE_CRTC_FUNCS,
            ptr::null(),
        )
    };
    if crtc_init_status != 0 {
        return crtc_init_status;
    }

    // Configure encoder to drive this specific CRTC.
    // SAFETY: Setting possible_crtcs bitmask from the initialized CRTC.
    unsafe {
        (*pipe).encoder.possible_crtcs = crate::drm_crtc_upstream::drm_crtc_mask(&(*pipe).crtc);
    }

    // Initialize the encoder.
    // SAFETY: Invoking encoder initialization for the pipe's embedded encoder.
    let enc_init_status = unsafe {
        drm_simple_encoder_init(
            dev,
            &mut (*pipe).encoder,
            crate::uapi_drm_consts::DRM_MODE_ENCODER_NONE as i32,
        )
    };
    if enc_init_status != 0 || connector.is_null() {
        return enc_init_status;
    }

    // Bind output connector to the pipeline's encoder.
    // SAFETY: Attaching the verified non-null connector to the initialized encoder.
    unsafe {
        crate::drm_connector_upstream::drm_connector_attach_encoder(connector, &mut (*pipe).encoder)
    }
}

#[cfg(test)]
mod tests {
    use core::ptr;

    use super::*;

    #[test]
    fn simple_crtc_wires_timer_vblank_and_atomic_flip_events() {
        assert!(SIMPLE_CRTC_FUNCS.enable_vblank.is_some());
        assert!(SIMPLE_CRTC_FUNCS.disable_vblank.is_some());
        assert!(SIMPLE_CRTC_FUNCS.get_vblank_timestamp.is_some());
        assert!(SIMPLE_CRTC_HELPER_FUNCS.atomic_flush.is_some());
    }

    /// # Safety
    ///
    /// Validates test invariant that pointers passed by caller are non-null.
    unsafe extern "C" fn mock_prepare_fb(
        pipe: *mut DrmSimpleDisplayPipe,
        state: *mut DrmPlaneState,
    ) -> i32 {
        assert!(!pipe.is_null());
        assert!(!state.is_null());
        37
    }

    #[test]
    fn simple_plane_prepare_dispatches_to_driver_callback() {
        let funcs = DrmSimpleDisplayPipeFuncs {
            mode_valid: None,
            enable: None,
            disable: None,
            check: None,
            update: None,
            prepare_fb: Some(mock_prepare_fb),
            cleanup_fb: None,
            begin_fb_access: None,
            end_fb_access: None,
            enable_vblank: None,
            disable_vblank: None,
            reset_crtc: None,
            duplicate_crtc_state: None,
            destroy_crtc_state: None,
            reset_plane: None,
            duplicate_plane_state: None,
            destroy_plane_state: None,
        };
        // SAFETY: Zeroed initialization is safe for plain structure testing layout.
        let mut pipe: DrmSimpleDisplayPipe = unsafe { core::mem::zeroed() };
        pipe.funcs = &funcs;
        // SAFETY: Zeroed plane state for unit test evaluation.
        let mut state: DrmPlaneState = unsafe { core::mem::zeroed() };

        // SAFETY: Pointers to stack test fixtures are valid and properly aligned.
        let result = unsafe { plane_prepare_fb(&mut pipe.plane, &mut state) };
        assert_eq!(result, 37);
    }

    #[test]
    fn simple_plane_uses_linux_driver_gem_feature_bit() {
        let funcs = DrmSimpleDisplayPipeFuncs {
            mode_valid: None,
            enable: None,
            disable: None,
            check: None,
            update: None,
            prepare_fb: None,
            cleanup_fb: None,
            begin_fb_access: None,
            end_fb_access: None,
            enable_vblank: None,
            disable_vblank: None,
            reset_crtc: None,
            duplicate_crtc_state: None,
            destroy_crtc_state: None,
            reset_plane: None,
            duplicate_plane_state: None,
            destroy_plane_state: None,
        };
        // SAFETY: Test fixture initialized to zeroes.
        let mut dev: DrmDevice = unsafe { core::mem::zeroed() };
        // Set DRIVER_RENDER without DRIVER_GEM; helper must not treat driver as GEM-enabled.
        dev.driver_features = crate::drm_mode_object_upstream::DRIVER_RENDER;

        // SAFETY: Test fixture initialized to zeroes.
        let mut pipe: DrmSimpleDisplayPipe = unsafe { core::mem::zeroed() };
        pipe.funcs = &funcs;
        pipe.plane.dev = &mut dev;

        // SAFETY: Test fixture initialized to zeroes.
        let mut format: crate::drm_fourcc_types::DrmFormatInfo = unsafe { core::mem::zeroed() };
        format.num_planes = 1;

        // SAFETY: Test fixture initialized to zeroes.
        let mut fb: crate::drm_framebuffer_types::DrmFramebuffer = unsafe { core::mem::zeroed() };
        fb.format = &mut format;

        // SAFETY: Test fixture initialized to zeroes.
        let mut state: DrmPlaneState = unsafe { core::mem::zeroed() };
        state.fb = &mut fb;

        // SAFETY: Test fixture pointers are live on stack and valid for the call duration.
        let status = unsafe { plane_prepare_fb(&mut pipe.plane, &mut state) };
        assert_eq!(status, 0);
    }

    #[test]
    fn simple_plane_only_accepts_linear_modifiers() {
        // SAFETY: Modifier support check does not dereference the plane pointer.
        let linear_ok =
            unsafe { plane_format_mod_supported(ptr::null_mut(), 0, DRM_FORMAT_MOD_LINEAR) };
        assert!(linear_ok);

        // SAFETY: Modifier support check does not dereference the plane pointer.
        let tiled_rejected = unsafe { plane_format_mod_supported(ptr::null_mut(), 0, 1) };
        assert!(!tiled_rejected);
    }
}
