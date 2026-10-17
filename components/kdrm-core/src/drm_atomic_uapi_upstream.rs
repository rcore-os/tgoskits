// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_atomic_uapi.c`.

#![allow(non_snake_case)]

use core::{
    ffi::{c_int, c_void},
    mem::size_of,
};

use drm_kapi::{
    config::{EDEADLK, EFAULT, EINVAL, ENOENT, ENOMEM, EOPNOTSUPP, GFP_KERNEL, IS_ERR, PTR_ERR},
    memory::{kfree, kzalloc},
    sync_file::{SyncFile, sync_file_create, sync_file_get_fence},
    types::DmaFence,
};

use crate::{
    drm_atomic_types::DrmAtomicCommit,
    drm_connector_types::{DRM_LINK_STATUS_GOOD, DrmConnector, DrmConnectorState},
    drm_crtc_types::{DrmCrtc, DrmCrtcState},
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_framebuffer_types::DrmFramebuffer,
    drm_mode_object_types::DrmModeObject,
    drm_mode_object_upstream::DRIVER_ATOMIC,
    drm_modes_types::DrmDisplayMode,
    drm_modeset_lock_types::DrmModesetAcquireCtx,
    drm_modeset_lock_upstream::drm_modeset_lock,
    drm_plane_types::{DRM_PLANE_TYPE_CURSOR, DrmPlane, DrmPlaneState},
    drm_property_types::{DrmProperty, DrmPropertyBlob},
    drm_vblank_types::DrmPendingVblankEvent,
    uapi_drm_consts::*,
    uapi_drm_mode_types::{DrmColorLut, DrmModeAtomic, DrmModeModeinfo, DrmModeRect},
    uapi_drm_types::{DrmEvent, DrmEventVblank},
};

const EACCES: i32 = 13;
const O_CLOEXEC: i32 = 0x80000;
const DRM_MODESET_ACQUIRE_INTERRUPTIBLE: u32 = 0x1;

// SAFETY: Foreign C declarations linked from peer DRM modules or kernel core.
unsafe extern "C" {
    fn drm_property_create_blob(
        dev: *mut DrmDevice,
        length: usize,
        data: *const c_void,
    ) -> *mut DrmPropertyBlob;
    fn drm_property_blob_put(blob: *mut DrmPropertyBlob);
    fn drm_property_blob_get(blob: *mut DrmPropertyBlob) -> *mut DrmPropertyBlob;
    fn drm_property_lookup_blob(dev: *mut DrmDevice, id: u32) -> *mut DrmPropertyBlob;
    fn drm_property_replace_blob_from_id(
        dev: *mut DrmDevice,
        blob: *mut *mut DrmPropertyBlob,
        blob_id: u64,
        expected_size: isize,
        expected_elem_size: isize,
        replaced: *mut bool,
    ) -> i32;
    fn drm_property_change_valid_get(
        prop: *mut DrmProperty,
        value: u64,
        ref_obj: *mut *mut DrmModeObject,
    ) -> bool;
    fn drm_property_change_valid_put(prop: *mut DrmProperty, ref_obj: *mut DrmModeObject);
    fn drm_object_immutable_property_get_value(
        obj: *mut DrmModeObject,
        prop: *mut DrmProperty,
        val: *mut u64,
    ) -> i32;
    fn drm_mode_convert_to_umode(out: *mut DrmModeModeinfo, in_: *const DrmDisplayMode);
    fn drm_mode_convert_umode(
        dev: *mut DrmDevice,
        out: *mut DrmDisplayMode,
        in_: *const DrmModeModeinfo,
    ) -> i32;
    fn drm_mode_object_find(
        dev: *mut DrmDevice,
        file_priv: *mut DrmFile,
        id: u32,
        obj_type: u32,
    ) -> *mut DrmModeObject;
    fn drm_mode_object_put(obj: *mut DrmModeObject);
    fn drm_framebuffer_lookup(
        dev: *mut DrmDevice,
        file_priv: *mut DrmFile,
        id: u32,
    ) -> *mut DrmFramebuffer;
    fn drm_event_reserve_init(
        dev: *mut DrmDevice,
        file_priv: *mut DrmFile,
        p: *mut crate::drm_file_types::DrmPendingEvent,
        e: *mut DrmEvent,
    ) -> i32;
    fn drm_event_cancel_free(dev: *mut DrmDevice, p: *mut crate::drm_file_types::DrmPendingEvent);
    fn dma_fence_put(fence: *mut DmaFence);
    fn get_unused_fd_flags(flags: c_int) -> c_int;
    fn put_unused_fd(fd: c_int);
    fn fd_install(fd: c_int, file: *mut c_void);
    fn fput(file: *mut c_void);
    fn krealloc(p: *mut c_void, new_size: usize, flags: u32) -> *mut c_void;
}

/// # Safety
///
/// `crtc` must point to an initialized, readable `DrmCrtc`.
#[inline]
unsafe fn drm_crtc_index(crtc: *const DrmCrtc) -> usize {
    // SAFETY: `crtc` points to an initialized `DrmCrtc` passed by caller; accessing `index` is aligned and valid.
    unsafe { (*crtc).index as usize }
}

/// # Safety
///
/// `connector` must point to an initialized, readable `DrmConnector` whose `index < 32`.
#[inline]
unsafe fn drm_connector_mask(connector: *const DrmConnector) -> u32 {
    // SAFETY: `connector` points to an initialized `DrmConnector` passed by caller; `index < 32` per caller precondition.
    1u32 << unsafe { (*connector).index }
}

/// # Safety
///
/// `plane` must point to an initialized, readable `DrmPlane` whose `index < 32`.
#[inline]
unsafe fn drm_plane_mask(plane: *const DrmPlane) -> u32 {
    // SAFETY: `plane` points to an initialized `DrmPlane` passed by caller; `index < 32` per caller precondition.
    1u32 << unsafe { (*plane).index }
}

/// # Safety
///
/// If non-null, `fb` must point to a live, initialized `DrmFramebuffer`.
#[inline]
unsafe fn drm_framebuffer_get(fb: *mut DrmFramebuffer) {
    // SAFETY: if non-null, `fb` points to an active `DrmFramebuffer` with initialized `base` mode object.
    unsafe {
        if !fb.is_null() {
            crate::drm_mode_object_upstream::drm_mode_object_get(core::ptr::addr_of_mut!(
                (*fb).base
            ));
        }
    }
}

/// # Safety
///
/// If non-null, `fb` must point to a live, initialized `DrmFramebuffer` holding a reference to drop.
#[inline]
unsafe fn drm_framebuffer_put(fb: *mut DrmFramebuffer) {
    // SAFETY: if non-null, `fb` points to a live `DrmFramebuffer` holding a reference on `base` to drop.
    unsafe {
        if !fb.is_null() {
            crate::drm_mode_object_upstream::drm_mode_object_put(core::ptr::addr_of_mut!(
                (*fb).base
            ));
        }
    }
}

/// # Safety
///
/// If non-null, `connector` must point to a live, initialized `DrmConnector`.
#[inline]
unsafe fn drm_connector_get(connector: *mut DrmConnector) {
    // SAFETY: if non-null, `connector` points to an active `DrmConnector` with initialized `base` mode object.
    unsafe {
        if !connector.is_null() {
            crate::drm_mode_object_upstream::drm_mode_object_get(core::ptr::addr_of_mut!(
                (*connector).base
            ));
        }
    }
}

/// # Safety
///
/// If non-null, `connector` must point to a live, initialized `DrmConnector` holding a reference to drop.
#[inline]
unsafe fn drm_connector_put(connector: *mut DrmConnector) {
    // SAFETY: if non-null, `connector` points to a live `DrmConnector` holding a reference on `base` to drop.
    unsafe {
        if !connector.is_null() {
            crate::drm_mode_object_upstream::drm_mode_object_put(core::ptr::addr_of_mut!(
                (*connector).base
            ));
        }
    }
}

/// # Safety
///
/// `p` must be non-null, properly aligned, and valid for reads and writes of `*mut DrmFramebuffer` without
/// concurrent unsynchronized access. If non-null, `fb` and `*p` must point to live `DrmFramebuffer`s.
#[inline]
unsafe fn drm_framebuffer_assign(p: *mut *mut DrmFramebuffer, fb: *mut DrmFramebuffer) {
    // SAFETY: `p` is non-null and valid for dereference/assignment; reference to `fb` is acquired before releasing `*p`.
    unsafe {
        if !fb.is_null() {
            drm_framebuffer_get(fb);
        }
        if !(*p).is_null() {
            drm_framebuffer_put(*p);
        }
        *p = fb;
    }
}

#[inline]
fn is_power_of_2(x: u64) -> bool {
    x != 0 && (x & (x - 1)) == 0
}

/// # Safety
///
/// `obj` must point to the `base` field of an enclosing `DrmConnector` within an active allocation.
#[inline]
unsafe fn obj_to_connector(obj: *mut DrmModeObject) -> *mut DrmConnector {
    // SAFETY: `obj` points to `base` inside an enclosing `DrmConnector` allocation.
    unsafe { (obj as *mut u8).sub(core::mem::offset_of!(DrmConnector, base)) as *mut DrmConnector }
}

/// # Safety
///
/// `obj` must point to the `base` field of an enclosing `DrmCrtc` within an active allocation.
#[inline]
unsafe fn obj_to_crtc(obj: *mut DrmModeObject) -> *mut DrmCrtc {
    // SAFETY: `obj` points to `base` inside an enclosing `DrmCrtc` allocation.
    unsafe { (obj as *mut u8).sub(core::mem::offset_of!(DrmCrtc, base)) as *mut DrmCrtc }
}

/// # Safety
///
/// `obj` must point to the `base` field of an enclosing `DrmPlane` within an active allocation.
#[inline]
unsafe fn obj_to_plane(obj: *mut DrmModeObject) -> *mut DrmPlane {
    // SAFETY: `obj` points to `base` inside an enclosing `DrmPlane` allocation.
    unsafe { (obj as *mut u8).sub(core::mem::offset_of!(DrmPlane, base)) as *mut DrmPlane }
}

/// # Safety
///
/// `ptr` must be a user-space address valid for reading `size_of::<T>()` bytes. `val` must be a valid reference.
#[inline]
unsafe fn get_user<T: Copy>(val: &mut T, ptr: *const T) -> bool {
    // SAFETY: `ptr` is a user-space address safely validated and read by `__get_user`; `val` is a valid kernel reference.
    unsafe { drm_kapi::mm_native::__get_user(val, ptr) == 0 }
}

/// # Safety
///
/// `ptr` must be a user-space address valid for writing `size_of::<T>()` bytes.
#[inline]
unsafe fn put_user<T: Copy>(val: T, ptr: *mut T) -> bool {
    // SAFETY: `ptr` is a user-space address safely validated and written by `__put_user`.
    unsafe { drm_kapi::mm_native::__put_user(val, ptr) == 0 }
}

/// # Safety
///
/// `dst` must point to a writable kernel buffer of at least `size` bytes. `src` must be a user-space address
/// valid for reading `size` bytes.
#[inline]
unsafe fn copy_from_user(dst: *mut c_void, src: *const c_void, size: usize) -> bool {
    // SAFETY: `dst` is a writable kernel buffer of `size` bytes; `src` is a user address safely read by `copy_from_user`.
    unsafe { drm_kapi::mm_native::copy_from_user(dst, src, size) == 0 }
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_set_mode_for_crtc()
/// # Safety
///
/// If non-null, `state` must point to an initialized `DrmCrtcState` whose `(*state).crtc` is valid and points
/// to a `DrmCrtc` with a live `dev`. If non-null, `mode` must point to an initialized, readable `DrmDisplayMode`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_set_mode_for_crtc(
    state: *mut DrmCrtcState,
    mode: *const DrmDisplayMode,
) -> i32 {
    if state.is_null() {
        return -EINVAL;
    }
    // SAFETY: `state` was verified non-null above and points to a valid `DrmCrtcState`.
    let crtc = unsafe { (*state).crtc };

    if !mode.is_null() {
        // SAFETY: `state` points to valid `DrmCrtcState`, and `mode` is non-null and valid for `size_of::<DrmDisplayMode>()` bytes.
        let same = unsafe {
            core::slice::from_raw_parts(
                core::ptr::addr_of!((*state).mode) as *const u8,
                size_of::<DrmDisplayMode>(),
            ) == core::slice::from_raw_parts(mode as *const u8, size_of::<DrmDisplayMode>())
        };
        if same {
            return 0;
        }
    }

    // SAFETY: `state` points to valid `DrmCrtcState`; `crtc.dev` is valid for blob creation; `mode` is copied into `(*state).mode`.
    unsafe {
        if !(*state).mode_blob.is_null() {
            drm_property_blob_put((*state).mode_blob);
            (*state).mode_blob = core::ptr::null_mut();
        }

        if !mode.is_null() {
            let mut umode = core::mem::MaybeUninit::<DrmModeModeinfo>::zeroed();
            drm_mode_convert_to_umode(umode.as_mut_ptr(), mode);
            let blob = drm_property_create_blob(
                (*crtc).dev,
                size_of::<DrmModeModeinfo>(),
                umode.as_ptr() as *const c_void,
            );
            if IS_ERR(blob) {
                return PTR_ERR(blob);
            }

            core::ptr::copy_nonoverlapping(mode, core::ptr::addr_of_mut!((*state).mode), 1);
            (*state).mode_blob = blob;
            (*state).enable = true;
        } else {
            core::ptr::write_bytes(core::ptr::addr_of_mut!((*state).mode), 0, 1);
            (*state).enable = false;
        }
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_set_mode_prop_for_crtc()
/// # Safety
///
/// If non-null, `state` must point to an initialized `DrmCrtcState` whose `(*state).crtc` is valid and points
/// to a `DrmCrtc` with a live `dev`. If non-null, `blob` must point to a live `DrmPropertyBlob`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_set_mode_prop_for_crtc(
    state: *mut DrmCrtcState,
    blob: *mut DrmPropertyBlob,
) -> i32 {
    if state.is_null() {
        return -EINVAL;
    }
    // SAFETY: `state` was verified non-null above and points to a valid `DrmCrtcState`.
    let crtc = unsafe { (*state).crtc };

    // SAFETY: `state` is non-null and points to a valid `DrmCrtcState`.
    if blob == unsafe { (*state).mode_blob } {
        return 0;
    }

    // SAFETY: `state` points to valid `DrmCrtcState`; `blob.length` matches `DrmModeModeinfo` and `blob.data` is valid for conversion.
    unsafe {
        if !(*state).mode_blob.is_null() {
            drm_property_blob_put((*state).mode_blob);
            (*state).mode_blob = core::ptr::null_mut();
        }

        core::ptr::write_bytes(core::ptr::addr_of_mut!((*state).mode), 0, 1);

        if !blob.is_null() {
            if (*blob).length != size_of::<DrmModeModeinfo>() {
                return -EINVAL;
            }

            let ret = drm_mode_convert_umode(
                (*crtc).dev,
                core::ptr::addr_of_mut!((*state).mode),
                (*blob).data as *const DrmModeModeinfo,
            );
            if ret != 0 {
                return -EINVAL;
            }

            (*state).mode_blob = drm_property_blob_get(blob);
            (*state).enable = true;
        } else {
            (*state).enable = false;
        }
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_set_crtc_for_plane()
/// # Safety
///
/// If non-null, `plane_state` must point to an initialized `DrmPlaneState` associated with a valid commit `state`
/// and a live `plane`. If non-null, `crtc` must point to a live `DrmCrtc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_set_crtc_for_plane(
    plane_state: *mut DrmPlaneState,
    crtc: *mut DrmCrtc,
) -> i32 {
    if plane_state.is_null() {
        return -EINVAL;
    }
    // SAFETY: `plane_state` was verified non-null above and points to a valid `DrmPlaneState`.
    if unsafe { (*plane_state).crtc } == crtc {
        return 0;
    }

    // SAFETY: `plane_state` points to a valid `DrmPlaneState` with valid `plane`.
    let plane = unsafe { (*plane_state).plane };
    // SAFETY: `plane_state` points to valid `DrmPlaneState`; CRTC state lookups via `drm_atomic_get_crtc_state` are checked; plane index bitmask is within bounds.
    unsafe {
        if !(*plane_state).crtc.is_null() {
            let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_crtc_state(
                (*plane_state).state,
                (*plane_state).crtc,
            );
            if IS_ERR(crtc_state) {
                return PTR_ERR(crtc_state);
            }
            (*crtc_state).plane_mask &= !drm_plane_mask(plane);
        }

        (*plane_state).crtc = crtc;

        if !crtc.is_null() {
            let crtc_state =
                crate::drm_atomic_upstream::drm_atomic_get_crtc_state((*plane_state).state, crtc);
            if IS_ERR(crtc_state) {
                return PTR_ERR(crtc_state);
            }
            (*crtc_state).plane_mask |= drm_plane_mask(plane);
        }
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_set_fb_for_plane()
/// # Safety
///
/// If non-null, `plane_state` must point to an initialized `DrmPlaneState`. If non-null, `fb` must point to a
/// live `DrmFramebuffer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_set_fb_for_plane(
    plane_state: *mut DrmPlaneState,
    fb: *mut DrmFramebuffer,
) {
    if plane_state.is_null() {
        return;
    }
    // SAFETY: `plane_state` was verified non-null above; `addr_of_mut!((*plane_state).fb)` is an aligned, valid pointer slot.
    unsafe {
        drm_framebuffer_assign(core::ptr::addr_of_mut!((*plane_state).fb), fb);
    }
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_set_colorop_for_plane()
/// # Safety
///
/// Unused stub. Any non-null pointers must satisfy basic pointer alignment and validity.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_set_colorop_for_plane(
    _plane_state: *mut DrmPlaneState,
    _colorop: *mut c_void,
) -> bool {
    false
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_set_crtc_for_connector()
/// # Safety
///
/// If non-null, `conn_state` must point to an initialized `DrmConnectorState` with valid `connector` and `state`.
/// If non-null, `crtc` must point to a live `DrmCrtc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_set_crtc_for_connector(
    conn_state: *mut DrmConnectorState,
    crtc: *mut DrmCrtc,
) -> i32 {
    if conn_state.is_null() {
        return -EINVAL;
    }
    // SAFETY: `conn_state` was verified non-null above and points to a valid `DrmConnectorState`.
    if unsafe { (*conn_state).crtc } == crtc {
        return 0;
    }

    // SAFETY: `conn_state` points to a valid `DrmConnectorState` with valid `connector`.
    let connector = unsafe { (*conn_state).connector };

    // SAFETY: `conn_state` points to valid `DrmConnectorState`; CRTC state lookups via `drm_atomic_get_*_state` are checked; connector references are balanced.
    unsafe {
        if !(*conn_state).crtc.is_null() {
            let crtc_state = crate::drm_atomic_upstream::drm_atomic_get_new_crtc_state(
                (*conn_state).state,
                (*conn_state).crtc,
            );
            if !crtc_state.is_null() {
                (*crtc_state).connector_mask &= !drm_connector_mask(connector);
            }
            drm_connector_put(connector);
            (*conn_state).crtc = core::ptr::null_mut();
        }

        if !crtc.is_null() {
            let crtc_state =
                crate::drm_atomic_upstream::drm_atomic_get_crtc_state((*conn_state).state, crtc);
            if IS_ERR(crtc_state) {
                let ret = PTR_ERR(crtc_state);
                return ret;
            }

            (*crtc_state).connector_mask |= drm_connector_mask(connector);
            drm_connector_get(connector);
            (*conn_state).crtc = crtc;
        }
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c set_out_fence_for_crtc()
/// # Safety
///
/// `state` must point to an initialized `DrmAtomicCommit` whose `crtcs` array contains at least
/// `drm_crtc_index(crtc) + 1` entries. `crtc` must point to a live `DrmCrtc`.
unsafe fn set_out_fence_for_crtc(
    state: *mut DrmAtomicCommit,
    crtc: *mut DrmCrtc,
    fence_ptr: *mut i32,
) {
    // SAFETY: `crtc` points to an initialized `DrmCrtc` guaranteed by caller.
    let index = unsafe { drm_crtc_index(crtc) };
    // SAFETY: `state.crtcs` is allocated with enough entries to index `crtc` by `index` per caller contract.
    unsafe {
        (*(*state).crtcs.add(index)).out_fence_ptr = fence_ptr;
    }
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c get_out_fence_for_crtc()
/// # Safety
///
/// `state` must point to an initialized `DrmAtomicCommit` whose `crtcs` array contains at least
/// `drm_crtc_index(crtc) + 1` entries. `crtc` must point to a live `DrmCrtc`.
unsafe fn get_out_fence_for_crtc(state: *mut DrmAtomicCommit, crtc: *mut DrmCrtc) -> *mut i32 {
    // SAFETY: `crtc` points to an initialized `DrmCrtc` guaranteed by caller.
    let index = unsafe { drm_crtc_index(crtc) };
    // SAFETY: `state.crtcs` is allocated with enough entries to index `crtc` by `index` per caller contract.
    unsafe {
        let fence_ptr = (*(*state).crtcs.add(index)).out_fence_ptr;
        (*(*state).crtcs.add(index)).out_fence_ptr = core::ptr::null_mut();
        fence_ptr
    }
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_crtc_set_property()
/// # Safety
///
/// `crtc` must point to an initialized, live `DrmCrtc` with a valid `dev`. `state` must point to an initialized,
/// mutable `DrmCrtcState` associated with `crtc`. `property` must point to an initialized `DrmProperty`.
unsafe fn drm_atomic_crtc_set_property(
    crtc: *mut DrmCrtc,
    state: *mut DrmCrtcState,
    property: *mut DrmProperty,
    val: u64,
) -> i32 {
    // SAFETY: `crtc` points to an active DRM structure passed by caller; `dev` field is non-null and valid.
    let dev = unsafe { (*crtc).dev };
    // SAFETY: `dev` points to an initialized `DrmDevice` containing embedded `mode_config`.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };
    let mut replaced = false;

    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    if property == unsafe { (*config).prop_active } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `active` write is aligned and within bounds.
        unsafe { (*state).active = val != 0 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_mode_id } {
        // SAFETY: `dev` points to live `DrmDevice`; looks up blob property by ID.
        let mode = unsafe { drm_property_lookup_blob(dev, val as u32) };
        // SAFETY: `state` points to mutable `DrmCrtcState`; `mode` is the looked-up blob (or null).
        let ret = unsafe { drm_atomic_set_mode_prop_for_crtc(state, mode) };
        // SAFETY: `mode` was returned by `drm_property_lookup_blob` and holds an acquired reference to drop.
        unsafe {
            drm_property_blob_put(mode);
        }
        return ret;
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_vrr_enabled } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `vrr_enabled` write is aligned and within bounds.
        unsafe { (*state).vrr_enabled = val != 0 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).degamma_lut_property } {
        let elem_size = size_of::<DrmColorLut>();
        let mut lut_size: u64 = 0;
        // SAFETY: `crtc` points to live `DrmCrtc` with valid `base` mode object; out pointer reference is aligned and writable.
        let ret = unsafe {
            drm_object_immutable_property_get_value(
                core::ptr::addr_of_mut!((*crtc).base),
                (*config).degamma_lut_size_property,
                &mut lut_size,
            )
        };
        if ret != 0 {
            return ret;
        }

        // SAFETY: `dev` points to live `DrmDevice`; destination blob pointer in `state` is valid and aligned.
        let ret = unsafe {
            drm_property_replace_blob_from_id(
                dev,
                core::ptr::addr_of_mut!((*state).degamma_lut),
                val,
                (elem_size * lut_size as usize) as isize,
                elem_size as isize,
                &mut replaced,
            )
        };
        // SAFETY: `state` points to mutable `DrmCrtcState`; bitflag accessor and mutator operate on valid state.
        unsafe { (*state).set_color_mgmt_changed((*state).color_mgmt_changed() || replaced) };
        return ret;
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).ctm_property } {
        // SAFETY: `dev` points to live `DrmDevice`; destination blob pointer in `state` is valid and aligned.
        let ret = unsafe {
            drm_property_replace_blob_from_id(
                dev,
                core::ptr::addr_of_mut!((*state).ctm),
                val,
                40, // sizeof(struct drm_color_ctm)
                -1,
                &mut replaced,
            )
        };
        // SAFETY: `state` points to mutable `DrmCrtcState`; bitflag accessor and mutator operate on valid state.
        unsafe { (*state).set_color_mgmt_changed((*state).color_mgmt_changed() || replaced) };
        return ret;
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).gamma_lut_property } {
        let elem_size = size_of::<DrmColorLut>();
        let mut lut_size: u64 = 0;
        // SAFETY: `crtc` points to live `DrmCrtc` with valid `base` mode object; out pointer reference is aligned and writable.
        let ret = unsafe {
            drm_object_immutable_property_get_value(
                core::ptr::addr_of_mut!((*crtc).base),
                (*config).gamma_lut_size_property,
                &mut lut_size,
            )
        };
        if ret != 0 {
            return ret;
        }

        // SAFETY: `dev` points to live `DrmDevice`; destination blob pointer in `state` is valid and aligned.
        let ret = unsafe {
            drm_property_replace_blob_from_id(
                dev,
                core::ptr::addr_of_mut!((*state).gamma_lut),
                val,
                (elem_size * lut_size as usize) as isize,
                elem_size as isize,
                &mut replaced,
            )
        };
        // SAFETY: `state` points to mutable `DrmCrtcState`; bitflag accessor and mutator operate on valid state.
        unsafe { (*state).set_color_mgmt_changed((*state).color_mgmt_changed() || replaced) };
        return ret;
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).background_color_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `background_color` write is aligned and within bounds.
        unsafe { (*state).background_color = val };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_out_fence_ptr } {
        let fence_ptr = val as *mut i32;
        if fence_ptr.is_null() {
            return 0;
        }
        // SAFETY: `fence_ptr` is a user-space address validated by `put_user`.
        if !unsafe { put_user(-1i32, fence_ptr) } {
            return -EFAULT;
        }
        // SAFETY: `(*state).state` is the enclosing `DrmAtomicCommit`; `crtc` and `fence_ptr` are valid.
        unsafe { set_out_fence_for_crtc((*state).state, crtc, fence_ptr) };
    // SAFETY: `crtc` points to live object; reading `scaling_filter_property` is aligned and valid.
    } else if property == unsafe { (*crtc).scaling_filter_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `scaling_filter` write is aligned and within bounds.
        unsafe { (*state).scaling_filter = val as u32 };
    } else {
        // SAFETY: `crtc` points to live object; reading `funcs` table pointer is aligned and valid.
        let funcs = unsafe { (*crtc).funcs };
        if !funcs.is_null()
            // SAFETY: `funcs` was verified non-null above; reading the function pointer is aligned and valid.
            && let Some(set_fn) = unsafe { (*funcs).atomic_set_property }
        {
            // SAFETY: invokes driver-provided atomic property callback with caller-validated object and state pointers.
            return unsafe { set_fn(crtc, state, property, val) };
        }
        return -EINVAL;
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_crtc_get_property()
/// # Safety
///
/// `crtc` must point to an initialized, live `DrmCrtc` with a valid `dev`. `state` must point to an initialized
/// `DrmCrtcState`. `property` must point to an initialized `DrmProperty`. `val` must be aligned and writable for `u64`.
unsafe fn drm_atomic_crtc_get_property(
    crtc: *mut DrmCrtc,
    state: *const DrmCrtcState,
    property: *mut DrmProperty,
    val: *mut u64,
) -> i32 {
    // SAFETY: `crtc` points to an active DRM structure passed by caller; `dev` field is non-null and valid.
    let dev = unsafe { (*crtc).dev };
    // SAFETY: `dev` points to an initialized `DrmDevice` containing embedded `mode_config`.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };

    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    if property == unsafe { (*config).prop_active } {
        // SAFETY: `state` points to valid `DrmCrtcState`; `val` is aligned and writable for `u64`.
        unsafe {
            *val =
                crate::drm_atomic_state_helper_upstream::drm_atomic_crtc_effectively_active(state)
                    as u64
        };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_mode_id } {
        // SAFETY: `state` points to valid state; if blob pointer is non-null, `base.id` is aligned and valid.
        unsafe {
            *val = if !(*state).mode_blob.is_null() {
                (*(*state).mode_blob).base.id as u64
            } else {
                0
            }
        };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_vrr_enabled } {
        // SAFETY: `state` points to valid state; reading `vrr_enabled` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).vrr_enabled as u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).degamma_lut_property } {
        // SAFETY: `state` points to valid state; if blob pointer is non-null, `base.id` is aligned and valid.
        unsafe {
            *val = if !(*state).degamma_lut.is_null() {
                (*(*state).degamma_lut).base.id as u64
            } else {
                0
            }
        };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).ctm_property } {
        // SAFETY: `state` points to valid state; if blob pointer is non-null, `base.id` is aligned and valid.
        unsafe {
            *val = if !(*state).ctm.is_null() {
                (*(*state).ctm).base.id as u64
            } else {
                0
            }
        };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).gamma_lut_property } {
        // SAFETY: `state` points to valid state; if blob pointer is non-null, `base.id` is aligned and valid.
        unsafe {
            *val = if !(*state).gamma_lut.is_null() {
                (*(*state).gamma_lut).base.id as u64
            } else {
                0
            }
        };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).background_color_property } {
        // SAFETY: `state` points to valid state; reading `background_color` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).background_color };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_out_fence_ptr } {
        // SAFETY: caller guarantees `val` is aligned and writable for `u64`; state field access is valid.
        unsafe { *val = 0 };
    // SAFETY: `crtc` points to live object; reading `scaling_filter_property` is aligned and valid.
    } else if property == unsafe { (*crtc).scaling_filter_property } {
        // SAFETY: `state` points to valid state; reading `scaling_filter` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).scaling_filter as u64 };
    } else {
        // SAFETY: `crtc` points to live object; reading `funcs` table pointer is aligned and valid.
        let funcs = unsafe { (*crtc).funcs };
        if !funcs.is_null()
            // SAFETY: `funcs` was verified non-null above; reading the function pointer is aligned and valid.
            && let Some(get_fn) = unsafe { (*funcs).atomic_get_property }
        {
            // SAFETY: invokes driver-provided atomic property callback with caller-validated object and state pointers.
            return unsafe { get_fn(crtc, state as *mut DrmCrtcState, property, val) };
        }
        return -EINVAL;
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_plane_set_property()
/// # Safety
///
/// `plane` must point to an initialized, live `DrmPlane` with a valid `dev`. `state` must point to an initialized,
/// mutable `DrmPlaneState`. `property` must point to an initialized `DrmProperty`. `file_priv` can be null or a valid `DrmFile`.
unsafe fn drm_atomic_plane_set_property(
    plane: *mut DrmPlane,
    state: *mut DrmPlaneState,
    file_priv: *mut DrmFile,
    property: *mut DrmProperty,
    val: u64,
) -> i32 {
    // SAFETY: `plane` points to an active DRM structure passed by caller; `dev` field is non-null and valid.
    let dev = unsafe { (*plane).dev };
    // SAFETY: `dev` points to an initialized `DrmDevice` containing embedded `mode_config`.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };
    let mut replaced = false;

    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    if property == unsafe { (*config).prop_fb_id } {
        // SAFETY: `dev` and `file_priv` are live pointers; looks up framebuffer by user-provided ID.
        let fb = unsafe { drm_framebuffer_lookup(dev, file_priv, val as u32) };
        // SAFETY: `state` is mutable `DrmPlaneState`; `fb` is the looked-up framebuffer (or null).
        unsafe {
            drm_atomic_set_fb_for_plane(state, fb);
        }
        if !fb.is_null() {
            // SAFETY: `fb` was returned by `drm_framebuffer_lookup` and holds an acquired reference to drop.
            unsafe {
                drm_framebuffer_put(fb);
            }
        }
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_in_fence_fd } {
        // SAFETY: `state` points to mutable `DrmPlaneState`; reading `fence` is aligned and within bounds.
        if !unsafe { (*state).fence.is_null() } {
            return -EINVAL;
        }
        if val as i64 == -1 {
            return 0;
        }
        // SAFETY: `state` points to mutable `DrmPlaneState`; `sync_file_get_fence` retrieves fence reference for given fd.
        unsafe {
            (*state).fence = sync_file_get_fence(val as i32);
            if (*state).fence.is_null() {
                return -EINVAL;
            }
        }
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_crtc_id } {
        let crtc = if val != 0 {
            // SAFETY: `dev` and `file_priv` are live pointers; finds CRTC by ID.
            let c = unsafe { crate::drm_crtc_upstream::drm_crtc_find(dev, file_priv, val as u32) };
            if c.is_null() {
                return -EACCES;
            }
            c
        } else {
            core::ptr::null_mut()
        };
        // SAFETY: `state` points to mutable `DrmPlaneState`; sets associated `crtc` and updates CRTC masks.
        return unsafe { drm_atomic_set_crtc_for_plane(state, crtc) };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_crtc_x } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `crtc_x` write is aligned and within bounds.
        unsafe { (*state).crtc_x = val as i64 as i32 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_crtc_y } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `crtc_y` write is aligned and within bounds.
        unsafe { (*state).crtc_y = val as i64 as i32 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_crtc_w } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `crtc_w` write is aligned and within bounds.
        unsafe { (*state).crtc_w = val as u32 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_crtc_h } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `crtc_h` write is aligned and within bounds.
        unsafe { (*state).crtc_h = val as u32 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_src_x } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `src_x` write is aligned and within bounds.
        unsafe { (*state).src_x = val as u32 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_src_y } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `src_y` write is aligned and within bounds.
        unsafe { (*state).src_y = val as u32 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_src_w } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `src_w` write is aligned and within bounds.
        unsafe { (*state).src_w = val as u32 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_src_h } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `src_h` write is aligned and within bounds.
        unsafe { (*state).src_h = val as u32 };
    // SAFETY: `plane` points to live object; reading `alpha_property` is aligned and valid.
    } else if property == unsafe { (*plane).alpha_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `alpha` write is aligned and within bounds.
        unsafe { (*state).alpha = val as u16 };
    // SAFETY: `plane` points to live object; reading `blend_mode_property` is aligned and valid.
    } else if property == unsafe { (*plane).blend_mode_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `pixel_blend_mode` write is aligned and within bounds.
        unsafe { (*state).pixel_blend_mode = val as u16 };
    // SAFETY: `plane` points to live object; reading `rotation_property` is aligned and valid.
    } else if property == unsafe { (*plane).rotation_property } {
        if !is_power_of_2(val & DRM_MODE_ROTATE_MASK as u64) {
            return -EINVAL;
        }
        // SAFETY: `state` points to caller-supplied mutable atomic state; `rotation` write is aligned and within bounds.
        unsafe { (*state).rotation = val as u32 };
    // SAFETY: `plane` points to live object; reading `zpos_property` is aligned and valid.
    } else if property == unsafe { (*plane).zpos_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `zpos` write is aligned and within bounds.
        unsafe { (*state).zpos = val as u32 };
    // SAFETY: `plane` points to live object; reading `color_encoding_property` is aligned and valid.
    } else if property == unsafe { (*plane).color_encoding_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `color_encoding` write is aligned and within bounds.
        unsafe { (*state).color_encoding = val as u32 };
    // SAFETY: `plane` points to live object; reading `color_range_property` is aligned and valid.
    } else if property == unsafe { (*plane).color_range_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `color_range` write is aligned and within bounds.
        unsafe { (*state).color_range = val as u32 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_fb_damage_clips } {
        // SAFETY: `dev` points to live `DrmDevice`; destination blob pointer in `state` is valid and aligned.
        let ret = unsafe {
            drm_property_replace_blob_from_id(
                dev,
                core::ptr::addr_of_mut!((*state).fb_damage_clips),
                val,
                -1,
                size_of::<DrmModeRect>() as isize,
                &mut replaced,
            )
        };
        return ret;
    // SAFETY: `plane` points to live object; reading `scaling_filter_property` is aligned and valid.
    } else if property == unsafe { (*plane).scaling_filter_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `scaling_filter` write is aligned and within bounds.
        unsafe { (*state).scaling_filter = val as u32 };
    // SAFETY: `plane` points to live object; reading `hotspot_x_property` is aligned and valid.
    } else if property == unsafe { (*plane).hotspot_x_property } {
        // SAFETY: `plane` points to live `DrmPlane`; reading `r#type` is aligned and valid.
        if unsafe { (*plane).r#type != DRM_PLANE_TYPE_CURSOR } {
            return -EINVAL;
        }
        // SAFETY: `state` points to caller-supplied mutable atomic state; `hotspot_x` write is aligned and within bounds.
        unsafe { (*state).hotspot_x = val as i32 };
    // SAFETY: `plane` points to live object; reading `hotspot_y_property` is aligned and valid.
    } else if property == unsafe { (*plane).hotspot_y_property } {
        // SAFETY: `plane` points to live `DrmPlane`; reading `r#type` is aligned and valid.
        if unsafe { (*plane).r#type != DRM_PLANE_TYPE_CURSOR } {
            return -EINVAL;
        }
        // SAFETY: `state` points to caller-supplied mutable atomic state; `hotspot_y` write is aligned and within bounds.
        unsafe { (*state).hotspot_y = val as i32 };
    } else {
        // SAFETY: `plane` points to live object; reading `funcs` table pointer is aligned and valid.
        let funcs = unsafe { (*plane).funcs };
        if !funcs.is_null()
            // SAFETY: `funcs` was verified non-null above; reading the function pointer is aligned and valid.
            && let Some(set_fn) = unsafe { (*funcs).atomic_set_property }
        {
            // SAFETY: invokes driver-provided atomic property callback with caller-validated object and state pointers.
            return unsafe { set_fn(plane, state, property, val) };
        }
        return -EINVAL;
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_plane_get_property()
/// # Safety
///
/// `plane` must point to an initialized, live `DrmPlane` with a valid `dev`. `state` must point to an initialized
/// `DrmPlaneState`. `property` must point to an initialized `DrmProperty`. `val` must be aligned and writable for `u64`.
unsafe fn drm_atomic_plane_get_property(
    plane: *mut DrmPlane,
    state: *const DrmPlaneState,
    property: *mut DrmProperty,
    val: *mut u64,
) -> i32 {
    // SAFETY: `plane` points to an active DRM structure passed by caller; `dev` field is non-null and valid.
    let dev = unsafe { (*plane).dev };
    // SAFETY: `dev` points to an initialized `DrmDevice` containing embedded `mode_config`.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };

    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    if property == unsafe { (*config).prop_fb_id } {
        // SAFETY: `state` points to valid state; if blob pointer is non-null, `base.id` is aligned and valid.
        unsafe {
            *val = if !(*state).fb.is_null() {
                (*(*state).fb).base.id as u64
            } else {
                0
            }
        };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_in_fence_fd } {
        // SAFETY: caller guarantees `val` is aligned and writable for `u64`; state field access is valid.
        unsafe { *val = !0u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_crtc_id } {
        // SAFETY: `state` points to valid state; if blob pointer is non-null, `base.id` is aligned and valid.
        unsafe {
            *val = if !(*state).crtc.is_null() {
                (*(*state).crtc).base.id as u64
            } else {
                0
            }
        };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_crtc_x } {
        // SAFETY: `state` points to valid state; reading `crtc_x` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).crtc_x as i64 as u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_crtc_y } {
        // SAFETY: `state` points to valid state; reading `crtc_y` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).crtc_y as i64 as u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_crtc_w } {
        // SAFETY: `state` points to valid state; reading `crtc_w` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).crtc_w as u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_crtc_h } {
        // SAFETY: `state` points to valid state; reading `crtc_h` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).crtc_h as u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_src_x } {
        // SAFETY: `state` points to valid state; reading `src_x` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).src_x as u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_src_y } {
        // SAFETY: `state` points to valid state; reading `src_y` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).src_y as u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_src_w } {
        // SAFETY: `state` points to valid state; reading `src_w` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).src_w as u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_src_h } {
        // SAFETY: `state` points to valid state; reading `src_h` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).src_h as u64 };
    // SAFETY: `plane` points to live object; reading `alpha_property` is aligned and valid.
    } else if property == unsafe { (*plane).alpha_property } {
        // SAFETY: `state` points to valid state; reading `alpha` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).alpha as u64 };
    // SAFETY: `plane` points to live object; reading `blend_mode_property` is aligned and valid.
    } else if property == unsafe { (*plane).blend_mode_property } {
        // SAFETY: `state` points to valid state; reading `pixel_blend_mode` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).pixel_blend_mode as u64 };
    // SAFETY: `plane` points to live object; reading `rotation_property` is aligned and valid.
    } else if property == unsafe { (*plane).rotation_property } {
        // SAFETY: `state` points to valid state; reading `rotation` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).rotation as u64 };
    // SAFETY: `plane` points to live object; reading `zpos_property` is aligned and valid.
    } else if property == unsafe { (*plane).zpos_property } {
        // SAFETY: `state` points to valid state; reading `zpos` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).zpos as u64 };
    // SAFETY: `plane` points to live object; reading `color_encoding_property` is aligned and valid.
    } else if property == unsafe { (*plane).color_encoding_property } {
        // SAFETY: `state` points to valid state; reading `color_encoding` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).color_encoding as u64 };
    // SAFETY: `plane` points to live object; reading `color_range_property` is aligned and valid.
    } else if property == unsafe { (*plane).color_range_property } {
        // SAFETY: `state` points to valid state; reading `color_range` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).color_range as u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).prop_fb_damage_clips } {
        // SAFETY: `state` points to valid state; if blob pointer is non-null, `base.id` is aligned and valid.
        unsafe {
            *val = if !(*state).fb_damage_clips.is_null() {
                (*(*state).fb_damage_clips).base.id as u64
            } else {
                0
            }
        };
    // SAFETY: `plane` points to live object; reading `scaling_filter_property` is aligned and valid.
    } else if property == unsafe { (*plane).scaling_filter_property } {
        // SAFETY: `state` points to valid state; reading `scaling_filter` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).scaling_filter as u64 };
    // SAFETY: `plane` points to live object; reading `hotspot_x_property` is aligned and valid.
    } else if property == unsafe { (*plane).hotspot_x_property } {
        // SAFETY: `state` points to valid state; reading `hotspot_x` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).hotspot_x as i64 as u64 };
    // SAFETY: `plane` points to live object; reading `hotspot_y_property` is aligned and valid.
    } else if property == unsafe { (*plane).hotspot_y_property } {
        // SAFETY: `state` points to valid state; reading `hotspot_y` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).hotspot_y as i64 as u64 };
    } else {
        // SAFETY: `plane` points to live object; reading `funcs` table pointer is aligned and valid.
        let funcs = unsafe { (*plane).funcs };
        if !funcs.is_null()
            // SAFETY: `funcs` was verified non-null above; reading the function pointer is aligned and valid.
            && let Some(get_fn) = unsafe { (*funcs).atomic_get_property }
        {
            // SAFETY: invokes driver-provided atomic property callback with caller-validated object and state pointers.
            return unsafe { get_fn(plane, state as *mut DrmPlaneState, property, val) };
        }
        return -EINVAL;
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_connector_set_property()
/// # Safety
///
/// `connector` must point to an initialized, live `DrmConnector` with a valid `dev`. `state` must point to an initialized,
/// mutable `DrmConnectorState`. `property` must point to an initialized `DrmProperty`. `file_priv` can be null or a valid `DrmFile`.
unsafe fn drm_atomic_connector_set_property(
    connector: *mut DrmConnector,
    state: *mut DrmConnectorState,
    file_priv: *mut DrmFile,
    property: *mut DrmProperty,
    val: u64,
) -> i32 {
    // SAFETY: `connector` points to an active DRM structure passed by caller; `dev` field is non-null and valid.
    let dev = unsafe { (*connector).dev };
    // SAFETY: `dev` points to an initialized `DrmDevice` containing embedded `mode_config`.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };
    let mut replaced = false;

    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    if property == unsafe { (*config).prop_crtc_id } {
        let crtc = if val != 0 {
            // SAFETY: `dev` and `file_priv` are live pointers; finds CRTC by ID.
            let c = unsafe { crate::drm_crtc_upstream::drm_crtc_find(dev, file_priv, val as u32) };
            if c.is_null() {
                return -EACCES;
            }
            c
        } else {
            core::ptr::null_mut()
        };
        // SAFETY: `state` points to mutable `DrmConnectorState`; sets associated `crtc` and updates masks/refcounts.
        let ret = unsafe { drm_atomic_set_crtc_for_connector(state, crtc) };
        return ret;
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).dpms_property } {
        return -EINVAL;
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).link_status_property } {
        // SAFETY: `state` points to mutable `DrmConnectorState; reading `link_status` is aligned and within bounds.
        if unsafe { (*state).link_status != DRM_LINK_STATUS_GOOD } {
            // SAFETY: `state` points to caller-supplied mutable atomic state; `link_status` write is aligned and within bounds.
            unsafe { (*state).link_status = val as u32 };
        }
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).hdr_output_metadata_property } {
        // SAFETY: `dev` points to live `DrmDevice`; destination blob pointer in `state` is valid and aligned.
        let ret = unsafe {
            drm_property_replace_blob_from_id(
                dev,
                core::ptr::addr_of_mut!((*state).hdr_output_metadata),
                val,
                -1,
                -1,
                &mut replaced,
            )
        };
        return ret;
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).aspect_ratio_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `picture_aspect_ratio` write is aligned and within bounds.
        unsafe { (*state).picture_aspect_ratio = val as u32 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).content_type_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `content_type` write is aligned and within bounds.
        unsafe { (*state).content_type = val as u32 };
    // SAFETY: `connector` points to live object; reading `scaling_mode_property` is aligned and valid.
    } else if property == unsafe { (*connector).scaling_mode_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `scaling_mode` write is aligned and within bounds.
        unsafe { (*state).scaling_mode = val as u32 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).content_protection_property } {
        if val == DRM_MODE_CONTENT_PROTECTION_ENABLED as u64 {
            return -EINVAL;
        }
        // SAFETY: `state` points to caller-supplied mutable atomic state; `content_protection` write is aligned and within bounds.
        unsafe { (*state).content_protection = val as u32 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).hdcp_content_type_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `hdcp_content_type` write is aligned and within bounds.
        unsafe { (*state).hdcp_content_type = val as u32 };
    // SAFETY: `connector` points to live object; reading `colorspace_property` is aligned and valid.
    } else if property == unsafe { (*connector).colorspace_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `colorspace` write is aligned and within bounds.
        unsafe { (*state).colorspace = val as u32 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).writeback_fb_id_property }
        // SAFETY: `config` points to live `mode_config` embedded within `dev`.
        || property == unsafe { (*config).writeback_out_fence_ptr_property }
    {
        return -EOPNOTSUPP;
    // SAFETY: `connector` points to live object; reading `max_bpc_property` is aligned and valid.
    } else if property == unsafe { (*connector).max_bpc_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `max_requested_bpc` write is aligned and within bounds.
        unsafe { (*state).max_requested_bpc = val as u8 };
    // SAFETY: `connector` points to live object; reading `privacy_screen_sw_state_property` is aligned and valid.
    } else if property == unsafe { (*connector).privacy_screen_sw_state_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `privacy_screen_sw_state` write is aligned and within bounds.
        unsafe { (*state).privacy_screen_sw_state = val as u32 };
    // SAFETY: `connector` points to live object; reading `broadcast_rgb_property` is aligned and valid.
    } else if property == unsafe { (*connector).broadcast_rgb_property } {
        // SAFETY: `state` points to caller-supplied mutable atomic state; `hdmi.broadcast_rgb` write is aligned and within bounds.
        unsafe { (*state).hdmi.broadcast_rgb = val as u32 };
    } else {
        // SAFETY: `connector` points to live object; reading `funcs` table pointer is aligned and valid.
        let funcs = unsafe { (*connector).funcs };
        if !funcs.is_null()
            // SAFETY: `funcs` was verified non-null above; reading the function pointer is aligned and valid.
            && let Some(set_fn) = unsafe { (*funcs).atomic_set_property }
        {
            // SAFETY: invokes driver-provided atomic property callback with caller-validated object and state pointers.
            return unsafe { set_fn(connector, state, property, val) };
        }
        return -EINVAL;
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_connector_get_property()
/// # Safety
///
/// `connector` must point to an initialized, live `DrmConnector` with a valid `dev`. `state` must point to an initialized
/// `DrmConnectorState`. `property` must point to an initialized `DrmProperty`. `val` must be aligned and writable for `u64`.
unsafe fn drm_atomic_connector_get_property(
    connector: *mut DrmConnector,
    state: *const DrmConnectorState,
    property: *mut DrmProperty,
    val: *mut u64,
) -> i32 {
    // SAFETY: `connector` points to an active DRM structure passed by caller; `dev` field is non-null and valid.
    let dev = unsafe { (*connector).dev };
    // SAFETY: `dev` points to an initialized `DrmDevice` containing embedded `mode_config`.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };

    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    if property == unsafe { (*config).prop_crtc_id } {
        // SAFETY: `state` points to valid state; if blob pointer is non-null, `base.id` is aligned and valid.
        unsafe {
            *val = if !(*state).crtc.is_null() {
                (*(*state).crtc).base.id as u64
            } else {
                0
            }
        };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).dpms_property } {
        // SAFETY: `connector` and `state` are valid; traverses CRTC and its state if non-null, or reads `connector.dpms`.
        unsafe {
            if !(*state).crtc.is_null()
                && !(*(*state).crtc).state.is_null()
                && (*(*(*state).crtc).state).self_refresh_active
            {
                *val = DRM_MODE_DPMS_ON as u64;
            } else {
                *val = (*connector).dpms as u64;
            }
        }
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).link_status_property } {
        // SAFETY: `state` points to valid state; reading `link_status` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).link_status as u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).aspect_ratio_property } {
        // SAFETY: `state` points to valid state; reading `picture_aspect_ratio` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).picture_aspect_ratio as u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).content_type_property } {
        // SAFETY: `state` points to valid state; reading `content_type` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).content_type as u64 };
    // SAFETY: `connector` points to live object; reading `colorspace_property` is aligned and valid.
    } else if property == unsafe { (*connector).colorspace_property } {
        // SAFETY: `state` points to valid state; reading `colorspace` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).colorspace as u64 };
    // SAFETY: `connector` points to live object; reading `scaling_mode_property` is aligned and valid.
    } else if property == unsafe { (*connector).scaling_mode_property } {
        // SAFETY: `state` points to valid state; reading `scaling_mode` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).scaling_mode as u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).hdr_output_metadata_property } {
        // SAFETY: `state` points to valid state; if blob pointer is non-null, `base.id` is aligned and valid.
        unsafe {
            *val = if !(*state).hdr_output_metadata.is_null() {
                (*(*state).hdr_output_metadata).base.id as u64
            } else {
                0
            }
        };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).content_protection_property } {
        // SAFETY: `state` points to valid state; reading `content_protection` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).content_protection as u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).hdcp_content_type_property } {
        // SAFETY: `state` points to valid state; reading `hdcp_content_type` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).hdcp_content_type as u64 };
    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
    } else if property == unsafe { (*config).writeback_fb_id_property }
        // SAFETY: `config` points to live `mode_config` embedded within `dev`.
        || property == unsafe { (*config).writeback_out_fence_ptr_property }
    {
        return -EOPNOTSUPP;
    // SAFETY: `connector` points to live object; reading `max_bpc_property` is aligned and valid.
    } else if property == unsafe { (*connector).max_bpc_property } {
        // SAFETY: `state` points to valid state; reading `max_requested_bpc` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).max_requested_bpc as u64 };
    // SAFETY: `connector` points to live object; reading `privacy_screen_sw_state_property` is aligned and valid.
    } else if property == unsafe { (*connector).privacy_screen_sw_state_property } {
        // SAFETY: `state` points to valid state; reading `privacy_screen_sw_state` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).privacy_screen_sw_state as u64 };
    // SAFETY: `connector` points to live object; reading `broadcast_rgb_property` is aligned and valid.
    } else if property == unsafe { (*connector).broadcast_rgb_property } {
        // SAFETY: `state` points to valid state; reading `hdmi.broadcast_rgb` and writing to caller-supplied aligned `val` is valid.
        unsafe { *val = (*state).hdmi.broadcast_rgb as u64 };
    } else {
        // SAFETY: `connector` points to live object; reading `funcs` table pointer is aligned and valid.
        let funcs = unsafe { (*connector).funcs };
        if !funcs.is_null()
            // SAFETY: `funcs` was verified non-null above; reading the function pointer is aligned and valid.
            && let Some(get_fn) = unsafe { (*funcs).atomic_get_property }
        {
            // SAFETY: invokes driver-provided atomic property callback with caller-validated object and state pointers.
            return unsafe { get_fn(connector, state as *mut DrmConnectorState, property, val) };
        }
        return -EINVAL;
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_get_property()
/// # Safety
///
/// If non-null, `obj` must point to an initialized `DrmModeObject` of connector, CRTC, or plane type embedded in the
/// corresponding DRM struct with an initialized `.state`. `property` must point to an initialized `DrmProperty`.
/// `val` must be aligned and writable for `u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_get_property(
    obj: *mut DrmModeObject,
    property: *mut DrmProperty,
    val: *mut u64,
) -> i32 {
    if obj.is_null() || property.is_null() || val.is_null() {
        return -EINVAL;
    }

    // SAFETY: `obj` was verified non-null above and points to an initialized `DrmModeObject`.
    match unsafe { (*obj).r#type } {
        DRM_MODE_OBJECT_CONNECTOR => {
            // SAFETY: `obj.type` was verified as `DRM_MODE_OBJECT_CONNECTOR`; embedded within `DrmConnector`.
            let connector = unsafe { obj_to_connector(obj) };
            // SAFETY: `connector` points to live `DrmConnector`; `connector.state` is valid atomic state per caller contract; `val` is writable.
            unsafe {
                drm_atomic_connector_get_property(connector, (*connector).state, property, val)
            }
        }
        DRM_MODE_OBJECT_CRTC => {
            // SAFETY: `obj.type` was verified as `DRM_MODE_OBJECT_CRTC`; embedded within `DrmCrtc`.
            let crtc = unsafe { obj_to_crtc(obj) };
            // SAFETY: `crtc` points to live `DrmCrtc`; `crtc.state` is valid atomic state per caller contract; `val` is writable.
            unsafe { drm_atomic_crtc_get_property(crtc, (*crtc).state, property, val) }
        }
        DRM_MODE_OBJECT_PLANE => {
            // SAFETY: `obj.type` was verified as `DRM_MODE_OBJECT_PLANE`; embedded within `DrmPlane`.
            let plane = unsafe { obj_to_plane(obj) };
            // SAFETY: `plane` points to live `DrmPlane`; `plane.state` is valid atomic state per caller contract; `val` is writable.
            unsafe { drm_atomic_plane_get_property(plane, (*plane).state, property, val) }
        }
        _ => -EINVAL,
    }
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c create_vblank_event()
/// # Safety
///
/// `crtc` must point to an initialized, live `DrmCrtc`. The caller takes ownership of the returned
/// `DrmPendingVblankEvent` allocation (if non-null) and must manage its lifecycle or free it.
unsafe fn create_vblank_event(crtc: *mut DrmCrtc, user_data: u64) -> *mut DrmPendingVblankEvent {
    let e = kzalloc(size_of::<DrmPendingVblankEvent>(), GFP_KERNEL) as *mut DrmPendingVblankEvent;
    if e.is_null() {
        return core::ptr::null_mut();
    }

    // SAFETY: `e` was allocated with `size_of::<DrmPendingVblankEvent>()` and verified non-null; pointers to embedded event fields are valid and aligned; `crtc` is live.
    unsafe {
        let base_ptr = core::ptr::addr_of_mut!((*e).event.base) as *mut DrmEvent;
        let vbl_ptr = core::ptr::addr_of_mut!((*e).event.vbl) as *mut DrmEventVblank;
        (*base_ptr).r#type = DRM_EVENT_FLIP_COMPLETE;
        (*base_ptr).length = size_of::<DrmEventVblank>() as u32;
        (*vbl_ptr).crtc_id = (*crtc).base.id;
        (*vbl_ptr).user_data = user_data;
    }

    e
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_connector_commit_dpms()
/// # Safety
///
/// If non-null, `state` must point to an initialized, mutable `DrmAtomicCommit` with valid `dev` and `acquire_ctx`.
/// `connector` must point to an initialized, live `DrmConnector` with a valid `dev`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_connector_commit_dpms(
    state: *mut DrmAtomicCommit,
    connector: *mut DrmConnector,
    mode: i32,
) -> i32 {
    if state.is_null() || connector.is_null() {
        return -EINVAL;
    }

    // SAFETY: `state` points to an active DRM structure passed by caller; `dev` field is non-null and valid.
    let dev = unsafe { (*state).dev };
    // SAFETY: `dev` points to an initialized `DrmDevice` containing embedded `mode_config`.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };
    // SAFETY: `connection_mutex` is embedded in `mode_config`; `acquire_ctx` is the commit context.
    let ret = unsafe {
        drm_modeset_lock(
            core::ptr::addr_of_mut!((*config).connection_mutex),
            (*state).acquire_ctx,
        )
    };
    if ret != 0 {
        return ret;
    }

    let target_mode = if mode == DRM_MODE_DPMS_ON as i32 {
        DRM_MODE_DPMS_ON as i32
    } else {
        DRM_MODE_DPMS_OFF as i32
    };
    // SAFETY: `connector` was verified non-null above; reading `dpms` is aligned and valid.
    let old_mode = unsafe { (*connector).dpms };
    if old_mode == target_mode {
        return 0;
    }

    // SAFETY: `connector` points to caller-supplied mutable atomic state; `dpms` write is aligned and within bounds.
    unsafe { (*connector).dpms = target_mode };

    // SAFETY: `connector` points to an active `DrmConnector`; `state` field is either null or a valid `DrmConnectorState`.
    let crtc = unsafe {
        if !(*connector).state.is_null() {
            (*(*connector).state).crtc
        } else {
            core::ptr::null_mut()
        }
    };

    if crtc.is_null() {
        return 0;
    }

    let ret =
        // SAFETY: `state` and `crtc` are valid; helper adds affected connectors to the atomic state.
        unsafe { crate::drm_atomic_upstream::drm_atomic_add_affected_connectors(state, crtc) };
    if ret != 0 {
        // SAFETY: `connector` points to caller-supplied mutable atomic state; `dpms` write is aligned and within bounds.
        unsafe { (*connector).dpms = old_mode };
        return ret;
    }

    // SAFETY: `state` and `crtc` are valid for atomic CRTC state lookup.
    let crtc_state = unsafe { crate::drm_atomic_upstream::drm_atomic_get_crtc_state(state, crtc) };
    if IS_ERR(crtc_state) {
        // SAFETY: `connector` points to caller-supplied mutable atomic state; `dpms` write is aligned and within bounds.
        unsafe { (*connector).dpms = old_mode };
        return PTR_ERR(crtc_state);
    }

    let mut active = false;
    // SAFETY: `state` points to active `DrmAtomicCommit` with connectors array of length `num_connector`; `crtc_state` is valid.
    unsafe {
        let num_conns = (*state).num_connector as usize;
        for i in 0..num_conns {
            let entry = (*state).connectors.add(i);
            if (*entry).ptr.is_null() {
                continue;
            }
            if (*(*entry).new_state).crtc != crtc {
                continue;
            }
            if (*(*entry).ptr).dpms == DRM_MODE_DPMS_ON as i32 {
                active = true;
                break;
            }
        }

        (*crtc_state).active = active;
    }

    // SAFETY: `state` points to active `DrmAtomicCommit` ready for atomic commit.
    let ret = unsafe { crate::drm_atomic_upstream::drm_atomic_commit(state) };
    if ret != 0 {
        // SAFETY: `connector` points to caller-supplied mutable atomic state; `dpms` write is aligned and within bounds.
        unsafe { (*connector).dpms = old_mode };
    }

    ret
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_check_prop_changes()
/// # Safety
///
/// Pure helper checking property return codes and unchanged values. Performs no pointer dereferences.
unsafe fn drm_atomic_check_prop_changes(
    ret: i32,
    old_val: u64,
    prop_value: u64,
    _prop: *mut DrmProperty,
) -> i32 {
    if ret != 0 || old_val != prop_value {
        return -EINVAL;
    }
    0
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_atomic_set_property()
/// # Safety
///
/// If non-null, `state` must point to an initialized `DrmAtomicCommit`. `obj` must point to a live DRM mode object
/// (connector, CRTC, or plane). `prop` must point to an initialized `DrmProperty`. `file_priv` can be null or a valid `DrmFile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_atomic_set_property(
    state: *mut DrmAtomicCommit,
    file_priv: *mut DrmFile,
    obj: *mut DrmModeObject,
    prop: *mut DrmProperty,
    prop_value: u64,
    async_flip: bool,
) -> i32 {
    if state.is_null() || obj.is_null() || prop.is_null() {
        return -EINVAL;
    }

    let mut ref_obj: *mut DrmModeObject = core::ptr::null_mut();
    // SAFETY: `prop` points to valid `DrmProperty`; `ref_obj` is an aligned out-pointer to receive reference object.
    if !unsafe { drm_property_change_valid_get(prop, prop_value, &mut ref_obj) } {
        return -EINVAL;
    }

    let mut ret: i32;
    let mut old_val: u64 = 0;

    // SAFETY: `obj` was checked non-null above and points to an initialized `DrmModeObject`.
    match unsafe { (*obj).r#type } {
        DRM_MODE_OBJECT_CONNECTOR => {
            // SAFETY: `obj.type` was verified as `DRM_MODE_OBJECT_CONNECTOR`; embedded within `DrmConnector`.
            let connector = unsafe { obj_to_connector(obj) };
            // SAFETY: `state` and `connector` are valid for atomic connector state lookup.
            let connector_state = unsafe {
                crate::drm_atomic_upstream::drm_atomic_get_connector_state(state, connector)
            };
            if IS_ERR(connector_state) {
                ret = PTR_ERR(connector_state);
            } else if async_flip {
                // SAFETY: `connector` and `connector_state` are valid; `old_val` is an aligned out-reference.
                let get_ret = unsafe {
                    drm_atomic_connector_get_property(
                        connector,
                        connector_state,
                        prop,
                        &mut old_val,
                    )
                };
                // SAFETY: pure check of property return code and old/new values.
                ret = unsafe { drm_atomic_check_prop_changes(get_ret, old_val, prop_value, prop) };
            } else {
                // SAFETY: `connector`, `connector_state`, `prop`, and `file_priv` are valid for setting connector property.
                ret = unsafe {
                    drm_atomic_connector_set_property(
                        connector,
                        connector_state,
                        file_priv,
                        prop,
                        prop_value,
                    )
                };
            }
        }
        DRM_MODE_OBJECT_CRTC => {
            // SAFETY: `obj.type` was verified as `DRM_MODE_OBJECT_CRTC`; embedded within `DrmCrtc`.
            let crtc = unsafe { obj_to_crtc(obj) };
            let crtc_state =
                // SAFETY: `state` and `crtc` are valid for atomic CRTC state lookup.
                unsafe { crate::drm_atomic_upstream::drm_atomic_get_crtc_state(state, crtc) };
            if IS_ERR(crtc_state) {
                ret = PTR_ERR(crtc_state);
            } else if async_flip {
                let get_ret =
                    // SAFETY: `crtc` and `crtc_state` are valid; `old_val` is an aligned out-reference.
                    unsafe { drm_atomic_crtc_get_property(crtc, crtc_state, prop, &mut old_val) };
                // SAFETY: pure check of property return code and old/new values.
                ret = unsafe { drm_atomic_check_prop_changes(get_ret, old_val, prop_value, prop) };
            } else {
                // SAFETY: `crtc`, `crtc_state`, and `prop` are valid for setting CRTC property.
                ret = unsafe { drm_atomic_crtc_set_property(crtc, crtc_state, prop, prop_value) };
            }
        }
        DRM_MODE_OBJECT_PLANE => {
            // SAFETY: `obj.type` was verified as `DRM_MODE_OBJECT_PLANE`; embedded within `DrmPlane`.
            let plane = unsafe { obj_to_plane(obj) };
            let plane_state =
                // SAFETY: `state` and `plane` are valid for atomic plane state lookup.
                unsafe { crate::drm_atomic_upstream::drm_atomic_get_plane_state(state, plane) };
            if IS_ERR(plane_state) {
                ret = PTR_ERR(plane_state);
            } else if async_flip {
                // SAFETY: `plane` and `plane_state` are valid; `old_val` is an aligned out-reference.
                let get_ret = unsafe {
                    drm_atomic_plane_get_property(plane, plane_state, prop, &mut old_val)
                };
                // SAFETY: pure check of property return code and old/new values.
                ret = unsafe { drm_atomic_check_prop_changes(get_ret, old_val, prop_value, prop) };
                if ret != 0 {
                    // SAFETY: `plane` points to an active DRM structure passed by caller; `dev` field is non-null and valid.
                    let dev = unsafe { (*plane).dev };
                    // SAFETY: `dev` points to an initialized `DrmDevice` containing embedded `mode_config`.
                    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };
                    // SAFETY: `config` points to live `mode_config` embedded within `dev`.
                    if prop == unsafe { (*config).prop_fb_id }
                        // SAFETY: `config` points to live `mode_config` embedded within `dev`.
                        || prop == unsafe { (*config).prop_in_fence_fd }
                        // SAFETY: `config` points to live `mode_config` embedded within `dev`.
                        || prop == unsafe { (*config).prop_fb_damage_clips }
                    {
                        // SAFETY: `plane`, `plane_state`, `file_priv`, and `prop` are valid for setting plane property.
                        ret = unsafe {
                            drm_atomic_plane_set_property(
                                plane,
                                plane_state,
                                file_priv,
                                prop,
                                prop_value,
                            )
                        };
                    }
                }
            } else {
                // SAFETY: `plane`, `plane_state`, `file_priv`, and `prop` are valid for setting plane property.
                ret = unsafe {
                    drm_atomic_plane_set_property(plane, plane_state, file_priv, prop, prop_value)
                };
            }
        }
        _ => {
            ret = -EINVAL;
        }
    }

    // SAFETY: releases the reference on `ref_obj` acquired by `drm_property_change_valid_get` above.
    unsafe {
        drm_property_change_valid_put(prop, ref_obj);
    }
    ret
}

#[repr(C)]
struct DrmOutFenceState {
    out_fence_ptr: *mut i32,
    sync_file: *mut SyncFile,
    fd: i32,
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c setup_out_fence()
/// # Safety
///
/// `fence_state` must point to an initialized, mutable `DrmOutFenceState` with a valid user-space `out_fence_ptr`.
/// `fence` must point to a live, initialized `DmaFence`.
unsafe fn setup_out_fence(fence_state: *mut DrmOutFenceState, fence: *mut DmaFence) -> i32 {
    // SAFETY: requests an unused file descriptor with `O_CLOEXEC` flags from kernel fd table.
    let fd = unsafe { get_unused_fd_flags(O_CLOEXEC as c_int) };
    // SAFETY: `fence_state` is the caller's counted, zero-initialized slot.
    // Record the result even on failure so cleanup sees a negative fd rather
    // than the zeroed placeholder.
    unsafe { (*fence_state).fd = fd };
    if fd < 0 {
        return fd;
    }
    // SAFETY: `fence_state` points to caller-allocated `DrmOutFenceState`; `out_fence_ptr` is validated by `put_user`; `sync_file_create` wraps valid fence.
    unsafe {
        if !put_user(fd, (*fence_state).out_fence_ptr) {
            return -EFAULT;
        }

        let sync_file = sync_file_create(fence);
        if sync_file.is_null() {
            return -ENOMEM;
        }
        (*fence_state).sync_file = sync_file;
    }
    0
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c prepare_signaling()
/// # Safety
///
/// `dev` must point to a live `DrmDevice`. `state` must point to an initialized `DrmAtomicCommit`.
/// `arg` must point to an initialized, readable `DrmModeAtomic`. `file_priv` can be null or a valid `DrmFile`.
/// `fence_state` and `num_fences` must be valid, non-null pointers to caller-managed out-variables.
unsafe fn prepare_signaling(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
    arg: *mut DrmModeAtomic,
    file_priv: *mut DrmFile,
    fence_state: *mut *mut DrmOutFenceState,
    num_fences: *mut u32,
) -> i32 {
    // SAFETY: `arg` is non-null and points to caller-supplied `DrmModeAtomic` struct.
    if (unsafe { (*arg).flags } & DRM_MODE_ATOMIC_TEST_ONLY) != 0 {
        return 0;
    }

    let mut c = 0;
    // SAFETY: `dev` points to an initialized `DrmDevice` containing embedded `mode_config`.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };

    // SAFETY: `state.crtcs` array has entries up to `num_crtc`; vblank event is created via `kzalloc`; `krealloc` resizes `fence_state` with `GFP_KERNEL`.
    unsafe {
        let num_crtcs = (*config).num_crtc as usize;
        for i in 0..num_crtcs {
            if (*state).crtcs.is_null() {
                break;
            }
            let entry = (*state).crtcs.add(i);
            let crtc = (*entry).ptr;
            if crtc.is_null() {
                continue;
            }
            let crtc_state = (*entry).new_state;
            let fence_ptr = get_out_fence_for_crtc(state, crtc);

            if ((*arg).flags & DRM_MODE_PAGE_FLIP_EVENT != 0) || !fence_ptr.is_null() {
                let e = create_vblank_event(crtc, (*arg).user_data);
                if e.is_null() {
                    return -ENOMEM;
                }
                (*crtc_state).event = e;
            }

            if ((*arg).flags & DRM_MODE_PAGE_FLIP_EVENT) != 0 {
                let e = (*crtc_state).event;
                if !file_priv.is_null() {
                    let ret = drm_event_reserve_init(
                        dev,
                        file_priv,
                        core::ptr::addr_of_mut!((*e).base),
                        core::ptr::addr_of_mut!((*e).event.base) as *mut DrmEvent,
                    );
                    if ret != 0 {
                        kfree(e);
                        (*crtc_state).event = core::ptr::null_mut();
                        return ret;
                    }
                }
            }

            if !fence_ptr.is_null() {
                let cur_count = *num_fences as usize;
                let new_count = cur_count + 1;
                let f = krealloc(
                    *fence_state as *mut c_void,
                    size_of::<DrmOutFenceState>() * new_count,
                    GFP_KERNEL,
                ) as *mut DrmOutFenceState;
                if f.is_null() {
                    return -ENOMEM;
                }
                *fence_state = f;
                core::ptr::write_bytes(f.add(cur_count), 0, 1);
                (*f.add(cur_count)).out_fence_ptr = fence_ptr;

                let fence = crate::drm_crtc_upstream::drm_crtc_create_fence(crtc);
                if fence.is_null() {
                    return -ENOMEM;
                }

                // Count the slot before filling it so that `complete_signaling`
                // releases a reserved fd or sync file if setup fails halfway.
                *num_fences = new_count as u32;
                let ret = setup_out_fence(f.add(cur_count), fence);
                if ret != 0 {
                    dma_fence_put(fence);
                    return ret;
                }

                (*(*crtc_state).event).base.fence = fence;
            }

            c += 1;
        }

        if c == 0 && ((*arg).flags & DRM_MODE_PAGE_FLIP_EVENT != 0) {
            return -EINVAL;
        }
    }

    0
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c complete_signaling()
/// # Safety
///
/// `dev` must point to a live `DrmDevice`. `state` must point to an initialized `DrmAtomicCommit`.
/// `fence_state` must be either null or point to a valid array of `num_fences` entries allocated by `prepare_signaling`,
/// which will be freed by this function.
unsafe fn complete_signaling(
    dev: *mut DrmDevice,
    state: *mut DrmAtomicCommit,
    fence_state: *mut DrmOutFenceState,
    num_fences: u32,
    install_fds: bool,
) {
    if install_fds {
        for i in 0..(num_fences as usize) {
            // SAFETY: `fence_state` contains `num_fences` initialized entries; `fd` and `sync_file.file` are valid for `fd_install`.
            unsafe {
                let entry = fence_state.add(i);
                fd_install((*entry).fd, (*(*entry).sync_file).file.cast());
            }
        }
        // SAFETY: `fence_state` was allocated via `krealloc` and is freed with `kfree` after installing fds.
        unsafe {
            kfree(fence_state);
        }
        return;
    }

    let dev_ptr = dev;
    // SAFETY: cancels pending vblank events on error; releases sync files via `fput`, frees fds via `put_unused_fd`, writes -1 to user pointers, and frees `fence_state`.
    unsafe {
        let num_crtcs = (*(*state).dev).mode_config.num_crtc as usize;
        for i in 0..num_crtcs {
            if (*state).crtcs.is_null() {
                break;
            }
            let entry = (*state).crtcs.add(i);
            if (*entry).ptr.is_null() {
                continue;
            }
            let crtc_state = (*entry).new_state;
            let event = (*crtc_state).event;
            if !event.is_null()
                && (!(*event).base.fence.is_null() || !(*event).base.file_priv.is_null())
            {
                drm_event_cancel_free(dev_ptr, core::ptr::addr_of_mut!((*event).base));
                (*crtc_state).event = core::ptr::null_mut();
            }
        }

        if fence_state.is_null() {
            return;
        }

        for i in 0..(num_fences as usize) {
            let entry = fence_state.add(i);
            if !(*entry).sync_file.is_null() {
                fput((*(*entry).sync_file).file.cast());
            }
            if (*entry).fd >= 0 {
                put_unused_fd((*entry).fd);
            }
            if !(*entry).out_fence_ptr.is_null() {
                let _ = put_user(-1i32, (*entry).out_fence_ptr);
            }
        }

        kfree(fence_state);
    }
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c set_async_flip()
/// # Safety
///
/// `state` must point to an initialized `DrmAtomicCommit` whose `dev` and CRTC state entries are initialized and live.
unsafe fn set_async_flip(state: *mut DrmAtomicCommit) {
    // SAFETY: `state` points to an active DRM structure passed by caller; `dev` field is non-null and valid.
    let dev = unsafe { (*state).dev };
    // SAFETY: `dev` points to an initialized `DrmDevice` containing `mode_config`.
    let num_crtcs = unsafe { (*dev).mode_config.num_crtc as usize };
    // SAFETY: `state.crtcs` contains allocated entries up to `num_crtc`; `new_state` is initialized and `async_flip` write is aligned.
    unsafe {
        for i in 0..num_crtcs {
            if (*state).crtcs.is_null() {
                break;
            }
            let entry = (*state).crtcs.add(i);
            if (*entry).ptr.is_null() {
                continue;
            }
            (*(*entry).new_state).async_flip = true;
        }
    }
}

// upstream: drivers/gpu/drm/drm_atomic_uapi.c drm_mode_atomic_ioctl()
/// # Safety
///
/// `dev` must point to an initialized, live `DrmDevice`. `data` must point to a readable `DrmModeAtomic` ioctl argument
/// structure. `file_priv` must point to an open, live `DrmFile`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_mode_atomic_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    if dev.is_null() || data.is_null() || file_priv.is_null() {
        return -EINVAL;
    }

    // SAFETY: `dev` was verified non-null above and points to live `DrmDevice`.
    if unsafe { !crate::drm_color_mgmt_upstream::drm_core_check_feature(dev, DRIVER_ATOMIC) } {
        return -EOPNOTSUPP;
    }

    // SAFETY: `file_priv` was verified non-null above and points to live `DrmFile`.
    if unsafe { !(*file_priv).atomic } {
        return -EINVAL;
    }

    let arg = data as *mut DrmModeAtomic;
    // SAFETY: `data` was verified non-null and `arg` points to caller-supplied `DrmModeAtomic`.
    if (unsafe { (*arg).flags } & !DRM_MODE_ATOMIC_FLAGS) != 0 {
        return -EINVAL;
    }
    // SAFETY: `arg` points to caller-supplied `DrmModeAtomic`.
    if unsafe { (*arg).reserved } != 0 {
        return -EINVAL;
    }

    let mut async_flip = false;
    // SAFETY: `arg` points to caller-supplied `DrmModeAtomic`.
    if (unsafe { (*arg).flags } & DRM_MODE_PAGE_FLIP_ASYNC) != 0 {
        // SAFETY: `dev` points to initialized `DrmDevice` containing `mode_config`.
        if unsafe { !(*dev).mode_config.async_page_flip } {
            return -EINVAL;
        }
        async_flip = true;
    }

    // SAFETY: `arg` points to caller-supplied `DrmModeAtomic`.
    if ((unsafe { (*arg).flags } & DRM_MODE_ATOMIC_TEST_ONLY) != 0)
        // SAFETY: `arg` points to caller-supplied `DrmModeAtomic`.
        && ((unsafe { (*arg).flags } & DRM_MODE_PAGE_FLIP_EVENT) != 0)
    {
        return -EINVAL;
    }

    // SAFETY: `dev` points to live `DrmDevice`; allocates a new `DrmAtomicCommit`.
    let state = unsafe { crate::drm_atomic_upstream::drm_atomic_commit_alloc(dev) };
    if state.is_null() {
        return -ENOMEM;
    }

    let mut ctx = core::mem::MaybeUninit::<DrmModesetAcquireCtx>::uninit();
    // SAFETY: `ctx` is uninitialized stack storage initialized by `drm_modeset_acquire_init`; `state` was verified non-null above.
    unsafe {
        crate::drm_modeset_lock_upstream::drm_modeset_acquire_init(
            ctx.as_mut_ptr(),
            DRM_MODESET_ACQUIRE_INTERRUPTIBLE,
        );
        (*state).acquire_ctx = ctx.as_mut_ptr();
        (*state).set_allow_modeset(((*arg).flags & DRM_MODE_ATOMIC_ALLOW_MODESET) != 0);
        (*state).set_plane_color_pipeline((*file_priv).plane_color_pipeline);
    }

    // SAFETY: `arg` points to caller-supplied `DrmModeAtomic`.
    let objs_ptr = unsafe { (*arg).objs_ptr as *const u32 };
    // SAFETY: `arg` points to caller-supplied `DrmModeAtomic`.
    let count_props_ptr = unsafe { (*arg).count_props_ptr as *const u32 };
    // SAFETY: `arg` points to caller-supplied `DrmModeAtomic`.
    let props_ptr = unsafe { (*arg).props_ptr as *const u32 };
    // SAFETY: `arg` points to caller-supplied `DrmModeAtomic`.
    let prop_values_ptr = unsafe { (*arg).prop_values_ptr as *const u64 };

    let mut ret: i32;

    'retry: loop {
        let mut copied_props: usize = 0;
        let mut fence_state: *mut DrmOutFenceState = core::ptr::null_mut();
        let mut num_fences: u32 = 0;

        ret = 0;
        // SAFETY: `arg` points to caller-supplied `DrmModeAtomic`.
        for object_index in 0..(unsafe { (*arg).count_objs } as usize) {
            let mut obj_id: u32 = 0;
            let mut count_props: u32 = 0;

            // SAFETY: `objs_ptr` is user-space pointer offset by `object_index`; safely read by `get_user`.
            if !unsafe { get_user(&mut obj_id, objs_ptr.add(object_index)) } {
                ret = -EFAULT;
                break;
            }

            // SAFETY: `dev` and `file_priv` are live pointers; looks up DRM mode object by user ID.
            let obj = unsafe { drm_mode_object_find(dev, file_priv, obj_id, DRM_MODE_OBJECT_ANY) };
            if obj.is_null() {
                ret = -ENOENT;
                break;
            }

            // SAFETY: `obj` was verified non-null above and points to live `DrmModeObject`.
            if unsafe { (*obj).properties.is_null() } {
                // SAFETY: `obj` was acquired by `drm_mode_object_find` above; drops reference.
                unsafe {
                    drm_mode_object_put(obj);
                }
                ret = -ENOENT;
                break;
            }

            // SAFETY: `count_props_ptr` is user-space pointer offset by `object_index`; safely read by `get_user`.
            if !unsafe { get_user(&mut count_props, count_props_ptr.add(object_index)) } {
                // SAFETY: `obj` was acquired by `drm_mode_object_find` above; drops reference on fault.
                unsafe {
                    drm_mode_object_put(obj);
                }
                ret = -EFAULT;
                break;
            }

            for _j in 0..(count_props as usize) {
                let mut prop_id: u32 = 0;
                let mut prop_value: u64 = 0;

                // SAFETY: `props_ptr` is user-space pointer offset by `copied_props`; safely read by `get_user`.
                if !unsafe { get_user(&mut prop_id, props_ptr.add(copied_props)) } {
                    ret = -EFAULT;
                    break;
                }

                // SAFETY: `obj` is live mode object; finds property by user ID on object.
                let prop = unsafe {
                    crate::drm_mode_object_upstream::drm_mode_obj_find_prop_id(obj, prop_id)
                };
                if prop.is_null() {
                    ret = -ENOENT;
                    break;
                }

                // SAFETY: `prop_values_ptr` is user pointer offset by `copied_props`; safely copied into stack `prop_value`.
                if !unsafe {
                    copy_from_user(
                        core::ptr::addr_of_mut!(prop_value) as *mut c_void,
                        prop_values_ptr.add(copied_props) as *const c_void,
                        size_of::<u64>(),
                    )
                } {
                    ret = -EFAULT;
                    break;
                }

                // SAFETY: `state`, `file_priv`, `obj`, and `prop` are valid pointers.
                ret = unsafe {
                    drm_atomic_set_property(state, file_priv, obj, prop, prop_value, async_flip)
                };
                if ret != 0 {
                    break;
                }

                copied_props += 1;
            }

            // SAFETY: `obj` was acquired by `drm_mode_object_find` above; drops the per-object reference.
            unsafe {
                drm_mode_object_put(obj);
            }
            if ret != 0 {
                break;
            }
        }

        if ret == 0 {
            // SAFETY: `dev`, `state`, `arg`, and `file_priv` are valid; initializes event and fence signaling structures.
            ret = unsafe {
                prepare_signaling(
                    dev,
                    state,
                    arg,
                    file_priv,
                    &mut fence_state,
                    &mut num_fences,
                )
            };
        }

        if ret == 0 {
            // SAFETY: `arg` points to caller-supplied `DrmModeAtomic`.
            if (unsafe { (*arg).flags } & DRM_MODE_PAGE_FLIP_ASYNC) != 0 {
                // SAFETY: `state` points to initialized `DrmAtomicCommit` with valid CRTC states.
                unsafe {
                    set_async_flip(state);
                }
            }

            // SAFETY: `arg` points to caller-supplied `DrmModeAtomic`.
            if (unsafe { (*arg).flags } & DRM_MODE_ATOMIC_TEST_ONLY) != 0 {
                // SAFETY: `state` points to initialized `DrmAtomicCommit` for test-only check.
                ret = unsafe { crate::drm_atomic_upstream::drm_atomic_check_only(state) };
            // SAFETY: `arg` points to caller-supplied `DrmModeAtomic`.
            } else if (unsafe { (*arg).flags } & DRM_MODE_ATOMIC_NONBLOCK) != 0 {
                // SAFETY: `state` points to initialized `DrmAtomicCommit` for nonblocking commit.
                ret = unsafe { crate::drm_atomic_upstream::drm_atomic_nonblocking_commit(state) };
            } else {
                // SAFETY: `state` points to initialized `DrmAtomicCommit` for blocking commit.
                ret = unsafe { crate::drm_atomic_upstream::drm_atomic_commit(state) };
            }
        }

        // SAFETY: `dev` and `state` are valid; fence structures from `prepare_signaling` are completed or cleaned up.
        unsafe {
            complete_signaling(dev, state, fence_state, num_fences, ret == 0);
        }

        if ret == -EDEADLK {
            // SAFETY: `state` is valid for commit clear; `ctx` was initialized by `drm_modeset_acquire_init` and is backed off.
            unsafe {
                crate::drm_atomic_upstream::drm_atomic_commit_clear(state);
                ret = crate::drm_modeset_lock_upstream::drm_modeset_backoff(ctx.as_mut_ptr());
            }
            if ret == 0 {
                continue 'retry;
            }
        }

        break;
    }

    // SAFETY: drops reference on `state`; drops locks and finalizes modeset context initialized on stack.
    unsafe {
        crate::drm_atomic_upstream::drm_atomic_commit_put(state);
        crate::drm_modeset_lock_upstream::drm_modeset_drop_locks(ctx.as_mut_ptr());
        crate::drm_modeset_lock_upstream::drm_modeset_acquire_fini(ctx.as_mut_ptr());
    }

    ret
}
