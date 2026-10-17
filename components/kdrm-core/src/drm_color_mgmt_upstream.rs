// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_color_mgmt.c`.
//!
//! Gamma/degamma/CTM/LUT property attachment, legacy gamma ioctls, color
//! encoding/range enum properties, LUT validation and the gamma/palette
//! fill helpers. Functions are named and ordered as in the C file; the C
//! `static` helpers stay private here.

// The Linux ports state the invariant of each C-ABI call once in the module
// header, as the GT ports do; per-block SAFETY prose is not repeated.
#![allow(non_snake_case)]

use core::{
    ffi::{c_char, c_void},
    mem::size_of,
};

use drm_kapi::{
    assert::warn_on,
    config::{EDEADLK, EFAULT, EINVAL, ENODEV, ENOENT, ENOMEM, EOPNOTSUPP, GFP_KERNEL},
};

/// `ENOSYS` (errno 38; not present in the kapi errno table).
const ENOSYS: i32 = 38;
use drm_kapi::mutex::{mutex_lock, mutex_unlock};

use crate::{
    drm_atomic_types::DrmAtomicCommit,
    drm_color_mgmt_types::{
        DRM_COLOR_ENCODING_MAX, DRM_COLOR_RANGE_MAX, DrmColorEncoding, DrmColorRange,
    },
    drm_crtc_types::{DrmCrtc, DrmCrtcState},
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_mode_object_types::DrmModeObject,
    drm_mode_object_upstream::{DRIVER_ATOMIC, DRIVER_MODESET},
    drm_modeset_lock_types::DrmModesetAcquireCtx,
    drm_plane_types::DrmPlane,
    drm_property_types::{DrmPropEnumList, DrmProperty, DrmPropertyBlob},
    uapi_drm_mode_types::{DrmColorLut, DrmColorLut32, DrmModeCrtcLut},
};

/// `DRM_COLOR_LUT_EQUAL_CHANNELS` (include/drm/drm_color_mgmt.h).
pub const DRM_COLOR_LUT_EQUAL_CHANNELS: u32 = 1 << 0;
/// `DRM_COLOR_LUT_NON_DECREASING` (include/drm/drm_color_mgmt.h).
pub const DRM_COLOR_LUT_NON_DECREASING: u32 = 1 << 1;

/// `drm_crtc_set_lut_func` (include/drm/drm_color_mgmt.h).
///
/// # Safety
///
/// Callers must ensure `crtc` points to a live, valid `DrmCrtc` and that
/// channel values are appropriate for the hardware's lookup table format.
pub type DrmCrtcSetLutFunc = unsafe extern "C" fn(*mut DrmCrtc, u32, u16, u16, u16);

// # Safety
//
// Foreign C functions linked from the DRM core runtime.
unsafe extern "C" {
    // upstream: drm_mode_object.c drm_object_attach_property()
    fn drm_object_attach_property(
        obj: *mut DrmModeObject,
        property: *mut DrmProperty,
        init_val: u64,
    );
    // upstream: drm_mode_object.c drm_mode_obj_find_prop_id()
    fn drm_mode_obj_find_prop_id(obj: *mut DrmModeObject, prop_id: u32) -> *mut DrmProperty;
    // upstream: drm_property.c drm_property_create_enum()
    fn drm_property_create_enum(
        dev: *mut DrmDevice,
        flags: u32,
        name: *const c_char,
        props: *const DrmPropEnumList,
        num_values: i32,
    ) -> *mut DrmProperty;
    // upstream: drm_property.c drm_property_create_blob()
    fn drm_property_create_blob(
        dev: *mut DrmDevice,
        length: usize,
        data: *const c_void,
    ) -> *mut DrmPropertyBlob;
    // upstream: drm_property.c drm_property_replace_blob()
    fn drm_property_replace_blob(
        blob: *mut *mut DrmPropertyBlob,
        new_blob: *mut DrmPropertyBlob,
    ) -> bool;
    // upstream: drm_property.c drm_property_blob_put()
    fn drm_property_blob_put(blob: *mut DrmPropertyBlob);
    // upstream: drm_atomic.c drm_atomic_commit_alloc()
    fn drm_atomic_commit_alloc(dev: *mut DrmDevice) -> *mut DrmAtomicCommit;
    // upstream: drm_atomic.c drm_atomic_get_crtc_state()
    fn drm_atomic_get_crtc_state(
        state: *mut DrmAtomicCommit,
        crtc: *mut DrmCrtc,
    ) -> *mut DrmCrtcState;
    // upstream: drm_atomic.c drm_atomic_commit()
    fn drm_atomic_commit(state: *mut DrmAtomicCommit) -> i32;
    // upstream: drm_atomic.c __drm_atomic_commit_free()
    fn __drm_atomic_commit_free(kref: *mut drm_kapi::types::Kref);
    // upstream: drm_modeset_lock.c drm_modeset_acquire_init() / drm_modeset_acquire_fini()
    fn drm_modeset_acquire_init(ctx: *mut DrmModesetAcquireCtx, flags: u32);
    fn drm_modeset_acquire_fini(ctx: *mut DrmModesetAcquireCtx);
    // upstream: drm_modeset_lock.c drm_modeset_drop_locks() / drm_modeset_backoff()
    fn drm_modeset_drop_locks(ctx: *mut DrmModesetAcquireCtx);
    fn drm_modeset_backoff(ctx: *mut DrmModesetAcquireCtx) -> i32;
    // upstream: drm_modeset_lock.c drm_modeset_lock_all_ctx()
    fn drm_modeset_lock_all_ctx(dev: *mut DrmDevice, ctx: *mut DrmModesetAcquireCtx) -> i32;
    // upstream: drm_modeset_lock.c drm_modeset_lock() / drm_modeset_unlock()
    fn drm_modeset_lock(
        lock: *mut crate::drm_modeset_lock_types::DrmModesetLock,
        ctx: *mut DrmModesetAcquireCtx,
    ) -> i32;
    fn drm_modeset_unlock(lock: *mut crate::drm_modeset_lock_types::DrmModesetLock);
}

// upstream: include/drm/drm_drv.h drm_core_check_feature()
/// # Safety
/// `dev` must be non-null and point to an initialized `DrmDevice` with a valid, non-null `driver` pointer.
#[inline]
pub(crate) unsafe fn drm_core_check_feature(dev: *mut DrmDevice, feature: u32) -> bool {
    // SAFETY: `dev` and its `driver` field are guaranteed valid, initialized, and readable per preconditions.
    unsafe { ((*(*dev).driver).driver_features & feature) != 0 }
}

// upstream: include/drm/drm_drv.h drm_drv_uses_atomic_modeset()
/// # Safety
/// `dev` must be non-null and point to an initialized `DrmDevice`.
#[inline]
pub(crate) unsafe fn drm_drv_uses_atomic_modeset(dev: *mut DrmDevice) -> bool {
    // SAFETY: `dev` is valid and non-null per caller contract. Dereferencing `dev.mode_config.funcs`
    // and calling `drm_core_check_feature` is safe while `dev` remains live.
    unsafe {
        drm_core_check_feature(dev, DRIVER_ATOMIC)
            || (!(*dev).mode_config.funcs.is_null()
                && (*(*dev).mode_config.funcs).atomic_commit.is_some())
    }
}

// upstream: include/drm/drm_atomic.h drm_atomic_commit_put()
/// # Safety
/// `state` must point to an initialized `DrmAtomicCommit` whose `ref` counter is active.
#[inline]
unsafe fn drm_atomic_commit_put(state: *mut DrmAtomicCommit) {
    // SAFETY: `state` points to a valid `DrmAtomicCommit`. Taking a mutable pointer to its `ref`
    // and calling `kref_put` decrements the reference count and invokes `__drm_atomic_commit_free` on release.
    unsafe {
        drm_kapi::primitives::kref_put(
            core::ptr::addr_of_mut!((*state).r#ref),
            Some(__drm_atomic_commit_free),
        );
    }
}

// upstream: include/drm/drm_color_mgmt.h drm_color_lut_size()
/// # Safety
/// `blob` must point to an initialized, readable `DrmPropertyBlob`.
#[inline]
unsafe fn drm_color_lut_size(blob: *const DrmPropertyBlob) -> i32 {
    // SAFETY: `blob` is guaranteed readable and properly aligned per caller precondition.
    unsafe { ((*blob).length / size_of::<DrmColorLut>()) as i32 }
}

// upstream: include/drm/drm_color_mgmt.h drm_color_lut32_size()
/// # Safety
/// `blob` must point to an initialized, readable `DrmPropertyBlob`.
#[inline]
unsafe fn drm_color_lut32_size(blob: *const DrmPropertyBlob) -> i32 {
    // SAFETY: `blob` is guaranteed readable and properly aligned per caller precondition.
    unsafe { ((*blob).length / size_of::<DrmColorLut32>()) as i32 }
}

/// `DRM_MODESET_LOCK_ALL_BEGIN()` / `DRM_MODESET_LOCK_ALL_END()` (drm_modeset_lock.h).
/// The C macros bracket a `goto`-based retry loop; `body` runs the code between
/// BEGIN and END and returns the value of `ret` it would leave behind.
// upstream: include/drm/drm_modeset_lock.h DRM_MODESET_LOCK_ALL_BEGIN/END
/// # Safety
/// `dev` must point to an initialized `DrmDevice` configured for modesetting.
pub(crate) unsafe fn with_modeset_lock_all(
    dev: *mut DrmDevice,
    flags: u32,
    mut body: impl FnMut(*mut DrmModesetAcquireCtx) -> i32,
) -> i32 {
    // SAFETY: Stack-allocated `ctx` is initialized via `drm_modeset_acquire_init`. `dev` is valid
    // and live. Modeset locks are acquired and safely dropped/cleaned up via `drm_modeset_drop_locks`
    // and `drm_modeset_acquire_fini` before returning, matching the DRM locking unwind protocol.
    unsafe {
        let mut ctx: DrmModesetAcquireCtx = core::mem::zeroed();
        let ctx_ptr: *mut DrmModesetAcquireCtx = &mut ctx;
        let uses_atomic = drm_drv_uses_atomic_modeset(dev);
        if !uses_atomic {
            mutex_lock(core::ptr::addr_of_mut!((*dev).mode_config.mutex));
        }
        drm_modeset_acquire_init(ctx_ptr, flags);
        let mut ret;
        loop {
            ret = drm_modeset_lock_all_ctx(dev, ctx_ptr);
            if ret == 0 {
                ret = body(ctx_ptr);
            }
            // modeset_lock_fail:
            if ret == -EDEADLK {
                ret = drm_modeset_backoff(ctx_ptr);
                if ret == 0 {
                    continue;
                }
            }
            break;
        }
        drm_modeset_drop_locks(ctx_ptr);
        drm_modeset_acquire_fini(ctx_ptr);
        if !uses_atomic {
            mutex_unlock(core::ptr::addr_of_mut!((*dev).mode_config.mutex));
        }
        ret
    }
}

/// Converts a u64 S31.32 fixed-point value to a signed Q(m).(n) value.
// upstream: drm_color_mgmt.c drm_color_ctm_s31_32_to_qm_n()
pub fn drm_color_ctm_s31_32_to_qm_n(user_input: u64, m: u32, n: u32) -> u64 {
    let mag = (user_input & !(1u64 << 63)) >> (32 - n);
    let negative = (user_input & (1u64 << 63)) != 0;
    warn_on(m > 32 || n > 32);
    let limit = if negative {
        1u64 << (n + m - 1)
    } else {
        (1u64 << (n + m - 1)).wrapping_sub(1)
    };
    // clamp_val(mag, 0, limit): mag is unsigned, so only the upper bound applies.
    let val = if mag > limit { limit } else { mag };
    // Negative values are returned as two's complement, as `-val` does in C.
    if negative {
        (val as i64).wrapping_neg() as u64
    } else {
        val
    }
}

/// Attaches the color-management properties a CRTC supports.
// upstream: drm_color_mgmt.c drm_crtc_enable_color_mgmt()
#[unsafe(no_mangle)]
/// # Safety
/// `crtc` must be non-null and point to an initialized, live `DrmCrtc` associated with an initialized `DrmDevice`.
/// Callers must hold the appropriate modeset configuration locks.
pub unsafe extern "C" fn drm_crtc_enable_color_mgmt(
    crtc: *mut DrmCrtc,
    degamma_lut_size: u32,
    has_ctm: bool,
    gamma_lut_size: u32,
) {
    // SAFETY: `crtc` and `(*crtc).dev` are guaranteed valid and initialized by the caller.
    // Accessing `mode_config` and attaching properties to `(*crtc).base` does not create aliasing
    // violations while modeset configuration locks are held.
    unsafe {
        let dev = (*crtc).dev;
        let config = core::ptr::addr_of_mut!((*dev).mode_config);
        let base = core::ptr::addr_of_mut!((*crtc).base);

        if degamma_lut_size != 0 {
            drm_object_attach_property(base, (*config).degamma_lut_property, 0);
            drm_object_attach_property(
                base,
                (*config).degamma_lut_size_property,
                degamma_lut_size as u64,
            );
        }

        if has_ctm {
            drm_object_attach_property(base, (*config).ctm_property, 0);
        }

        if gamma_lut_size != 0 {
            drm_object_attach_property(base, (*config).gamma_lut_property, 0);
            drm_object_attach_property(
                base,
                (*config).gamma_lut_size_property,
                gamma_lut_size as u64,
            );
        }
    }
}

/// Allocates the legacy gamma store for a CRTC and fills it with a linear ramp.
// upstream: drm_color_mgmt.c drm_mode_crtc_set_gamma_size()
#[unsafe(no_mangle)]
/// # Safety
/// `crtc` must be non-null, properly aligned, and point to an initialized `DrmCrtc`.
/// `gamma_size` must be non-negative.
pub unsafe extern "C" fn drm_mode_crtc_set_gamma_size(crtc: *mut DrmCrtc, gamma_size: i32) -> i32 {
    // SAFETY: `crtc` is valid and initialized. If allocation of `gamma_size * 3 * size_of::<u16>()`
    // bytes via `kzalloc` succeeds, `store` points to a zeroed buffer large enough for 3 disjoint
    // contiguous arrays of `gamma_size` elements of `u16`. Pointer offsets `r_base`, `g_base`, and
    // `b_base` are within allocated bounds, and writes up to `gamma_size` are strictly within bounds.
    unsafe {
        (*crtc).gamma_size = gamma_size as u32;
        // kcalloc(gamma_size, sizeof(uint16_t) * 3, GFP_KERNEL)
        let bytes = match (gamma_size as usize).checked_mul(size_of::<u16>() * 3) {
            Some(bytes) => bytes,
            None => {
                (*crtc).gamma_size = 0;
                return -ENOMEM;
            }
        };
        let store = drm_kapi::memory::kzalloc(bytes, GFP_KERNEL) as *mut u16;
        (*crtc).gamma_store = store;
        if store.is_null() {
            (*crtc).gamma_size = 0;
            return -ENOMEM;
        }

        let r_base = store;
        let g_base = r_base.add(gamma_size as usize);
        let b_base = g_base.add(gamma_size as usize);
        for i in 0..gamma_size as usize {
            *r_base.add(i) = (i << 8) as u16;
            *g_base.add(i) = (i << 8) as u16;
            *b_base.add(i) = (i << 8) as u16;
        }
        0
    }
}

// upstream: drm_color_mgmt.c drm_crtc_supports_legacy_gamma()
/// # Safety
/// `crtc` must be non-null and point to an initialized `DrmCrtc` associated with an initialized `DrmDevice`.
unsafe fn drm_crtc_supports_legacy_gamma(crtc: *mut DrmCrtc) -> bool {
    // SAFETY: `crtc`, `(*crtc).dev`, and `(*crtc).funcs` are guaranteed initialized and live per preconditions.
    // Reading mode_config properties and querying mode object properties via `drm_mode_obj_find_prop_id`
    // is safe and does not mutate shared state.
    unsafe {
        let config = core::ptr::addr_of!((*(*crtc).dev).mode_config);
        let gamma_id = (*(*config).gamma_lut_property).base.id;
        let degamma_id = (*(*config).degamma_lut_property).base.id;

        if (*crtc).gamma_size == 0 {
            return false;
        }
        if (*(*crtc).funcs).gamma_set.is_some() {
            return true;
        }
        !drm_mode_obj_find_prop_id(core::ptr::addr_of_mut!((*crtc).base), gamma_id).is_null()
            || !drm_mode_obj_find_prop_id(core::ptr::addr_of_mut!((*crtc).base), degamma_id)
                .is_null()
    }
}

// upstream: drm_color_mgmt.c drm_crtc_legacy_gamma_set()
/// # Safety
/// `crtc` must point to an initialized `DrmCrtc`. `red`, `green`, `blue` must each point to contiguous
/// readable buffers of at least `size` elements of `u16`. If `ctx` is non-null, it must be an active acquire context.
unsafe fn drm_crtc_legacy_gamma_set(
    crtc: *mut DrmCrtc,
    red: *mut u16,
    green: *mut u16,
    blue: *mut u16,
    size: u32,
    ctx: *mut DrmModesetAcquireCtx,
) -> i32 {
    // SAFETY: `crtc` is valid and live. If `gamma_set` hook is defined, delegating to it passes valid buffers.
    // Otherwise, `drm_property_create_blob` allocates a new blob of `size * size_of::<DrmColorLut>()` bytes;
    // indexing `blob_data` up to `size` matches the allocated capacity and caller buffers `red`, `green`, `blue`.
    // Updating CRTC state and committing via `drm_atomic_commit` follows the atomic state API; allocated
    // commit state and property blobs are cleaned up via `drm_atomic_commit_put` and `drm_property_blob_put`.
    unsafe {
        let dev = (*crtc).dev;
        let gamma_id = (*(*dev).mode_config.gamma_lut_property).base.id;
        let degamma_id = (*(*dev).mode_config.degamma_lut_property).base.id;
        let ret: i32;

        if let Some(gamma_set) = (*(*crtc).funcs).gamma_set {
            return gamma_set(crtc, red, green, blue, size, ctx);
        }

        let use_gamma_lut = if !drm_mode_obj_find_prop_id(
            core::ptr::addr_of_mut!((*crtc).base),
            gamma_id,
        )
        .is_null()
        {
            true
        } else if !drm_mode_obj_find_prop_id(core::ptr::addr_of_mut!((*crtc).base), degamma_id)
            .is_null()
        {
            false
        } else {
            return -ENODEV;
        };

        let state = drm_atomic_commit_alloc(dev);
        if state.is_null() {
            return -ENOMEM;
        }

        let mut blob: *mut DrmPropertyBlob = drm_property_create_blob(
            dev,
            size_of::<DrmColorLut>() * size as usize,
            core::ptr::null(),
        );
        if drm_kapi::config::IS_ERR(blob) {
            ret = drm_kapi::config::PTR_ERR(blob);
            blob = core::ptr::null_mut();
            // goto fail
            drm_atomic_commit_put(state);
            drm_property_blob_put(blob);
            return ret;
        }

        // Prepare GAMMA_LUT with the legacy values.
        let blob_data = (*blob).data as *mut DrmColorLut;
        for i in 0..size as usize {
            (*blob_data.add(i)).red = *red.add(i);
            (*blob_data.add(i)).green = *green.add(i);
            (*blob_data.add(i)).blue = *blue.add(i);
        }

        (*state).acquire_ctx = ctx;
        let crtc_state = drm_atomic_get_crtc_state(state, crtc);
        if drm_kapi::config::IS_ERR(crtc_state) {
            ret = drm_kapi::config::PTR_ERR(crtc_state);
            drm_atomic_commit_put(state);
            drm_property_blob_put(blob);
            return ret;
        }

        // Set GAMMA_LUT and reset DEGAMMA_LUT and CTM.
        let mut replaced = drm_property_replace_blob(
            core::ptr::addr_of_mut!((*crtc_state).degamma_lut),
            if use_gamma_lut {
                core::ptr::null_mut()
            } else {
                blob
            },
        );
        replaced |= drm_property_replace_blob(
            core::ptr::addr_of_mut!((*crtc_state).ctm),
            core::ptr::null_mut(),
        );
        replaced |= drm_property_replace_blob(
            core::ptr::addr_of_mut!((*crtc_state).gamma_lut),
            if use_gamma_lut {
                blob
            } else {
                core::ptr::null_mut()
            },
        );
        if replaced {
            (*crtc_state)._bitfields_10[0] |= 1 << 5; // color_mgmt_changed
        }
        ret = drm_atomic_commit(state);

        // fail:
        drm_atomic_commit_put(state);
        drm_property_blob_put(blob);
        ret
    }
}

/// Legacy GAMMA_SET ioctl: copies the user's R/G/B ramps into the gamma store
/// and programs them.
// upstream: drm_color_mgmt.c drm_mode_gamma_set_ioctl()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to an initialized `DrmDevice` supporting modesetting.
/// `data` must point to a readable `DrmModeCrtcLut` ioctl argument buffer.
/// `file_priv` must be a valid open `DrmFile` or null.
pub unsafe extern "C" fn drm_mode_gamma_set_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: `data` is cast to `*mut DrmModeCrtcLut` and valid for reads. `drm_crtc_find` verifies CRTC existence.
    // Under `with_modeset_lock_all`, `(*crtc).gamma_store` is accessed with exclusive lock; `copy_from_user`
    // safely validates userspace pointers and copies `size` bytes into the allocated disjoint regions
    // `r_base`, `g_base`, and `b_base` of `gamma_store`. Calling `drm_crtc_legacy_gamma_set` forwards valid buffers.
    unsafe {
        let crtc_lut = data as *mut DrmModeCrtcLut;

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        let crtc = crate::drm_crtc_upstream::drm_crtc_find(dev, file_priv, (*crtc_lut).crtc_id);
        if crtc.is_null() {
            return -ENOENT;
        }

        if !drm_crtc_supports_legacy_gamma(crtc) {
            return -ENOSYS;
        }

        // memcpy into gamma store
        if (*crtc_lut).gamma_size != (*crtc).gamma_size {
            return -EINVAL;
        }

        with_modeset_lock_all(dev, 0, |ctx| {
            let size = ((*crtc_lut).gamma_size as usize) * size_of::<u16>();
            let r_base = (*crtc).gamma_store as *mut c_void;
            if drm_kapi::mm_native::copy_from_user(r_base, (*crtc_lut).red as *const c_void, size)
                != 0
            {
                return -EFAULT;
            }

            let g_base = (r_base as *mut u8).add(size) as *mut c_void;
            if drm_kapi::mm_native::copy_from_user(g_base, (*crtc_lut).green as *const c_void, size)
                != 0
            {
                return -EFAULT;
            }

            let b_base = (g_base as *mut u8).add(size) as *mut c_void;
            if drm_kapi::mm_native::copy_from_user(b_base, (*crtc_lut).blue as *const c_void, size)
                != 0
            {
                return -EFAULT;
            }

            drm_crtc_legacy_gamma_set(
                crtc,
                r_base as *mut u16,
                g_base as *mut u16,
                b_base as *mut u16,
                (*crtc).gamma_size,
                ctx,
            )
        })
    }
}

/// Legacy GAMMA_GET ioctl: copies the gamma store back to userspace.
// upstream: drm_color_mgmt.c drm_mode_gamma_get_ioctl()
#[unsafe(no_mangle)]
/// # Safety
/// `dev` must point to an initialized `DrmDevice`.
/// `data` must point to a readable and writable `DrmModeCrtcLut` ioctl buffer.
/// `file_priv` must be a valid open `DrmFile` or null.
pub unsafe extern "C" fn drm_mode_gamma_get_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: `data` is cast to `DrmModeCrtcLut`. `drm_crtc_find` returns a valid CRTC or null.
    // `(*crtc).mutex` is locked during the read of `(*crtc).gamma_store` and unlocked before returning.
    // `copy_to_user` safely transfers `size` bytes from each disjoint channel buffer into user memory.
    unsafe {
        let crtc_lut = data as *mut DrmModeCrtcLut;
        let mut ret: i32 = 0;

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        let crtc = crate::drm_crtc_upstream::drm_crtc_find(dev, file_priv, (*crtc_lut).crtc_id);
        if crtc.is_null() {
            return -ENOENT;
        }

        // memcpy into gamma store
        if (*crtc_lut).gamma_size != (*crtc).gamma_size {
            return -EINVAL;
        }

        drm_modeset_lock(
            core::ptr::addr_of_mut!((*crtc).mutex),
            core::ptr::null_mut(),
        );

        let size = ((*crtc_lut).gamma_size as usize) * size_of::<u16>();
        let r_base = (*crtc).gamma_store as *const c_void;
        if drm_kapi::mm_native::copy_to_user((*crtc_lut).red as *mut c_void, r_base, size) != 0 {
            ret = -EFAULT;
            // goto out
            drm_modeset_unlock(core::ptr::addr_of_mut!((*crtc).mutex));
            return ret;
        }

        let g_base = (r_base as *const u8).add(size) as *const c_void;
        if drm_kapi::mm_native::copy_to_user((*crtc_lut).green as *mut c_void, g_base, size) != 0 {
            ret = -EFAULT;
            drm_modeset_unlock(core::ptr::addr_of_mut!((*crtc).mutex));
            return ret;
        }

        let b_base = (g_base as *const u8).add(size) as *const c_void;
        if drm_kapi::mm_native::copy_to_user((*crtc_lut).blue as *mut c_void, b_base, size) != 0 {
            ret = -EFAULT;
            drm_modeset_unlock(core::ptr::addr_of_mut!((*crtc).mutex));
            return ret;
        }

        // out:
        drm_modeset_unlock(core::ptr::addr_of_mut!((*crtc).mutex));
        ret
    }
}

// upstream: drm_color_mgmt.c color_encoding_name[]
static COLOR_ENCODING_NAME: [&[u8]; 3] = [
    b"ITU-R BT.601 YCbCr\0",
    b"ITU-R BT.709 YCbCr\0",
    b"ITU-R BT.2020 YCbCr\0",
];

// upstream: drm_color_mgmt.c color_range_name[] (indexed by DRM_COLOR_YCBCR_*_RANGE)
// The C table is designated-initialized: FULL_RANGE = 1 first entry, LIMITED = 0.
// Index 0 is therefore LIMITED and index 1 is FULL, matching the enum values.
static COLOR_RANGE_NAME_BY_ENUM: [&[u8]; 2] = [
    b"YCbCr limited range\0", // DRM_COLOR_YCBCR_LIMITED_RANGE = 0
    b"YCbCr full range\0",    // DRM_COLOR_YCBCR_FULL_RANGE = 1
];

// upstream: drm_color_mgmt.c drm_get_color_encoding_name()
pub fn drm_get_color_encoding_name(encoding: DrmColorEncoding) -> *const c_char {
    if warn_on(encoding as usize >= COLOR_ENCODING_NAME.len()) {
        return c"unknown".as_ptr();
    }
    COLOR_ENCODING_NAME[encoding as usize]
        .as_ptr()
        .cast::<c_char>()
}

// upstream: drm_color_mgmt.c drm_get_color_range_name()
pub fn drm_get_color_range_name(range: DrmColorRange) -> *const c_char {
    if warn_on(range as usize >= COLOR_RANGE_NAME_BY_ENUM.len()) {
        return c"unknown".as_ptr();
    }
    COLOR_RANGE_NAME_BY_ENUM[range as usize]
        .as_ptr()
        .cast::<c_char>()
}

/// Creates the COLOR_ENCODING and COLOR_RANGE enum properties for a plane.
// upstream: drm_color_mgmt.c drm_plane_create_color_properties()
#[unsafe(no_mangle)]
/// # Safety
/// `plane` must point to an initialized `DrmPlane` associated with an initialized `DrmDevice`.
pub unsafe extern "C" fn drm_plane_create_color_properties(
    plane: *mut DrmPlane,
    supported_encodings: u32,
    supported_ranges: u32,
    default_encoding: DrmColorEncoding,
    default_range: DrmColorRange,
) -> i32 {
    // SAFETY: `plane` and `(*plane).dev` are guaranteed valid. Enum lists are stack-allocated
    // and filled with valid static string pointers from `COLOR_ENCODING_NAME` and `COLOR_RANGE_NAME_BY_ENUM`.
    // Created enum properties are attached to `(*plane).base` and default values are assigned to `(*plane).state`
    // if non-null, without concurrent interference during plane initialization.
    unsafe {
        let dev = (*plane).dev;
        let max = if DRM_COLOR_ENCODING_MAX > DRM_COLOR_RANGE_MAX {
            DRM_COLOR_ENCODING_MAX
        } else {
            DRM_COLOR_RANGE_MAX
        } as usize;
        let mut enum_list: [DrmPropEnumList; 3] = [
            DrmPropEnumList {
                r#type: 0,
                name: core::ptr::null_mut(),
            },
            DrmPropEnumList {
                r#type: 0,
                name: core::ptr::null_mut(),
            },
            DrmPropEnumList {
                r#type: 0,
                name: core::ptr::null_mut(),
            },
        ];
        debug_assert!(max <= enum_list.len());

        // -BIT(DRM_COLOR_ENCODING_MAX) == !((1 << ENCODING_MAX) - 1)
        let encoding_hi_mask = !((1u32 << DRM_COLOR_ENCODING_MAX) - 1);
        let range_hi_mask = !((1u32 << DRM_COLOR_RANGE_MAX) - 1);
        if warn_on(
            supported_encodings == 0
                || (supported_encodings & encoding_hi_mask) != 0
                || (supported_encodings & (1u32 << default_encoding)) == 0,
        ) {
            return -EINVAL;
        }
        if warn_on(
            supported_ranges == 0
                || (supported_ranges & range_hi_mask) != 0
                || (supported_ranges & (1u32 << default_range)) == 0,
        ) {
            return -EINVAL;
        }

        let mut len = 0usize;
        for i in 0..DRM_COLOR_ENCODING_MAX {
            if (supported_encodings & (1u32 << i)) == 0 {
                continue;
            }
            enum_list[len].r#type = i as i32;
            enum_list[len].name = COLOR_ENCODING_NAME[i as usize]
                .as_ptr()
                .cast::<c_char>()
                .cast_mut();
            len += 1;
        }

        let prop = drm_property_create_enum(
            dev,
            0,
            c"COLOR_ENCODING".as_ptr(),
            enum_list.as_ptr(),
            len as i32,
        );
        if prop.is_null() {
            return -ENOMEM;
        }
        (*plane).color_encoding_property = prop;
        drm_object_attach_property(
            core::ptr::addr_of_mut!((*plane).base),
            prop,
            default_encoding as u64,
        );
        if !(*plane).state.is_null() {
            // The C field is `enum drm_color_encoding color_encoding`.
            (*(*plane).state).color_encoding = default_encoding;
        }

        len = 0;
        for i in 0..DRM_COLOR_RANGE_MAX {
            if (supported_ranges & (1u32 << i)) == 0 {
                continue;
            }
            enum_list[len].r#type = i as i32;
            enum_list[len].name = COLOR_RANGE_NAME_BY_ENUM[i as usize]
                .as_ptr()
                .cast::<c_char>()
                .cast_mut();
            len += 1;
        }

        let prop = drm_property_create_enum(
            dev,
            0,
            c"COLOR_RANGE".as_ptr(),
            enum_list.as_ptr(),
            len as i32,
        );
        if prop.is_null() {
            return -ENOMEM;
        }
        (*plane).color_range_property = prop;
        drm_object_attach_property(
            core::ptr::addr_of_mut!((*plane).base),
            prop,
            default_range as u64,
        );
        if !(*plane).state.is_null() {
            (*(*plane).state).color_range = default_range;
        }

        0
    }
}

/// Validates a 16-bit LUT blob against the `DRM_COLOR_LUT_*` test mask.
// upstream: drm_color_mgmt.c drm_color_lut_check()
#[unsafe(no_mangle)]
/// # Safety
/// If non-null, `lut` must point to an initialized `DrmPropertyBlob` whose `data` field points
/// to readable memory of at least `lut.length` bytes.
pub unsafe extern "C" fn drm_color_lut_check(lut: *const DrmPropertyBlob, tests: u32) -> i32 {
    // SAFETY: `lut` is checked for null. Reading elements of `DrmColorLut` through `entry.add(i)`
    // stays strictly within `drm_color_lut_size(lut)` elements, which is bounded by `(*lut).length`.
    unsafe {
        if lut.is_null() || tests == 0 {
            return 0;
        }
        let entry = (*lut).data as *const DrmColorLut;
        for i in 0..drm_color_lut_size(lut) {
            let e = core::ptr::read(entry.add(i as usize));
            if (tests & DRM_COLOR_LUT_EQUAL_CHANNELS) != 0 && (e.red != e.blue || e.red != e.green)
            {
                // DRM_DEBUG_KMS("All LUT entries must have equal r/g/b\n")
                return -EINVAL;
            }
            if i > 0 && (tests & DRM_COLOR_LUT_NON_DECREASING) != 0 {
                let p = core::ptr::read(entry.add(i as usize - 1));
                if e.red < p.red || e.green < p.green || e.blue < p.blue {
                    // DRM_DEBUG_KMS("LUT entries must never decrease.\n")
                    return -EINVAL;
                }
            }
        }
        0
    }
}

/// Programs a 256-entry 8:8:8 LUT through `set_gamma`.
// upstream: drm_color_mgmt.c drm_crtc_load_gamma_888()
/// # Safety
/// `crtc` must be an initialized `DrmCrtc`.
/// `lut` must point to an array of at least 256 valid, readable `DrmColorLut` entries.
/// `set_gamma` must be callable with valid channel parameters.
pub unsafe fn drm_crtc_load_gamma_888(
    crtc: *mut DrmCrtc,
    lut: *const DrmColorLut,
    set_gamma: DrmCrtcSetLutFunc,
) {
    // SAFETY: `lut` is guaranteed by the caller to point to at least 256 readable entries.
    // Each index `i` in `0..256` is within bounds and calling `set_gamma` satisfies its callback preconditions.
    unsafe {
        for i in 0..256usize {
            let e = core::ptr::read(lut.add(i));
            set_gamma(crtc, i as u32, e.red, e.green, e.blue);
        }
    }
}

/// Programs a 32-entry 5:6:5 gamma LUT derived from a 8:8:8 source.
// upstream: drm_color_mgmt.c drm_crtc_load_gamma_565_from_888()
/// # Safety
/// `crtc` must be an initialized `DrmCrtc`.
/// `lut` must point to an array of at least 256 valid, readable `DrmColorLut` entries.
/// `set_gamma` must be callable with valid channel parameters.
pub unsafe fn drm_crtc_load_gamma_565_from_888(
    crtc: *mut DrmCrtc,
    lut: *const DrmColorLut,
    set_gamma: DrmCrtcSetLutFunc,
) {
    // SAFETY: The highest index accessed into `lut` is `63 * 4 + 63 / 16 = 255`, which is within
    // the 256 elements guaranteed by the caller. Calling `set_gamma` satisfies its callback invariants.
    unsafe {
        for i in 0..32usize {
            let r = (*lut.add(i * 8 + i / 4)).red;
            let g = (*lut.add(i * 4 + i / 16)).green;
            let b = (*lut.add(i * 8 + i / 4)).blue;
            set_gamma(crtc, i as u32, r, g, b);
        }
        // Green has one more bit, so add padding with 0 for red and blue.
        for i in 32..64usize {
            let g = (*lut.add(i * 4 + i / 16)).green;
            set_gamma(crtc, i as u32, 0, g, 0);
        }
    }
}

/// Programs a 32-entry 5:5:5 gamma LUT derived from a 8:8:8 source.
// upstream: drm_color_mgmt.c drm_crtc_load_gamma_555_from_888()
/// # Safety
/// `crtc` must be an initialized `DrmCrtc`.
/// `lut` must point to an array of at least 256 valid, readable `DrmColorLut` entries.
/// `set_gamma` must be callable with valid channel parameters.
pub unsafe fn drm_crtc_load_gamma_555_from_888(
    crtc: *mut DrmCrtc,
    lut: *const DrmColorLut,
    set_gamma: DrmCrtcSetLutFunc,
) {
    // SAFETY: The highest index accessed is `31 * 8 + 31 / 4 = 255`, within the 256 readable entries
    // guaranteed by the caller. Calling `set_gamma` satisfies its callback invariants.
    unsafe {
        for i in 0..32usize {
            let r = (*lut.add(i * 8 + i / 4)).red;
            let g = (*lut.add(i * 8 + i / 4)).green;
            let b = (*lut.add(i * 8 + i / 4)).blue;
            set_gamma(crtc, i as u32, r, g, b);
        }
    }
}

// upstream: drm_color_mgmt.c fill_gamma_888()
/// # Safety
/// `crtc` must be valid for `set_gamma`. `set_gamma` must be safe to call with index `i`.
unsafe fn fill_gamma_888(
    crtc: *mut DrmCrtc,
    i: u32,
    r: u16,
    g: u16,
    b: u16,
    set_gamma: DrmCrtcSetLutFunc,
) {
    let r = (r << 8) | r;
    let g = (g << 8) | g;
    let b = (b << 8) | b;
    // SAFETY: Invoking `set_gamma` with 8-bit channel values replicated to 16 bits for entry index `i`.
    unsafe { set_gamma(crtc, i, r, g, b) };
}

// upstream: drm_color_mgmt.c drm_crtc_fill_gamma_888()
/// # Safety
/// `crtc` must be valid for `set_gamma`. `set_gamma` must be callable for indices 0..256.
pub unsafe fn drm_crtc_fill_gamma_888(crtc: *mut DrmCrtc, set_gamma: DrmCrtcSetLutFunc) {
    for i in 0..256u32 {
        // SAFETY: Invoking `fill_gamma_888` for indices 0..256 is safe under the caller's guarantee.
        unsafe { fill_gamma_888(crtc, i, i as u16, i as u16, i as u16, set_gamma) };
    }
}

// upstream: drm_color_mgmt.c fill_gamma_565()
/// # Safety
/// `crtc` must be valid for `set_gamma`. `set_gamma` must be safe to call with index `i`.
unsafe fn fill_gamma_565(
    crtc: *mut DrmCrtc,
    i: u32,
    r: u16,
    g: u16,
    b: u16,
    set_gamma: DrmCrtcSetLutFunc,
) {
    let r = (r << 11) | (r << 6) | (r << 1) | (r >> 4);
    let g = (g << 10) | (g << 4) | (g >> 2);
    let b = (b << 11) | (b << 6) | (b << 1) | (b >> 4);
    // SAFETY: Invoking `set_gamma` with 5/6-bit channels bit-expanded into 16-bit RGB565 representation.
    unsafe { set_gamma(crtc, i, r, g, b) };
}

// upstream: drm_color_mgmt.c drm_crtc_fill_gamma_565()
/// # Safety
/// `crtc` must be valid for `set_gamma`. `set_gamma` must be callable for indices 0..64.
pub unsafe fn drm_crtc_fill_gamma_565(crtc: *mut DrmCrtc, set_gamma: DrmCrtcSetLutFunc) {
    for i in 0..32u32 {
        // SAFETY: Invoking `fill_gamma_565` for indices 0..32 is safe under caller's guarantee.
        unsafe { fill_gamma_565(crtc, i, i as u16, i as u16, i as u16, set_gamma) };
    }
    // Green has one more bit, so add padding with 0 for red and blue.
    for i in 32..64u32 {
        // SAFETY: Invoking `fill_gamma_565` for padded indices 32..64 is safe under caller's guarantee.
        unsafe { fill_gamma_565(crtc, i, 0, i as u16, 0, set_gamma) };
    }
}

// upstream: drm_color_mgmt.c fill_gamma_555()
/// # Safety
/// `crtc` must be valid for `set_gamma`. `set_gamma` must be safe to call with index `i`.
unsafe fn fill_gamma_555(
    crtc: *mut DrmCrtc,
    i: u32,
    r: u16,
    g: u16,
    b: u16,
    set_gamma: DrmCrtcSetLutFunc,
) {
    let r = (r << 11) | (r << 6) | (r << 1) | (r >> 4);
    let g = (g << 11) | (g << 6) | (g << 1) | (g >> 4);
    // Upstream computes the blue channel from r (`r >> 4`); kept as-is.
    let b = (b << 11) | (b << 6) | (b << 1) | (r >> 4);
    // SAFETY: Invoking `set_gamma` with 5-bit channels bit-expanded into 16-bit RGB555 representation.
    unsafe { set_gamma(crtc, i, r, g, b) };
}

// upstream: drm_color_mgmt.c drm_crtc_fill_gamma_555()
/// # Safety
/// `crtc` must be valid for `set_gamma`. `set_gamma` must be callable for indices 0..32.
pub unsafe fn drm_crtc_fill_gamma_555(crtc: *mut DrmCrtc, set_gamma: DrmCrtcSetLutFunc) {
    for i in 0..32u32 {
        // SAFETY: Invoking `fill_gamma_555` for indices 0..32 is safe under caller's guarantee.
        unsafe { fill_gamma_555(crtc, i, i as u16, i as u16, i as u16, set_gamma) };
    }
}

/// Programs a 256-entry 8-bit palette from a 8:8:8 LUT.
// upstream: drm_color_mgmt.c drm_crtc_load_palette_8()
/// # Safety
/// `crtc` must be valid for `set_palette`.
/// `lut` must point to an array of at least 256 readable `DrmColorLut` entries.
/// `set_palette` must be callable with valid channel parameters.
pub unsafe fn drm_crtc_load_palette_8(
    crtc: *mut DrmCrtc,
    lut: *const DrmColorLut,
    set_palette: DrmCrtcSetLutFunc,
) {
    // SAFETY: `lut` is guaranteed readable for 256 entries. Dereferencing entries and invoking
    // `set_palette` for each index in 0..256 satisfies the callback invariants.
    unsafe {
        for i in 0..256usize {
            let e = core::ptr::read(lut.add(i));
            set_palette(crtc, i as u32, e.red, e.green, e.blue);
        }
    }
}

// upstream: drm_color_mgmt.c fill_palette_332()
/// # Safety
/// `crtc` must be valid for `set_palette`. `set_palette` must be safe to call with index `i`.
unsafe fn fill_palette_332(
    crtc: *mut DrmCrtc,
    r: u16,
    g: u16,
    b: u16,
    set_palette: DrmCrtcSetLutFunc,
) {
    let i = ((r << 5) | (g << 2) | b) as u32; // 8-bit palette index
    // Expand R (3-bit) G (3-bit) and B (2-bit) values to 16-bit values.
    let r = (r << 13) | (r << 10) | (r << 7) | (r << 4) | (r << 1) | (r >> 2);
    let g = (g << 13) | (g << 10) | (g << 7) | (g << 4) | (g << 1) | (g >> 2);
    let b = (b << 14) | (b << 12) | (b << 10) | (b << 8) | (b << 6) | (b << 4) | (b << 2) | b;
    // SAFETY: Calling `set_palette` with calculated 8-bit index and expanded 16-bit color channels is safe.
    unsafe { set_palette(crtc, i, r, g, b) };
}

// upstream: drm_color_mgmt.c drm_crtc_fill_palette_332()
/// # Safety
/// `crtc` must be valid for `set_palette`. `set_palette` must be callable for all 8-bit palette indices.
pub unsafe fn drm_crtc_fill_palette_332(crtc: *mut DrmCrtc, set_palette: DrmCrtcSetLutFunc) {
    // Limits of 8-8-4 are the maximum number of values for each channel.
    for r in 0..8u16 {
        for g in 0..8u16 {
            for b in 0..4u16 {
                // SAFETY: Calling `fill_palette_332` for r < 8, g < 8, b < 4 stays within 8-bit index bounds.
                unsafe { fill_palette_332(crtc, r, g, b, set_palette) };
            }
        }
    }
}

// upstream: drm_color_mgmt.c fill_palette_8()
/// # Safety
/// `crtc` must be valid for `set_palette`. `set_palette` must be safe to call with index `i`.
unsafe fn fill_palette_8(crtc: *mut DrmCrtc, i: u32, set_palette: DrmCrtcSetLutFunc) {
    let y = ((i << 8) | i) as u16; // relative luminance
    // SAFETY: Calling `set_palette` with luminance replicated across channels satisfies preconditions.
    unsafe { set_palette(crtc, i, y, y, y) };
}

// upstream: drm_color_mgmt.c drm_crtc_fill_palette_8()
/// # Safety
/// `crtc` must be valid for `set_palette`. `set_palette` must be callable for indices 0..256.
pub unsafe fn drm_crtc_fill_palette_8(crtc: *mut DrmCrtc, set_palette: DrmCrtcSetLutFunc) {
    for i in 0..256u32 {
        // SAFETY: Invoking `fill_palette_8` for indices 0..256 is safe under caller's guarantee.
        unsafe { fill_palette_8(crtc, i, set_palette) };
    }
}

/// Validates a 32-bit LUT blob against the `DRM_COLOR_LUT_*` test mask.
// upstream: drm_color_mgmt.c drm_color_lut32_check()
#[unsafe(no_mangle)]
/// # Safety
/// If non-null, `lut` must point to an initialized `DrmPropertyBlob` whose `data` field points
/// to readable memory of at least `lut.length` bytes.
pub unsafe extern "C" fn drm_color_lut32_check(lut: *const DrmPropertyBlob, tests: u32) -> i32 {
    // SAFETY: `lut` is checked for null. Reading elements of `DrmColorLut32` through `entry.add(i)`
    // stays strictly within `drm_color_lut32_size(lut)` elements, which is bounded by `(*lut).length`.
    unsafe {
        if lut.is_null() || tests == 0 {
            return 0;
        }
        let entry = (*lut).data as *const DrmColorLut32;
        for i in 0..drm_color_lut32_size(lut) {
            let e = core::ptr::read(entry.add(i as usize));
            if (tests & DRM_COLOR_LUT_EQUAL_CHANNELS) != 0 && (e.red != e.blue || e.red != e.green)
            {
                return -EINVAL;
            }
            if i > 0 && (tests & DRM_COLOR_LUT_NON_DECREASING) != 0 {
                let p = core::ptr::read(entry.add(i as usize - 1));
                if e.red < p.red || e.green < p.green || e.blue < p.blue {
                    return -EINVAL;
                }
            }
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use core::mem::zeroed;
    use std::{boxed::Box, ffi::CStr, sync::Mutex, vec::Vec};

    use super::*;
    use crate::{
        drm_color_mgmt_types::{
            DRM_COLOR_YCBCR_BT601, DRM_COLOR_YCBCR_BT2020, DRM_COLOR_YCBCR_FULL_RANGE,
            DRM_COLOR_YCBCR_LIMITED_RANGE,
        },
        drm_property_types::DrmPropertyBlob,
    };

    #[derive(Default)]
    struct LutRecorder {
        calls: Mutex<Vec<(u32, u16, u16, u16)>>,
    }

    /// # Safety
    /// `crtc` must point to a live `LutRecorder`.
    unsafe extern "C" fn record(crtc: *mut DrmCrtc, i: u32, r: u16, g: u16, b: u16) {
        // SAFETY: these tests pass the address of a live `LutRecorder` as the
        // otherwise opaque CRTC pointer, and the fill helper calls us inline.
        let recorder = unsafe { &*crtc.cast::<LutRecorder>() };
        recorder
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((i, r, g, b));
    }

    /// A blob whose payload is `data` (its length is the byte length of `data`).
    fn blob_of<T>(data: &'static mut [T]) -> DrmPropertyBlob {
        // SAFETY: `zeroed` produces a valid all-zeros representation for `DrmPropertyBlob` POD fields.
        let mut blob: DrmPropertyBlob = unsafe { zeroed() };
        blob.length = core::mem::size_of_val(data);
        blob.data = data.as_mut_ptr() as *mut core::ffi::c_void;
        blob
    }

    #[test]
    fn ctm_s31_32_to_qm_n_scales_and_clamps() {
        // 2.5 in S31.32 is 0x2_8000_0000; in Q16.16 it is 2.5 * 2^16.
        assert_eq!(drm_color_ctm_s31_32_to_qm_n(0x2_8000_0000, 16, 16), 163_840);
        // The sign bit is carried through as two's complement.
        assert_eq!(
            drm_color_ctm_s31_32_to_qm_n((1u64 << 63) | 0x2_8000_0000, 16, 16),
            163_840u64.wrapping_neg()
        );
        // Magnitudes beyond the Q(m).(n) range clamp to the largest value.
        assert_eq!(drm_color_ctm_s31_32_to_qm_n(1u64 << 40, 4, 28), 0x7FFF_FFFF);
    }

    #[test]
    fn lut_check_enforces_equal_channels_and_monotonic_entries() {
        let lut: &'static mut [DrmColorLut] = Box::leak(
            std::vec![
                DrmColorLut {
                    red: 1,
                    green: 1,
                    blue: 1,
                    reserved: 0
                },
                DrmColorLut {
                    red: 2,
                    green: 2,
                    blue: 2,
                    reserved: 0
                },
            ]
            .into_boxed_slice(),
        );
        let blob = blob_of(lut);
        let blob_ptr: *const DrmPropertyBlob = &blob;
        // SAFETY: `blob` has static lifetime and valid payload pointer; tests verify LUT check logic inline.
        unsafe {
            assert_eq!(drm_color_lut_check(blob_ptr, 0), 0);
            assert_eq!(
                drm_color_lut_check(core::ptr::null(), DRM_COLOR_LUT_EQUAL_CHANNELS),
                0
            );
            assert_eq!(
                drm_color_lut_check(
                    blob_ptr,
                    DRM_COLOR_LUT_EQUAL_CHANNELS | DRM_COLOR_LUT_NON_DECREASING
                ),
                0
            );
            // Unequal channels in one entry.
            (*(blob.data as *mut DrmColorLut)).green = 9;
            assert_eq!(
                drm_color_lut_check(blob_ptr, DRM_COLOR_LUT_EQUAL_CHANNELS),
                -EINVAL
            );
            (*(blob.data as *mut DrmColorLut)).green = 1;
            // A decreasing entry.
            (*(blob.data as *mut DrmColorLut).add(1)).blue = 0;
            assert_eq!(
                drm_color_lut_check(blob_ptr, DRM_COLOR_LUT_NON_DECREASING),
                -EINVAL
            );
        }
        // SAFETY: `blob` is a local `DrmPropertyBlob` whose length was initialized to `2 * size_of::<DrmColorLut>()`.
        assert_eq!(unsafe { drm_color_lut_size(&blob) }, 2);
    }

    #[test]
    fn gamma_888_fill_expands_bytes_and_covers_256_entries() {
        let recorder = LutRecorder::default();
        let crtc = (&recorder as *const LutRecorder)
            .cast_mut()
            .cast::<DrmCrtc>();
        // SAFETY: this helper forwards `crtc` to `record`, which reinterprets
        // it as the live recorder above and never as a real `DrmCrtc`.
        unsafe { drm_crtc_fill_gamma_888(crtc, record) };
        let calls = recorder
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(calls.len(), 256);
        assert_eq!(calls[7], (7, 0x0707, 0x0707, 0x0707));
        assert_eq!(calls[255], (255, 0xFFFF, 0xFFFF, 0xFFFF));
    }

    #[test]
    fn palette_332_visits_every_index_once() {
        let recorder = LutRecorder::default();
        let crtc = (&recorder as *const LutRecorder)
            .cast_mut()
            .cast::<DrmCrtc>();
        // SAFETY: this helper forwards `crtc` to `record`, which reinterprets
        // it as the live recorder above and never as a real `DrmCrtc`.
        unsafe { drm_crtc_fill_palette_332(crtc, record) };
        let calls = recorder
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(calls.len(), 8 * 8 * 4);
        let mut seen = [false; 256];
        for (i, ..) in &calls {
            assert!(!seen[*i as usize]);
            seen[*i as usize] = true;
        }
        assert!(seen.iter().all(|s| *s));
    }

    #[test]
    fn color_names_match_the_c_tables() {
        // SAFETY: String pointers returned from enum name getters point to valid static null-terminated byte strings.
        unsafe {
            let name = |p: *const c_char| CStr::from_ptr(p).to_str().unwrap();
            assert_eq!(
                name(drm_get_color_encoding_name(DRM_COLOR_YCBCR_BT601)),
                "ITU-R BT.601 YCbCr"
            );
            assert_eq!(
                name(drm_get_color_encoding_name(DRM_COLOR_YCBCR_BT2020)),
                "ITU-R BT.2020 YCbCr"
            );
            assert_eq!(name(drm_get_color_encoding_name(99)), "unknown");
            assert_eq!(
                name(drm_get_color_range_name(DRM_COLOR_YCBCR_LIMITED_RANGE)),
                "YCbCr limited range"
            );
            assert_eq!(
                name(drm_get_color_range_name(DRM_COLOR_YCBCR_FULL_RANGE)),
                "YCbCr full range"
            );
        }
    }
}
