// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors.
//! Atomic plane helpers for GEM-backed framebuffers.
//!
//! Two concerns live here: gathering the implicit fences that a framebuffer's
//! GEM objects expose before a plane is scanned out, and managing the shadow
//! plane state that carries a plane's vmap bookkeeping and format-conversion
//! scratch state across atomic commits.

use core::{ffi::c_void, ptr::null_mut};

use drm_kapi::{
    config::{EINVAL, ENOMEM, GFP_KERNEL},
    dma_fence_chain::{dma_fence_chain_alloc, dma_fence_chain_init},
    memory::{kfree, kzalloc},
    types::{DmaFence, DmaFenceChain},
};

use crate::{
    dma_resv_types::DmaResv,
    drm_atomic_state_helper_upstream::{
        __drm_atomic_helper_plane_destroy_state, __drm_atomic_helper_plane_duplicate_state,
        __drm_atomic_helper_plane_reset,
    },
    drm_format_helper_types::DrmFormatConvState,
    drm_framebuffer_types::DrmFramebuffer,
    drm_gem_atomic_helper_types::{DrmShadowPlaneState, DrmSimpleDisplayPipe},
    drm_gem_framebuffer_helper_upstream::{drm_gem_fb_get_obj, drm_gem_fb_vmap, drm_gem_fb_vunmap},
    drm_plane_types::{DrmPlane, DrmPlaneState},
};

unsafe extern "C" {
    fn dma_fence_get(fence: *mut DmaFence) -> *mut DmaFence;
    fn dma_fence_put(fence: *mut DmaFence);
    fn dma_resv_get_singleton(resv: *mut DmaResv, usage: i32, fence: *mut *mut DmaFence) -> i32;

    fn drm_format_conv_state_init(state: *mut DrmFormatConvState);
    fn drm_format_conv_state_copy(
        state: *mut DrmFormatConvState,
        old_state: *const DrmFormatConvState,
    );
    fn drm_format_conv_state_release(state: *mut DrmFormatConvState);
}

/// Reservation usage that covers every fence on the object (used when the
/// plane already carries an explicit fence and only kernel work must finish).
pub const DMA_RESV_USAGE_KERNEL: i32 = 0;
/// Reservation usage that covers only writers (implicit sync default).
pub const DMA_RESV_USAGE_WRITE: i32 = 1;

/// Reinterprets a plane state as its shadow-state container.
///
/// # Safety
/// `state` must be null or a [`DrmPlaneState`] that is the first field of a
/// [`DrmShadowPlaneState`].
#[inline]
pub unsafe fn to_drm_shadow_plane_state(state: *mut DrmPlaneState) -> *mut DrmShadowPlaneState {
    state.cast::<DrmShadowPlaneState>()
}

/// Number of planes in the framebuffer's format, or 0 when no format is set.
///
/// # Safety
/// `fb` must point at a valid framebuffer.
unsafe fn format_plane_count(fb: *mut DrmFramebuffer) -> usize {
    // SAFETY: `fb` is valid per the caller contract.
    let format = unsafe { (*fb).format };
    if format.is_null() {
        return 0;
    }
    // SAFETY: a non-null format pointer refers to a live format description.
    unsafe { usize::from((*format).num_planes) }
}

/// Folds the implicit fences of every plane's GEM object into `acc`.
///
/// The function takes ownership of `acc` (which may be null) and returns the
/// combined fence owned by the caller. A chain node is created whenever two
/// fences need to be combined. On failure, every reference is dropped and the
/// error is returned.
///
/// # Safety
/// `fb` must be a valid framebuffer whose planes reference live reservation
/// objects, and `acc` must be null or an owned fence reference.
unsafe fn merge_implicit_fences(
    fb: *mut DrmFramebuffer,
    usage: i32,
    mut acc: *mut DmaFence,
) -> Result<*mut DmaFence, i32> {
    // SAFETY: `fb` is valid per the caller contract; every plane object is checked before use, and each fence reference is either kept or released exactly once.
    unsafe {
        for plane in 0..format_plane_count(fb) {
            let obj = drm_gem_fb_get_obj(fb, plane as u32);
            if obj.is_null() {
                dma_fence_put_if_set(acc);
                return Err(-EINVAL);
            }

            let mut incoming: *mut DmaFence = null_mut();
            let ret = dma_resv_get_singleton((*obj).resv, usage, &mut incoming);
            if ret != 0 {
                dma_fence_put_if_set(acc);
                return Err(ret);
            }
            if incoming.is_null() {
                continue;
            }
            if acc.is_null() {
                acc = incoming;
                continue;
            }

            let chain = dma_fence_chain_alloc::<DmaFenceChain>();
            if chain.is_null() {
                dma_fence_put(incoming);
                dma_fence_put(acc);
                return Err(-ENOMEM);
            }
            // The chain takes over both references, so `acc` is not dropped here.
            dma_fence_chain_init(chain, acc, incoming, 1);
            acc = &mut (*chain).base;
        }
    }
    Ok(acc)
}

/// Drops a fence reference when one is held.
///
/// # Safety
/// A non-null `fence` must be an owned reference.
unsafe fn dma_fence_put_if_set(fence: *mut DmaFence) {
    if !fence.is_null() {
        // SAFETY: the caller guarantees `fence` is an owned reference.
        unsafe { dma_fence_put(fence) };
    }
}

/// Prepares a plane's framebuffer for scan-out by collecting its implicit
/// fences into the plane state.
///
/// If the state has no framebuffer this is a no-op. If it has an explicit fence
/// the kernel-usage fences are added to it; otherwise the writer fences are
/// collected. On success the combined fence replaces the state's previous one.
///
/// # Safety
/// `state` must be a valid plane state. Its framebuffer, when set, must be a
/// valid GEM-backed framebuffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_plane_helper_prepare_fb(
    _plane: *mut DrmPlane,
    state: *mut DrmPlaneState,
) -> i32 {
    // SAFETY: `state` is valid per the caller contract; the framebuffer is checked for null before it is used.
    unsafe {
        let fb = (*state).fb;
        let explicit = (*state).fence;
        let held = if explicit.is_null() {
            null_mut()
        } else {
            dma_fence_get(explicit)
        };

        if fb.is_null() {
            dma_fence_put_if_set(held);
            return 0;
        }

        let usage = if held.is_null() {
            DMA_RESV_USAGE_WRITE
        } else {
            DMA_RESV_USAGE_KERNEL
        };

        match merge_implicit_fences(fb, usage, held) {
            Err(err) => err,
            Ok(combined) => {
                dma_fence_put_if_set((*state).fence);
                (*state).fence = combined;
                0
            }
        }
    }
}

/// Copies a plane state into `new_shadow_plane_state` and duplicates the
/// format-conversion state it carries.
///
/// # Safety
/// `plane` must be a valid plane whose current state is a shadow state, and
/// `new_shadow_plane_state` must be writable and not yet initialised.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_gem_duplicate_shadow_plane_state(
    plane: *mut DrmPlane,
    new_shadow_plane_state: *mut DrmShadowPlaneState,
) {
    // SAFETY: both pointers are valid per the caller contract; the source shadow state is the current plane state.
    unsafe {
        let source = to_drm_shadow_plane_state((*plane).state);
        __drm_atomic_helper_plane_duplicate_state(plane, &mut (*new_shadow_plane_state).base);
        drm_format_conv_state_copy(
            &mut (*new_shadow_plane_state).fmtcnv_state,
            &(*source).fmtcnv_state,
        );
    }
}

/// Allocates a shadow plane state that duplicates the plane's current state.
///
/// Returns null when the plane has no state or allocation fails.
///
/// # Safety
/// `plane` must be a valid plane.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_duplicate_shadow_plane_state(
    plane: *mut DrmPlane,
) -> *mut DrmPlaneState {
    // SAFETY: `plane` is valid per the caller contract; the new allocation is checked before use.
    unsafe {
        if (*plane).state.is_null() {
            return null_mut();
        }
        let shadow = kzalloc(core::mem::size_of::<DrmShadowPlaneState>(), GFP_KERNEL)
            .cast::<DrmShadowPlaneState>();
        if shadow.is_null() {
            return null_mut();
        }
        __drm_gem_duplicate_shadow_plane_state(plane, shadow);
        &mut (*shadow).base
    }
}

/// Releases the resources a shadow state owns without freeing the state.
///
/// # Safety
/// `shadow_plane_state` must be a valid shadow state that is not used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_gem_destroy_shadow_plane_state(
    shadow_plane_state: *mut DrmShadowPlaneState,
) {
    // SAFETY: the caller contract above guarantees the state is valid and owned.
    unsafe {
        drm_format_conv_state_release(&mut (*shadow_plane_state).fmtcnv_state);
        __drm_atomic_helper_plane_destroy_state(&mut (*shadow_plane_state).base);
    }
}

/// Destroys and frees a shadow plane state.
///
/// # Safety
/// `plane_state` must be a shadow state allocated by this module that is no
/// longer referenced by the plane.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_destroy_shadow_plane_state(
    _plane: *mut DrmPlane,
    plane_state: *mut DrmPlaneState,
) {
    // SAFETY: the caller contract above guarantees the state is valid, and it is freed only after its contents are released.
    unsafe {
        let shadow = to_drm_shadow_plane_state(plane_state);
        __drm_gem_destroy_shadow_plane_state(shadow);
        kfree(shadow.cast::<c_void>());
    }
}

/// Installs `shadow_plane_state` as the plane's reset state. When it is null,
/// the plane is reset without a shadow state.
///
/// # Safety
/// `plane` must be valid. `shadow_plane_state` must be null or a writable
/// shadow state that the plane will own.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __drm_gem_reset_shadow_plane(
    plane: *mut DrmPlane,
    shadow_plane_state: *mut DrmShadowPlaneState,
) {
    // SAFETY: `plane` is valid, and the shadow state is initialised only when non-null.
    unsafe {
        if shadow_plane_state.is_null() {
            __drm_atomic_helper_plane_reset(plane, null_mut());
            return;
        }
        __drm_atomic_helper_plane_reset(plane, &mut (*shadow_plane_state).base);
        drm_format_conv_state_init(&mut (*shadow_plane_state).fmtcnv_state);
    }
}

/// Resets a plane to a fresh shadow state, releasing any existing state first.
///
/// # Safety
/// `plane` must be a valid plane.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_reset_shadow_plane(plane: *mut DrmPlane) {
    // SAFETY: `plane` is valid per the caller contract; the old state is detached before it is destroyed.
    unsafe {
        if !(*plane).state.is_null() {
            drm_gem_destroy_shadow_plane_state(plane, (*plane).state);
            (*plane).state = null_mut();
        }
        let shadow = kzalloc(core::mem::size_of::<DrmShadowPlaneState>(), GFP_KERNEL)
            .cast::<DrmShadowPlaneState>();
        __drm_gem_reset_shadow_plane(plane, shadow);
    }
}

/// Maps the framebuffer behind `plane_state` into the shadow state's mapping
/// arrays. A state without a framebuffer needs no mapping.
///
/// # Safety
/// `plane_state` must be a valid shadow state for the plane.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_begin_shadow_fb_access(
    _plane: *mut DrmPlane,
    plane_state: *mut DrmPlaneState,
) -> i32 {
    // SAFETY: `plane_state` is a valid shadow state per the caller contract; its arrays are passed as the mapping destinations.
    unsafe {
        let fb = (*plane_state).fb;
        if fb.is_null() {
            return 0;
        }
        let shadow = to_drm_shadow_plane_state(plane_state);
        drm_gem_fb_vmap(fb, (*shadow).map.as_mut_ptr(), (*shadow).data.as_mut_ptr())
    }
}

/// Releases the mappings created by [`drm_gem_begin_shadow_fb_access`].
///
/// # Safety
/// `plane_state` must be a valid shadow state whose framebuffer was mapped by
/// the matching begin call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_end_shadow_fb_access(
    _plane: *mut DrmPlane,
    plane_state: *mut DrmPlaneState,
) {
    // SAFETY: `plane_state` is a valid shadow state per the caller contract.
    unsafe {
        let fb = (*plane_state).fb;
        if fb.is_null() {
            return;
        }
        let shadow = to_drm_shadow_plane_state(plane_state);
        drm_gem_fb_vunmap(fb, (*shadow).map.as_mut_ptr());
    }
}

/// Simple-KMS wrapper of [`drm_gem_begin_shadow_fb_access`] for the pipe's plane.
///
/// # Safety
/// `pipe` must be a valid display pipe and `plane_state` a valid shadow state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_simple_kms_begin_shadow_fb_access(
    pipe: *mut DrmSimpleDisplayPipe,
    plane_state: *mut DrmPlaneState,
) -> i32 {
    // SAFETY: the pipe and state are valid per the caller contract; the plane is a field of the pipe.
    unsafe { drm_gem_begin_shadow_fb_access(&mut (*pipe).plane, plane_state) }
}

/// Simple-KMS wrapper of [`drm_gem_end_shadow_fb_access`] for the pipe's plane.
///
/// # Safety
/// `pipe` must be a valid display pipe and `plane_state` a valid shadow state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_simple_kms_end_shadow_fb_access(
    pipe: *mut DrmSimpleDisplayPipe,
    plane_state: *mut DrmPlaneState,
) {
    // SAFETY: the pipe and state are valid per the caller contract; the plane is a field of the pipe.
    unsafe { drm_gem_end_shadow_fb_access(&mut (*pipe).plane, plane_state) };
}

/// Simple-KMS wrapper of [`drm_gem_reset_shadow_plane`] for the pipe's plane.
///
/// # Safety
/// `pipe` must be a valid display pipe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_simple_kms_reset_shadow_plane(pipe: *mut DrmSimpleDisplayPipe) {
    // SAFETY: `pipe` is valid per the caller contract; the plane is a field of the pipe.
    unsafe { drm_gem_reset_shadow_plane(&mut (*pipe).plane) };
}

/// Simple-KMS wrapper of [`drm_gem_duplicate_shadow_plane_state`] for the pipe's plane.
///
/// # Safety
/// `pipe` must be a valid display pipe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_simple_kms_duplicate_shadow_plane_state(
    pipe: *mut DrmSimpleDisplayPipe,
) -> *mut DrmPlaneState {
    // SAFETY: `pipe` is valid per the caller contract; the plane is a field of the pipe.
    unsafe { drm_gem_duplicate_shadow_plane_state(&mut (*pipe).plane) }
}

/// Simple-KMS wrapper of [`drm_gem_destroy_shadow_plane_state`] for the pipe's plane.
///
/// # Safety
/// `pipe` must be a valid display pipe and `plane_state` a shadow state the
/// plane no longer references.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_gem_simple_kms_destroy_shadow_plane_state(
    pipe: *mut DrmSimpleDisplayPipe,
    plane_state: *mut DrmPlaneState,
) {
    // SAFETY: the pipe and state are valid per the caller contract; the plane is a field of the pipe.
    unsafe { drm_gem_destroy_shadow_plane_state(&mut (*pipe).plane, plane_state) };
}
