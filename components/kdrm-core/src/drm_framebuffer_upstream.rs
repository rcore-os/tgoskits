// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_framebuffer.c`.
//!
//! Source-faithful transcription of Linux 7.2.3: the ADDFB/ADDFB2, RMFB,
//! CLOSEFB, GETFB/GETFB2 and DIRTYFB ioctl handlers, framebuffer init,
//! lookup, refcounting and removal.
//!
//! Deviations:
//! - `drm_mode_rmfb()` and `drm_fb_release()` run the deferred removal
//!   (`drm_mode_rmfb_work_fn`) inline. The Linux `schedule_work()` +
//!   `flush_work()` pair waits for that same function before returning, so
//!   the observable ordering is unchanged.
//! - The debugfs `framebuffer` info file is not ported (CONFIG_DEBUG_FS is off
//!   for this target).
//! - ioctl `data` pointers are kernel-resident (the uAPI copy has already been
//!   made), except `DIRTYFB` clips, which are copied from the user pointer.

use alloc::format;
use core::{
    ffi::{c_char, c_void},
    mem::{offset_of, size_of},
};

use drm_kapi::{
    assert::warn_on,
    config::{
        EDEADLK, EFAULT, EINVAL, ENODEV, ENOENT, ENOMEM, ENOSPC, EOPNOTSUPP, ERANGE, ERR_PTR,
        IS_ERR, PTR_ERR,
    },
};

// upstream: include/uapi/asm-generic/errno-base.h (not exported by tk-linux-kapi config)
const ENOSYS: i32 = 38;
use drm_kapi::{
    list::{list_add, list_add_tail, list_del, list_del_init, list_empty},
    memory::{kfree, kref_get, kref_put, kzalloc_objs},
    mutex::{mutex_lock, mutex_unlock},
    print::{DrmLogLevel, drm_log_at},
    types::{Kref, ListHead},
};

use crate::{
    drm_atomic_types::DrmAtomicCommit,
    drm_connector_types::DrmConnectorState,
    drm_crtc_types::{DrmCrtc, DrmCrtcState},
    drm_damage_helper_upstream::drm_atomic_commit_put,
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_fourcc_types::DrmFormatInfo,
    drm_fourcc_upstream::{
        __drm_format_info, drm_driver_legacy_fb_format, drm_format_info_bpp,
        drm_format_info_min_pitch, drm_get_format_info,
    },
    drm_framebuffer_types::{DrmFramebuffer, DrmFramebufferFuncs},
    drm_mode_object_types::DrmModeObject,
    drm_mode_object_upstream::{DRIVER_ATOMIC, DRIVER_MODESET},
    drm_modes_types::DrmDisplayMode,
    drm_modeset_lock_types::DrmModesetAcquireCtx,
    drm_modeset_lock_upstream::{
        drm_modeset_acquire_fini, drm_modeset_acquire_init, drm_modeset_backoff,
        drm_modeset_drop_locks, drm_modeset_lock_all, drm_modeset_lock_all_ctx,
        drm_modeset_unlock_all,
    },
    drm_plane_types::{DrmPlane, DrmPlaneState},
    drm_print_types::DrmPrinter,
    uapi_drm_consts::*,
    uapi_drm_mode_types::{DrmModeClosefb, DrmModeFbCmd, DrmModeFbCmd2, DrmModeFbDirtyCmd},
    uapi_drm_types::DrmClipRect,
};

// upstream: include/drm/drm_capabilities CAP_SYS_ADMIN
const CAP_SYS_ADMIN: i32 = 21;

// upstream: drm_internal.h DRM_FRAMEBUFFER_HAS_HANDLE_REF(_idx)
#[inline]
const fn drm_framebuffer_has_handle_ref(idx: u32) -> u32 {
    1 << idx
}

// upstream: include/drm/drm_fourcc.h drm_format_info_plane_width()
#[inline]
fn drm_format_info_plane_width(info: *const DrmFormatInfo, width: i32, plane: i32) -> i32 {
    // SAFETY: `info` is checked non-null by the short-circuiting check; read `num_planes`.
    if info.is_null() || plane >= unsafe { (*info).num_planes } as i32 {
        return 0;
    }
    if plane == 0 {
        return width;
    }
    // SAFETY: `info` was checked non-null above; read horizontal subsampling factor `hsub`.
    let hsub = unsafe { (*info).hsub } as i32;
    (width + hsub - 1) / hsub
}

// upstream: include/drm/drm_fourcc.h drm_format_info_plane_height()
#[inline]
fn drm_format_info_plane_height(info: *const DrmFormatInfo, height: i32, plane: i32) -> i32 {
    // SAFETY: `info` is checked non-null by the short-circuiting check; read `num_planes`.
    if info.is_null() || plane >= unsafe { (*info).num_planes } as i32 {
        return 0;
    }
    if plane == 0 {
        return height;
    }
    // SAFETY: `info` was checked non-null above; read vertical subsampling factor `vsub`.
    let vsub = unsafe { (*info).vsub } as i32;
    (height + vsub - 1) / vsub
}

// upstream: include/drm/drm_device.h drm_core_check_feature()
#[inline]
fn drm_core_check_feature(dev: *const DrmDevice, feature: u32) -> bool {
    // SAFETY: `dev` is valid and live; read `driver_features`.
    (unsafe { (*dev).driver_features }) & feature != 0
}

// upstream: include/drm/drm_drv.h drm_drv_uses_atomic_modeset()
#[inline]
fn drm_drv_uses_atomic_modeset(dev: *const DrmDevice) -> bool {
    drm_core_check_feature(dev, DRIVER_ATOMIC)
}

// upstream: include/drm/drm_framebuffer.h drm_framebuffer_read_refcount()
#[inline]
/// # Safety
///
/// `fb` must point to a live, properly aligned `DrmFramebuffer`.
unsafe fn drm_framebuffer_read_refcount(fb: *const DrmFramebuffer) -> u32 {
    // SAFETY: `fb` is a valid pointer to a live `DrmFramebuffer`; read its atomic refcount.
    unsafe { drm_kapi::memory::kref_read(&(*fb).base.refcount) }
}

/// Retain a framebuffer reference across driver-owned asynchronous work.
///
/// # Safety
/// `fb` must be null or point to a live framebuffer, and the caller must
/// already own a reference which keeps the framebuffer storage alive for the
/// duration of this call. The returned reference must later be released
/// exactly once with [`drm_framebuffer_put`].
#[inline]
pub unsafe fn drm_framebuffer_get(fb: *mut DrmFramebuffer) -> *mut DrmFramebuffer {
    if fb.is_null() {
        return fb;
    }
    // SAFETY: required by this API's contract; the caller retains a live
    // framebuffer reference while incrementing the embedded kref.
    unsafe { kref_get(core::ptr::addr_of_mut!((*fb).base.refcount)) };
    fb
}

// upstream: include/drm/drm_framebuffer.h drm_framebuffer_put()
#[inline]
/// # Safety
/// `fb` must be null or point to a live framebuffer for which the caller owns
/// one reference. The final put may destroy and free the framebuffer.
pub unsafe fn drm_framebuffer_put(fb: *mut DrmFramebuffer) {
    if fb.is_null() {
        return;
    }
    // SAFETY: `fb` is a valid pointer to a live `DrmFramebuffer`; drop reference via `kref_put`.
    unsafe {
        kref_put(
            core::ptr::addr_of_mut!((*fb).base.refcount),
            drm_framebuffer_free,
        )
    };
}

// upstream: include/drm/drm_mode_object.h obj_to_fb()
#[inline]
/// # Safety
///
/// `obj` must point to the `base` field of an allocated `DrmFramebuffer`.
unsafe fn obj_to_fb(obj: *mut DrmModeObject) -> *mut DrmFramebuffer {
    // SAFETY: `obj` points to the `base` member of `DrmFramebuffer`; compute enclosing pointer.
    unsafe { (obj as *mut u8).sub(offset_of!(DrmFramebuffer, base)) as *mut DrmFramebuffer }
}

// Cross-module callees (mode objects, GEM handles, atomic, CRTC/plane
// force-disable, DRM auth) provided by sibling DRM modules.
// # Safety
//
// External C ABI symbol declarations linked from kernel or runtime modules.
unsafe extern "C" {
    fn __drm_mode_object_add(
        dev: *mut DrmDevice,
        obj: *mut DrmModeObject,
        obj_type: u32,
        register: bool,
        free_cb: Option<unsafe extern "C" fn(*mut Kref)>,
    ) -> i32;
    fn drm_mode_object_register(dev: *mut DrmDevice, obj: *mut DrmModeObject);
    fn drm_mode_object_unregister(dev: *mut DrmDevice, obj: *mut DrmModeObject);
    fn __drm_mode_object_find(
        dev: *mut DrmDevice,
        file_priv: *mut DrmFile,
        id: u32,
        obj_type: u32,
    ) -> *mut DrmModeObject;
    fn drm_gem_handle_create(file_priv: *mut DrmFile, obj: *mut c_void, handlep: *mut u32) -> i32;
    fn drm_gem_handle_delete(file_priv: *mut DrmFile, handle: u32) -> i32;
    fn drm_gem_object_handle_get_if_exists_unlocked(obj: *mut c_void) -> bool;
    fn drm_gem_object_handle_put_unlocked(obj: *mut c_void);
    fn drm_gem_print_info(p: *mut DrmPrinter, indent: u32, obj: *const c_void);
    fn drm_is_current_master(file_priv: *mut DrmFile) -> bool;
    fn drm_atomic_commit_alloc(dev: *mut DrmDevice) -> *mut DrmAtomicCommit;
    fn drm_atomic_commit_clear(state: *mut DrmAtomicCommit);
    fn drm_atomic_add_affected_connectors(state: *mut DrmAtomicCommit, crtc: *mut DrmCrtc) -> i32;
    fn drm_atomic_set_mode_for_crtc(
        crtc_state: *mut DrmCrtcState,
        mode: *const DrmDisplayMode,
    ) -> i32;
    fn drm_atomic_set_fb_for_plane(plane_state: *mut DrmPlaneState, fb: *mut DrmFramebuffer);
    fn drm_atomic_set_crtc_for_plane(plane_state: *mut DrmPlaneState, crtc: *mut DrmCrtc) -> i32;
    fn drm_atomic_set_crtc_for_connector(
        conn_state: *mut DrmConnectorState,
        crtc: *mut DrmCrtc,
    ) -> i32;
    fn drm_atomic_commit(state: *mut DrmAtomicCommit) -> i32;
    fn drm_atomic_get_plane_state(
        state: *mut DrmAtomicCommit,
        plane: *mut DrmPlane,
    ) -> *mut DrmPlaneState;
    fn drm_crtc_force_disable(crtc: *mut DrmCrtc) -> i32;
    fn drm_plane_force_disable(plane: *mut DrmPlane);
}

// upstream: drm_framebuffer.c drm_framebuffer_check_src_coords()
#[unsafe(no_mangle)]
/// # Safety
///
/// `fb` must point to a valid, initialized `DrmFramebuffer` whose `width` and
/// `height` fields can be safely read concurrently or while caller holds the
/// appropriate modeset locks.
pub unsafe extern "C" fn drm_framebuffer_check_src_coords(
    src_x: u32,
    src_y: u32,
    src_w: u32,
    src_h: u32,
    fb: *const DrmFramebuffer,
) -> i32 {
    // SAFETY: `fb` is valid and live; dereference to immutable reference.
    let fb = unsafe { &*fb };
    let fb_width: u32 = fb.width << 16;
    let fb_height: u32 = fb.height << 16;

    if src_w > fb_width
        || src_x > fb_width - src_w
        || src_h > fb_height
        || src_y > fb_height - src_h
    {
        drm_log_at(
            DrmLogLevel::Debug,
            "drm_framebuffer",
            file!(),
            line!(),
            &format!(
                "Invalid source coordinates {}.{:06}x{}.{:06}+{}.{:06}+{}.{:06} (fb {}x{})",
                src_w >> 16,
                ((src_w & 0xffff) * 15625) >> 10,
                src_h >> 16,
                ((src_h & 0xffff) * 15625) >> 10,
                src_x >> 16,
                ((src_x & 0xffff) * 15625) >> 10,
                src_y >> 16,
                ((src_y & 0xffff) * 15625) >> 10,
                fb.width,
                fb.height
            ),
        );
        return -ENOSPC;
    }
    0
}

// upstream: drm_framebuffer.c drm_mode_addfb()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `or_` must point to a valid, readable and writable `DrmModeFbCmd` buffer.
/// - `file_priv` must point to an open `DrmFile` associated with the calling process.
pub unsafe extern "C" fn drm_mode_addfb(
    dev: *mut DrmDevice,
    or_: *mut DrmModeFbCmd,
    file_priv: *mut DrmFile,
) -> i32 {
    if !drm_core_check_feature(dev, DRIVER_MODESET) {
        return -EOPNOTSUPP;
    }
    // SAFETY: `or_` points to valid, mutable `DrmModeFbCmd` buffer passed from caller.
    let or_ = unsafe { &mut *or_ };

    let mut r = DrmModeFbCmd2 {
        fb_id: 0,
        width: 0,
        height: 0,
        pixel_format: 0,
        flags: 0,
        handles: [0; 4],
        pitches: [0; 4],
        offsets: [0; 4],
        modifier: [0; 4],
    };
    // SAFETY: `dev` is valid; determine legacy pixel format from bpp and depth.
    r.pixel_format = unsafe { drm_driver_legacy_fb_format(dev, or_.bpp, or_.depth) };
    if r.pixel_format == DRM_FORMAT_INVALID {
        drm_log_at(
            DrmLogLevel::Debug,
            "drm_framebuffer",
            file!(),
            line!(),
            &format!("bad {{bpp:{}, depth:{}}}", or_.bpp, or_.depth),
        );
        return -EINVAL;
    }

    r.fb_id = or_.fb_id;
    r.width = or_.width;
    r.height = or_.height;
    r.pitches[0] = or_.pitch;
    r.handles[0] = or_.handle;

    let ret =
        // SAFETY: `r` is a stack-allocated `DrmModeFbCmd2`; call `drm_mode_addfb2`.
        unsafe { drm_mode_addfb2(dev, &mut r as *mut DrmModeFbCmd2 as *mut c_void, file_priv) };
    if ret != 0 {
        return ret;
    }

    or_.fb_id = r.fb_id;
    0
}

// upstream: drm_framebuffer.c drm_mode_addfb_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `data` must point to a valid, readable and writable `DrmModeFbCmd` ioctl payload.
/// - `file_priv` must point to an open `DrmFile` associated with the calling process.
pub unsafe extern "C" fn drm_mode_addfb_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: Forward addfb ioctl request to `drm_mode_addfb` with verified pointers.
    unsafe { drm_mode_addfb(dev, data as *mut DrmModeFbCmd, file_priv) }
}

// upstream: drm_framebuffer.c framebuffer_check()
/// # Safety
///
/// `dev`, `info`, and `r` must be valid, live pointers.
unsafe fn framebuffer_check(
    dev: *mut DrmDevice,
    info: *const DrmFormatInfo,
    r: *const DrmModeFbCmd2,
) -> i32 {
    // SAFETY: `r` points to valid, caller-owned `DrmModeFbCmd2`.
    let r = unsafe { &*r };
    // SAFETY: `info` points to valid `DrmFormatInfo`.
    let info_ref = unsafe { &*info };

    if r.width == 0 {
        dbg_kms(dev, &format!("bad framebuffer width {}", r.width));
        return -EINVAL;
    }

    if r.height == 0 {
        dbg_kms(dev, &format!("bad framebuffer height {}", r.height));
        return -EINVAL;
    }

    for i in 0..info_ref.num_planes as usize {
        let width = drm_format_info_plane_width(info, r.width as i32, i as i32) as u32;
        let height = drm_format_info_plane_height(info, r.height as i32, i as i32) as u32;
        // SAFETY: `i` is within `0..info_ref.num_planes`; read `char_per_block[i]`.
        let block_size = unsafe { info_ref._anon_6.char_per_block[i] };
        // SAFETY: `info` is valid; compute minimum pitch for plane `i`.
        let min_pitch = unsafe { drm_format_info_min_pitch(info, i as i32, width) };

        if block_size == 0 && r.modifier[i] == DRM_FORMAT_MOD_LINEAR {
            dbg_kms(
                dev,
                &format!("Format requires non-linear modifier for plane {}", i),
            );
            return -EINVAL;
        }

        if r.handles[i] == 0 {
            dbg_kms(dev, &format!("no buffer object handle for plane {}", i));
            return -EINVAL;
        }

        if min_pitch > u32::MAX as u64 {
            return -ERANGE;
        }

        if (height as u64) * (r.pitches[i] as u64) + (r.offsets[i] as u64) > u32::MAX as u64 {
            return -ERANGE;
        }

        if block_size != 0 && (r.pitches[i] as u64) < min_pitch {
            dbg_kms(dev, &format!("bad pitch {} for plane {}", r.pitches[i], i));
            return -EINVAL;
        }

        if r.modifier[i] != 0 && r.flags & DRM_MODE_FB_MODIFIERS == 0 {
            dbg_kms(
                dev,
                &format!("bad fb modifier {} for plane {}", r.modifier[i], i),
            );
            return -EINVAL;
        }

        if r.flags & DRM_MODE_FB_MODIFIERS != 0 && r.modifier[i] != r.modifier[0] {
            dbg_kms(
                dev,
                &format!("bad fb modifier {} for plane {}", r.modifier[i], i),
            );
            return -EINVAL;
        }

        match r.modifier[i] {
            DRM_FORMAT_MOD_SAMSUNG_64_32_TILE
                if (r.pixel_format != DRM_FORMAT_NV12
                    || !width.is_multiple_of(128)
                    || !height.is_multiple_of(32)
                    || r.pitches[i] % 128 != 0) =>
            {
                dbg_kms(dev, &format!("bad modifier data for plane {}", i));
                return -EINVAL;
            }
            _ => {}
        }
    }

    for i in info_ref.num_planes as usize..4 {
        if r.modifier[i] != 0 {
            dbg_kms(dev, &format!("non-zero modifier for unused plane {}", i));
            return -EINVAL;
        }

        if r.flags & DRM_MODE_FB_MODIFIERS == 0 {
            continue;
        }

        if r.handles[i] != 0 {
            dbg_kms(dev, &format!("buffer object handle for unused plane {}", i));
            return -EINVAL;
        }

        if r.pitches[i] != 0 {
            dbg_kms(dev, &format!("non-zero pitch for unused plane {}", i));
            return -EINVAL;
        }

        if r.offsets[i] != 0 {
            dbg_kms(dev, &format!("non-zero offset for unused plane {}", i));
            return -EINVAL;
        }
    }

    0
}

// drm_dbg_kms(dev, ...) routed to the kernel log.
fn dbg_kms(dev: *const DrmDevice, msg: &str) {
    let _ = dev;
    drm_log_at(DrmLogLevel::Debug, "drm_kms", file!(), line!(), msg);
}

// upstream: drm_framebuffer.c drm_internal_framebuffer_create()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an initialized `DrmDevice` with valid `mode_config.funcs->fb_create`.
/// - `r` must point to a valid, readable `DrmModeFbCmd2` command struct.
/// - `file_priv` must point to a live `DrmFile` owning the handles specified in `r`.
///
/// On success, returns a newly referenced `DrmFramebuffer` pointer; on failure, returns an ERR_PTR.
pub unsafe extern "C" fn drm_internal_framebuffer_create(
    dev: *mut DrmDevice,
    r: *const DrmModeFbCmd2,
    file_priv: *mut DrmFile,
) -> *mut DrmFramebuffer {
    // SAFETY: `dev` is valid; get pointer to its `mode_config`.
    let config = unsafe { core::ptr::addr_of_mut!((*dev).mode_config) };
    // SAFETY: `r` points to valid `DrmModeFbCmd2`.
    let rr = unsafe { &*r };

    if rr.flags & !(DRM_MODE_FB_INTERLACED | DRM_MODE_FB_MODIFIERS) != 0 {
        dbg_kms(dev, &format!("bad framebuffer flags 0x{:08x}", rr.flags));
        return ERR_PTR(-EINVAL);
    }

    // SAFETY: `config` is valid; read min/max width and height constraints.
    unsafe {
        if (*config).min_width > rr.width || rr.width > (*config).max_width {
            dbg_kms(
                dev,
                &format!(
                    "bad framebuffer width {}, should be >= {} && <= {}",
                    rr.width,
                    (*config).min_width,
                    (*config).max_width
                ),
            );
            return ERR_PTR(-EINVAL);
        }
        if (*config).min_height > rr.height || rr.height > (*config).max_height {
            dbg_kms(
                dev,
                &format!(
                    "bad framebuffer height {}, should be >= {} && <= {}",
                    rr.height,
                    (*config).min_height,
                    (*config).max_height
                ),
            );
            return ERR_PTR(-EINVAL);
        }
    }

    // SAFETY: `config` is valid; check `fb_modifiers_not_supported` flag.
    if rr.flags & DRM_MODE_FB_MODIFIERS != 0 && unsafe { (*config).fb_modifiers_not_supported } {
        dbg_kms(dev, "driver does not support fb modifiers");
        return ERR_PTR(-EINVAL);
    }

    if __drm_format_info(rr.pixel_format).is_null() {
        dbg_kms(
            dev,
            &format!("bad framebuffer format {:#010x}", rr.pixel_format),
        );
        return ERR_PTR(-EINVAL);
    }

    // SAFETY: `dev` is valid; look up format info for pixel format and modifier.
    let info = unsafe { drm_get_format_info(dev, rr.pixel_format, rr.modifier[0]) };
    // SAFETY: Validate framebuffer dimensions, pitches, and modifiers against format info.
    let ret = unsafe { framebuffer_check(dev, info, r) };
    if ret != 0 {
        return ERR_PTR(ret);
    }

    // SAFETY: `config` is valid; read driver `fb_create` function pointer.
    let fb_create = unsafe { (*(*config).funcs).fb_create }.expect("fb_create");
    // SAFETY: Invoke driver's `fb_create` callback with verified arguments.
    let fb = unsafe {
        fb_create(
            dev,
            file_priv,
            info as *mut DrmFormatInfo,
            r as *mut DrmModeFbCmd2,
        )
    };
    if IS_ERR(fb) {
        dbg_kms(dev, "could not create framebuffer");
        return fb;
    }

    fb
}

// upstream: drm_framebuffer.c drm_mode_addfb2()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `data` must point to a valid, readable and writable `DrmModeFbCmd2` buffer whose `fb_id` field will be written on success.
/// - `file_priv` must point to an open `DrmFile` associated with the calling process.
pub unsafe extern "C" fn drm_mode_addfb2(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    if !drm_core_check_feature(dev, DRIVER_MODESET) {
        return -EOPNOTSUPP;
    }

    let r = data as *mut DrmModeFbCmd2;
    // SAFETY: Create internal framebuffer object from `r`.
    let fb = unsafe { drm_internal_framebuffer_create(dev, r, file_priv) };
    if IS_ERR(fb) {
        return PTR_ERR(fb);
    }

    // SAFETY: `fb` is a valid pointer to newly created framebuffer; read its ID.
    let id = unsafe { (*fb).base.id };
    dbg_kms(dev, &format!("[FB:{}]", id));
    // SAFETY: `r` is a valid, mutable pointer; store assigned `fb_id`.
    unsafe { (*r).fb_id = id };

    // SAFETY: `file_priv` is valid; get pointer to `fbs_lock` mutex.
    let fbs_lock = unsafe { core::ptr::addr_of_mut!((*file_priv).fbs_lock) };
    // SAFETY: Acquire `fbs_lock` protecting the file's framebuffer list.
    unsafe { mutex_lock(fbs_lock) };
    // SAFETY: Link newly created framebuffer's `filp_head` to `file_priv->fbs`.
    unsafe {
        list_add(
            core::ptr::addr_of_mut!((*fb).filp_head),
            core::ptr::addr_of_mut!((*file_priv).fbs),
        )
    };
    // SAFETY: Release `fbs_lock` after list insertion.
    unsafe { mutex_unlock(fbs_lock) };

    0
}

// upstream: drm_framebuffer.c drm_mode_addfb2_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice` supporting KMS mode config.
/// - `data` must point to a valid, readable and writable `DrmModeFbCmd2` ioctl payload.
/// - `file_priv` must point to an open `DrmFile` associated with the calling process.
pub unsafe extern "C" fn drm_mode_addfb2_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // `__BIG_ENDIAN` is not set on x86_64.
    // SAFETY: Forward addfb2 ioctl request to `drm_mode_addfb2`.
    unsafe { drm_mode_addfb2(dev, data, file_priv) }
}

// upstream: drm_framebuffer.c struct drm_mode_rmfb_work
#[repr(C)]
pub struct DrmModeRmfbWork {
    fbs: ListHead,
}

// upstream: drm_framebuffer.c drm_mode_rmfb_work_fn()
/// # Safety
///
/// `arg` must point to a live `DrmModeRmfbWork` structure with an initialized `fbs` list.
unsafe fn drm_mode_rmfb_work_fn(arg: *mut DrmModeRmfbWork) {
    // SAFETY: `arg` points to stack-allocated `DrmModeRmfbWork`; get pointer to `fbs` list head.
    let head = unsafe { core::ptr::addr_of_mut!((*arg).fbs) };
    // SAFETY: `head` is a valid pointer to an initialized `ListHead`.
    while !unsafe { list_empty(&*head) } {
        // list_first_entry(&arg->fbs, struct drm_framebuffer, filp_head)
        // SAFETY: `head` is not empty; read first framebuffer entry via `filp_head` offset.
        let fb = unsafe {
            (*head)
                .next
                .cast::<u8>()
                .sub(offset_of!(DrmFramebuffer, filp_head))
        } as *mut DrmFramebuffer;
        dbg_kms(
            // SAFETY: `fb` is valid; read its device pointer.
            unsafe { (*fb).dev },
            &format!(
                "Removing [FB:{}] from all active usage due to RMFB ioctl",
                // SAFETY: `fb` is valid; read its mode object ID.
                unsafe { (*fb).base.id }
            ),
        );
        // SAFETY: Remove framebuffer from the work item list.
        unsafe { list_del_init(core::ptr::addr_of_mut!((*fb).filp_head)) };
        // SAFETY: Remove framebuffer from all active CRTC/plane pipelines.
        unsafe { drm_framebuffer_remove(fb) };
    }
}

// upstream: drm_framebuffer.c drm_mode_closefb()
/// # Safety
///
/// `fb` and `file_priv` must point to live objects.
unsafe fn drm_mode_closefb(fb: *mut DrmFramebuffer, file_priv: *mut DrmFile) -> i32 {
    // SAFETY: `file_priv` is valid; get pointer to `fbs_lock`.
    let fbs_lock = unsafe { core::ptr::addr_of_mut!((*file_priv).fbs_lock) };
    // SAFETY: Acquire `fbs_lock` protecting `file_priv->fbs`.
    unsafe { mutex_lock(fbs_lock) };

    // SAFETY: `file_priv` is valid; get pointer to `fbs` list head.
    let head = unsafe { core::ptr::addr_of_mut!((*file_priv).fbs) };
    let mut found = false;
    // SAFETY: Read first link in `file_priv->fbs` under lock.
    let mut link = unsafe { (*head).next };
    while link != head {
        // SAFETY: Compute `DrmFramebuffer` pointer from `filp_head` link.
        let fbl = unsafe {
            link.cast::<u8>().sub(offset_of!(DrmFramebuffer, filp_head)) as *mut DrmFramebuffer
        };
        if fb == fbl {
            found = true;
        }
        // SAFETY: Read next link in `file_priv->fbs` under lock.
        link = unsafe { (*link).next };
    }

    if !found {
        // SAFETY: Release `fbs_lock` on error path when framebuffer is not found.
        unsafe { mutex_unlock(fbs_lock) };
        return -ENOENT;
    }

    // SAFETY: Unlink `fb->filp_head` from `file_priv->fbs`.
    unsafe { list_del_init(core::ptr::addr_of_mut!((*fb).filp_head)) };
    // SAFETY: Release `fbs_lock` after unlinking framebuffer.
    unsafe { mutex_unlock(fbs_lock) };

    // SAFETY: Drop the file's reference to `fb`.
    unsafe { drm_framebuffer_put(fb) };
    0
}

// upstream: drm_framebuffer.c drm_mode_rmfb()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `file_priv` must point to a valid `DrmFile` owning or permitted to close framebuffer `fb_id`.
pub unsafe extern "C" fn drm_mode_rmfb(
    dev: *mut DrmDevice,
    fb_id: u32,
    file_priv: *mut DrmFile,
) -> i32 {
    if !drm_core_check_feature(dev, DRIVER_MODESET) {
        return -EOPNOTSUPP;
    }

    // SAFETY: Look up framebuffer by ID in `dev` mode config for `file_priv`.
    let fb = unsafe { drm_framebuffer_lookup(dev, file_priv, fb_id) };
    if fb.is_null() {
        return -ENOENT;
    }

    // SAFETY: Close framebuffer in `file_priv`, unlinking from file list and dropping reference.
    let ret = unsafe { drm_mode_closefb(fb, file_priv) };
    if ret != 0 {
        // SAFETY: Drop lookup reference on close error.
        unsafe { drm_framebuffer_put(fb) };
        return ret;
    }

    // SAFETY: Read refcount to check if framebuffer has active references beyond the lookup ref.
    if unsafe { drm_framebuffer_read_refcount(fb) } > 1 {
        let mut arg = DrmModeRmfbWork {
            fbs: ListHead {
                next: core::ptr::null_mut(),
                prev: core::ptr::null_mut(),
            },
        };
        let arg_head = core::ptr::addr_of_mut!(arg.fbs);
        // SAFETY: Initialize list head for deferred rmfb work item.
        unsafe {
            (*arg_head).next = arg_head;
            (*arg_head).prev = arg_head;
        }
        // SAFETY: Verify framebuffer was unlinked from file list.
        warn_on(!unsafe { list_empty(&(*fb).filp_head) });
        // SAFETY: Add framebuffer to work item list.
        unsafe { list_add_tail(core::ptr::addr_of_mut!((*fb).filp_head), arg_head) };
        // schedule_work() + flush_work(): see the module note.
        // SAFETY: Work list is non-empty; process removal of framebuffers from active pipelines.
        unsafe { drm_mode_rmfb_work_fn(&mut arg as *mut DrmModeRmfbWork) };
    } else {
        // SAFETY: Drop lookup reference to fb.
        unsafe { drm_framebuffer_put(fb) };
    }

    0
}

// upstream: drm_framebuffer.c drm_mode_rmfb_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `data` must point to a readable `u32` containing the framebuffer object ID to remove.
/// - `file_priv` must point to an open `DrmFile` associated with the calling process.
pub unsafe extern "C" fn drm_mode_rmfb_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: `data` points to `DrmModeFbCmd`; read `fb_id`.
    let fb_id = unsafe { *(data as *mut u32) };
    // SAFETY: Call `drm_mode_rmfb` with verified arguments.
    unsafe { drm_mode_rmfb(dev, fb_id, file_priv) }
}

// upstream: drm_framebuffer.c drm_mode_closefb_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `data` must point to a valid, readable `DrmModeClosefb` ioctl payload.
/// - `file_priv` must point to an open `DrmFile` associated with the calling process.
pub unsafe extern "C" fn drm_mode_closefb_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    let r = data as *mut DrmModeClosefb;

    if !drm_core_check_feature(dev, DRIVER_MODESET) {
        return -EOPNOTSUPP;
    }

    // SAFETY: `r` is a valid pointer to `DrmModeClosefb`; verify `pad` is zero.
    if unsafe { (*r).pad } != 0 {
        return -EINVAL;
    }

    // SAFETY: Look up framebuffer by ID in `dev` mode config for `file_priv`.
    let fb = unsafe { drm_framebuffer_lookup(dev, file_priv, (*r).fb_id) };
    if fb.is_null() {
        return -ENOENT;
    }

    // SAFETY: Close framebuffer in `file_priv`, unlinking from file list and dropping reference.
    let ret = unsafe { drm_mode_closefb(fb, file_priv) };
    // SAFETY: Drop lookup reference to `fb`.
    unsafe { drm_framebuffer_put(fb) };
    ret
}

// upstream: drm_framebuffer.c drm_mode_getfb()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `data` must point to a valid, readable and writable `DrmModeFbCmd` payload where `fb_id` is supplied on input and metadata written on output.
/// - `file_priv` must point to an open `DrmFile` associated with the calling process.
pub unsafe extern "C" fn drm_mode_getfb(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    let r = data as *mut DrmModeFbCmd;

    if !drm_core_check_feature(dev, DRIVER_MODESET) {
        return -EOPNOTSUPP;
    }

    // SAFETY: Look up framebuffer by ID in `dev` mode config for `file_priv`.
    let fb = unsafe { drm_framebuffer_lookup(dev, file_priv, (*r).fb_id) };
    if fb.is_null() {
        return -ENOENT;
    }

    let ret: i32;
    'out: {
        // SAFETY: `fb` is valid; read format pointer.
        let format = unsafe { (*fb).format };
        // SAFETY: `format` is valid; check if number of planes exceeds 1.
        if unsafe { (*format).num_planes } > 1 {
            ret = -EINVAL;
            break 'out;
        }

        // SAFETY: `fb` is valid; check if driver provides `create_handle` callback.
        if unsafe { (*(*fb).funcs).create_handle }.is_none() {
            ret = -ENODEV;
            break 'out;
        }

        // SAFETY: Copy framebuffer dimensions, depth, bpp, and pitch to output struct `r`.
        unsafe {
            (*r).height = (*fb).height;
            (*r).width = (*fb).width;
            (*r).depth = (*format).depth as u32;
            (*r).bpp = drm_format_info_bpp(format, 0);
            (*r).pitch = (*fb).pitches[0];
        }

        // SAFETY: Check if caller is current master.
        if !unsafe { drm_is_current_master(file_priv) }
            // SAFETY: Check CAP_SYS_ADMIN capability.
            && !unsafe { drm_kapi::kernel_services::capable(CAP_SYS_ADMIN) }
        {
            // SAFETY: Clear handle field for unprivileged caller.
            unsafe { (*r).handle = 0 };
            ret = 0;
            break 'out;
        }

        // SAFETY: Driver provides `create_handle` callback; get function pointer.
        let create_handle = unsafe { (*(*fb).funcs).create_handle }.unwrap();
        // SAFETY: Invoke driver `create_handle` callback.
        ret = unsafe { create_handle(fb, file_priv, &mut (*r).handle) };
    }

    // SAFETY: Drop lookup reference to `fb`.
    unsafe { drm_framebuffer_put(fb) };
    ret
}

// upstream: drm_framebuffer.c drm_mode_getfb2_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `data` must point to a valid, readable and writable `DrmModeFbCmd2` payload where `fb_id` is supplied on input and metadata written on output.
/// - `file_priv` must point to an open `DrmFile` associated with the calling process.
pub unsafe extern "C" fn drm_mode_getfb2_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    let r = data as *mut DrmModeFbCmd2;
    let mut ret: i32 = 0;

    if !drm_core_check_feature(dev, DRIVER_MODESET) {
        return -EINVAL;
    }

    // SAFETY: Look up framebuffer by ID.
    let fb = unsafe { drm_framebuffer_lookup(dev, file_priv, (*r).fb_id) };
    if fb.is_null() {
        return -ENOENT;
    }

    // SAFETY: `dev` is valid; check `fb_modifiers_not_supported`.
    let mods_unsupported = unsafe { (*dev).mode_config.fb_modifiers_not_supported };
    // SAFETY: `fb` is valid; read format pointer.
    let format = unsafe { (*fb).format };
    // SAFETY: `format` is valid; read number of planes.
    let num_planes = unsafe { (*format).num_planes } as usize;
    // SAFETY: `fb` is valid; check if driver provides `create_handle` callback.
    let have_create_handle = unsafe { (*(*fb).funcs).create_handle }.is_some();

    'out: {
        // SAFETY: Check if plane 0 has a backing GEM object or driver handle creator.
        if unsafe { (*fb).obj[0] }.is_null() && (num_planes > 1 || !have_create_handle) {
            ret = -ENODEV;
            break 'out;
        }

        // SAFETY: Store width, height, and pixel format in output struct `r`.
        unsafe {
            (*r).height = (*fb).height;
            (*r).width = (*fb).width;
            (*r).pixel_format = (*format).format;
            (*r).flags = 0;
        }
        if !mods_unsupported {
            // SAFETY: Framebuffer uses modifiers; set `DRM_MODE_FB_MODIFIERS` flag in `r`.
            unsafe { (*r).flags |= DRM_MODE_FB_MODIFIERS };
        }

        for i in 0..4 {
            // SAFETY: Copy pitch, offset, and modifier for each plane to output struct `r`.
            unsafe {
                (*r).handles[i] = 0;
                (*r).pitches[i] = 0;
                (*r).offsets[i] = 0;
                (*r).modifier[i] = 0;
            }
        }

        for i in 0..num_planes {
            // SAFETY: Zero out unused plane fields in output struct `r`.
            unsafe {
                (*r).pitches[i] = (*fb).pitches[i];
                (*r).offsets[i] = (*fb).offsets[i];
                if !mods_unsupported {
                    (*r).modifier[i] = (*fb).modifier;
                }
            }
        }

        // SAFETY: Check master credentials before exposing handle.
        if !unsafe { drm_is_current_master(file_priv) }
            // SAFETY: Check CAP_SYS_ADMIN capability.
            && !unsafe { drm_kapi::kernel_services::capable(CAP_SYS_ADMIN) }
        {
            ret = 0;
            break 'out;
        }

        for i in 0..num_planes {
            for j in 0..i {
                // SAFETY: Check if plane `i` shares backing object with plane `j`.
                if unsafe { (*fb).obj[i] } == unsafe { (*fb).obj[j] } {
                    // SAFETY: Reuse already created handle for shared backing object.
                    unsafe { (*r).handles[i] = (*r).handles[j] };
                    break;
                }
            }
            // SAFETY: Check if handle was already set.
            if unsafe { (*r).handles[i] } != 0 {
                continue;
            }

            // SAFETY: `fb` is valid; read backing GEM object for plane `i`.
            let obj = unsafe { (*fb).obj[i] };
            if !obj.is_null() {
                // SAFETY: Create handle in `file_priv` for backing GEM object.
                ret = unsafe {
                    drm_gem_handle_create(file_priv, obj as *mut c_void, &mut (*r).handles[i])
                };
            } else {
                warn_on(i > 0);
                // SAFETY: Driver provides `create_handle` callback; get function pointer.
                let create_handle = unsafe { (*(*fb).funcs).create_handle }.unwrap();
                // SAFETY: Invoke driver `create_handle` callback.
                ret = unsafe { create_handle(fb, file_priv, &mut (*r).handles[i]) };
            }
            if ret != 0 {
                break 'out;
            }
        }
    }

    // out:
    if ret != 0 {
        for i in 0..4 {
            // SAFETY: Check if handle was created during error unwind.
            if unsafe { (*r).handles[i] } != 0 {
                // SAFETY: Delete handle from `file_priv` during error unwind.
                unsafe { drm_gem_handle_delete(file_priv, (*r).handles[i]) };
            }
            for j in i + 1..4 {
                // SAFETY: Check if plane `j` reused handle `i`.
                if unsafe { (*r).handles[j] == (*r).handles[i] } {
                    // SAFETY: Clear reused handle during error unwind.
                    unsafe { (*r).handles[j] = 0 };
                }
            }
        }
    }

    // SAFETY: Drop lookup reference to `fb`.
    unsafe { drm_framebuffer_put(fb) };
    ret
}

// upstream: drm_framebuffer.c drm_mode_dirtyfb_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `data` must point to a valid, readable `DrmModeFbDirtyCmd` payload, including any clip array if `num_clips > 0`.
/// - `file_priv` must point to an open `DrmFile` associated with the calling process.
pub unsafe extern "C" fn drm_mode_dirtyfb_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    let r = data as *mut DrmModeFbDirtyCmd;

    if !drm_core_check_feature(dev, DRIVER_MODESET) {
        return -EOPNOTSUPP;
    }

    // SAFETY: Look up framebuffer by ID.
    let fb = unsafe { drm_framebuffer_lookup(dev, file_priv, (*r).fb_id) };
    if fb.is_null() {
        return -ENOENT;
    }

    // SAFETY: `r` is valid; read `num_clips`.
    let num_clips = unsafe { (*r).num_clips } as i32;
    // SAFETY: `r` is valid; read `clips_ptr`.
    let clips_ptr = unsafe { (*r).clips_ptr } as usize as *mut DrmClipRect;

    let mut clips: *mut DrmClipRect = core::ptr::null_mut();
    let ret: i32;

    'out: {
        if (num_clips == 0) != clips_ptr.is_null() {
            ret = -EINVAL;
            break 'out;
        }

        // SAFETY: `r` is valid; read flags.
        let flags = DRM_MODE_FB_DIRTY_FLAGS & unsafe { (*r).flags };
        if flags & DRM_MODE_FB_DIRTY_ANNOTATE_COPY != 0 && (num_clips % 2) != 0 {
            ret = -EINVAL;
            break 'out;
        }

        if num_clips != 0 && !clips_ptr.is_null() {
            if num_clips < 0 || num_clips as u32 > DRM_MODE_FB_DIRTY_MAX_CLIPS {
                ret = -EINVAL;
                break 'out;
            }

            clips = kzalloc_objs::<DrmClipRect, i32>(num_clips);
            if clips.is_null() {
                ret = -ENOMEM;
                break 'out;
            }

            // SAFETY: Copy clip rectangles from user space buffer into allocated kernel memory.
            let left = unsafe {
                drm_kapi::mm_native::copy_from_user(
                    clips as *mut c_void,
                    clips_ptr as *const c_void,
                    num_clips as usize * size_of::<DrmClipRect>(),
                )
            };
            if left != 0 {
                ret = -EFAULT;
                // SAFETY: Free allocated clips buffer on copy error.
                unsafe { kfree(clips) };
                break 'out;
            }
        }

        // SAFETY: `fb` is valid; read driver `dirty` callback.
        let dirty = unsafe { (*(*fb).funcs).dirty };
        ret = match dirty {
            // SAFETY: Invoke driver `dirty` callback with clips array.
            Some(dirty) => unsafe {
                dirty(fb, file_priv, flags, (*r).color, clips, num_clips as u32)
            },
            None => -ENOSYS,
        };
        if !clips.is_null() {
            // SAFETY: Free allocated clips buffer.
            unsafe { kfree(clips) };
        }
        // SAFETY: Drop lookup reference to `fb`.
        unsafe { drm_framebuffer_put(fb) };
        return ret;
    }

    // out_err1:
    // SAFETY: Drop lookup reference to `fb` on error path.
    unsafe { drm_framebuffer_put(fb) };
    ret
}

// upstream: drm_framebuffer.c drm_fb_release()
#[unsafe(no_mangle)]
/// # Safety
///
/// `priv_` must point to a valid, live `DrmFile` being closed and released. Caller must ensure
/// no concurrent additions to `priv_->fbs` occur during cleanup.
pub unsafe extern "C" fn drm_fb_release(priv_: *mut DrmFile) {
    let mut arg = DrmModeRmfbWork {
        fbs: ListHead {
            next: core::ptr::null_mut(),
            prev: core::ptr::null_mut(),
        },
    };
    let arg_head = core::ptr::addr_of_mut!(arg.fbs);
    // SAFETY: Initialize work list for releasing all framebuffers associated with `priv_`.
    unsafe {
        (*arg_head).next = arg_head;
        (*arg_head).prev = arg_head;
    }

    // SAFETY: `priv_` is valid; get pointer to `fbs` list head.
    let head = unsafe { core::ptr::addr_of_mut!((*priv_).fbs) };
    // SAFETY: Acquire `fbs_lock` and read first link.
    let mut link = unsafe { (*head).next };
    while link != head {
        // SAFETY: Read next link before unlinking current node.
        let next = unsafe { (*link).next };
        // SAFETY: Compute `DrmFramebuffer` pointer from `filp_head` link.
        let fb = unsafe {
            link.cast::<u8>().sub(offset_of!(DrmFramebuffer, filp_head)) as *mut DrmFramebuffer
        };
        // SAFETY: Check if framebuffer is referenced by other users or active pipelines.
        if unsafe { drm_framebuffer_read_refcount(fb) } > 1 {
            // SAFETY: Move framebuffer to deferred work list.
            unsafe { list_move_tail_raw(core::ptr::addr_of_mut!((*fb).filp_head), arg_head) };
        } else {
            // SAFETY: Unlink framebuffer from file list.
            unsafe { list_del_init(core::ptr::addr_of_mut!((*fb).filp_head)) };
            // SAFETY: Drop file's reference to framebuffer.
            unsafe { drm_framebuffer_put(fb) };
        }
        link = next;
    }

    // SAFETY: Check if work list contains framebuffers requiring pipeline removal.
    if !unsafe { list_empty(&*arg_head) } {
        // schedule_work() + flush_work(): see the module note.
        // SAFETY: Process removal of framebuffers on work list.
        unsafe { drm_mode_rmfb_work_fn(&mut arg as *mut DrmModeRmfbWork) };
    }
}

// upstream: include/linux/list.h list_move_tail()
#[inline]
/// # Safety
///
/// `entry` and `head` must point to valid, initialized intrusive `ListHead` nodes.
unsafe fn list_move_tail_raw(entry: *mut ListHead, head: *mut ListHead) {
    // SAFETY: Move list entry to tail of head list.
    unsafe { drm_kapi::list::list_move_tail(entry, head) };
}

// upstream: drm_framebuffer.c drm_framebuffer_free()
#[unsafe(no_mangle)]
/// # Safety
///
/// `kref` must point to the `base.refcount` member embedded inside an allocated `DrmFramebuffer`.
/// Caller must ensure no other threads hold references or are accessing the object concurrently.
pub unsafe extern "C" fn drm_framebuffer_free(kref: *mut Kref) {
    // SAFETY: Compute `DrmFramebuffer` pointer from `kref` offset.
    let fb = unsafe {
        (kref as *mut u8)
            .sub(offset_of!(DrmFramebuffer, base) + offset_of!(DrmModeObject, refcount))
            as *mut DrmFramebuffer
    };
    // SAFETY: `fb` is valid; read its device pointer.
    let dev = unsafe { (*fb).dev };
    // SAFETY: Verify framebuffer is unlinked from any file list before destruction.
    warn_on(!unsafe { list_empty(&(*fb).filp_head) });
    // SAFETY: Unregister framebuffer mode object.
    unsafe { drm_mode_object_unregister(dev, core::ptr::addr_of_mut!((*fb).base)) };
    // SAFETY: Read driver's framebuffer `destroy` callback.
    let destroy = unsafe { (*(*fb).funcs).destroy }.expect("destroy");
    // SAFETY: Invoke driver's `destroy` callback to free framebuffer and private data.
    unsafe { destroy(fb) };
}

// upstream: drm_framebuffer.c drm_framebuffer_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an initialized `DrmDevice` with initialized `mode_config`.
/// - `fb` must point to an allocated `DrmFramebuffer` with metadata fields (`width`, `height`, `format`, `pitches`, `offsets`) already populated.
/// - `funcs` must point to a valid static `DrmFramebufferFuncs` table with a valid `destroy` callback.
pub unsafe extern "C" fn drm_framebuffer_init(
    dev: *mut DrmDevice,
    fb: *mut DrmFramebuffer,
    funcs: *const DrmFramebufferFuncs,
) -> i32 {
    // SAFETY: Verify framebuffer device matches and format pointer is non-null.
    if warn_on(unsafe { (*fb).dev } != dev || unsafe { (*fb).format }.is_null()) {
        return -EINVAL;
    }

    // SAFETY: `fb` is valid; read format pointer.
    let format = unsafe { (*fb).format };
    // SAFETY: `format` is valid; read number of planes.
    let num_planes = unsafe { (*format).num_planes } as u32;
    for i in 0..num_planes {
        let flag = drm_framebuffer_has_handle_ref(i);
        // SAFETY: Check internal flags for existing handle reference.
        if warn_on(unsafe { (*fb).internal_flags } & flag != 0) {
            // SAFETY: Clear handle reference flag.
            unsafe { (*fb).internal_flags &= !flag };
        }
        // SAFETY: `fb` is valid; read backing GEM object pointer.
        let obj = unsafe { (*fb).obj[i as usize] };
        if !obj.is_null() {
            let exists =
                // SAFETY: Acquire reference on backing GEM object if it exists.
                unsafe { drm_gem_object_handle_get_if_exists_unlocked(obj as *mut c_void) };
            if exists {
                // SAFETY: Set internal flag indicating handle reference is held.
                unsafe { (*fb).internal_flags |= flag };
            }
        }
    }

    // SAFETY: Initialize `filp_head` list node and zero atomic reference count.
    unsafe {
        list_init(core::ptr::addr_of_mut!((*fb).filp_head));
        (*fb).funcs = funcs as *mut DrmFramebufferFuncs;
    }
    // SAFETY: Copy current process comm into framebuffer debug buffer.
    unsafe { copy_comm(fb) };

    // SAFETY: Register framebuffer mode object with DRM core.
    let ret = unsafe {
        __drm_mode_object_add(
            dev,
            core::ptr::addr_of_mut!((*fb).base),
            DRM_MODE_OBJECT_FB,
            false,
            Some(drm_framebuffer_free),
        )
    };
    if ret != 0 {
        // SAFETY: Unwind acquired GEM object references on init error.
        return unsafe { drm_framebuffer_init_err(fb, num_planes, ret) };
    }

    // SAFETY: `dev` is valid; get pointer to `mode_config.fb_lock`.
    let fb_lock = unsafe { core::ptr::addr_of_mut!((*dev).mode_config.fb_lock) };
    // SAFETY: Acquire `fb_lock`.
    unsafe { mutex_lock(fb_lock) };
    // SAFETY: Insert framebuffer into `dev->mode_config.fb_list` and increment count; release lock.
    unsafe {
        (*dev).mode_config.num_fb += 1;
        list_add(
            core::ptr::addr_of_mut!((*fb).head),
            core::ptr::addr_of_mut!((*dev).mode_config.fb_list),
        );
        mutex_unlock(fb_lock);
        drm_mode_object_register(dev, core::ptr::addr_of_mut!((*fb).base));
    }
    0
}

// `err:` label of drm_framebuffer_init().
/// # Safety
///
/// `fb` must point to a `DrmFramebuffer` being initialized, and `num_planes` must match its plane count.
unsafe fn drm_framebuffer_init_err(fb: *mut DrmFramebuffer, num_planes: u32, ret: i32) -> i32 {
    for i in 0..num_planes {
        let flag = drm_framebuffer_has_handle_ref(i);
        // SAFETY: Check if handle reference was acquired for plane `i`.
        if unsafe { (*fb).internal_flags } & flag != 0 {
            // SAFETY: Release acquired GEM object reference.
            unsafe {
                drm_gem_object_handle_put_unlocked((*fb).obj[i as usize] as *mut c_void);
                (*fb).internal_flags &= !flag;
            }
        }
    }
    ret
}

// INIT_LIST_HEAD()
#[inline]
/// # Safety
///
/// `head` must point to writable memory for an intrusive `ListHead`.
unsafe fn list_init(head: *mut ListHead) {
    // SAFETY: Initialize intrusive list head.
    unsafe {
        (*head).next = head;
        (*head).prev = head;
    }
}

// strscpy(fb->comm, current->comm): TASK_COMM_LEN is 16 with the NUL.
/// # Safety
///
/// `fb` must point to a live `DrmFramebuffer` whose `comm` field is valid.
unsafe fn copy_comm(fb: *mut DrmFramebuffer) {
    // SAFETY: `fb` is valid; get pointer to `comm` array.
    let dst = unsafe { core::ptr::addr_of_mut!((*fb).comm) as *mut c_char };
    // SAFETY: the LinuxKPI provider copies exactly TASK_COMM_LEN bytes into
    // this live framebuffer's command-name field.
    unsafe { drm_kapi::kernel_services::copy_current_comm(dst) };
}

// upstream: drm_framebuffer.c drm_framebuffer_lookup()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid, live `DrmDevice`.
/// - `file_priv` may be null, or must point to an open `DrmFile` for access control checks.
///
/// On success, caller owns the returned reference and must eventually release it via `drm_framebuffer_put`.
pub unsafe extern "C" fn drm_framebuffer_lookup(
    dev: *mut DrmDevice,
    file_priv: *mut DrmFile,
    id: u32,
) -> *mut DrmFramebuffer {
    // SAFETY: Look up mode object of type FB by ID.
    let obj = unsafe { __drm_mode_object_find(dev, file_priv, id, DRM_MODE_OBJECT_FB) };
    if !obj.is_null() {
        // SAFETY: Convert mode object pointer to enclosing `DrmFramebuffer`.
        unsafe { obj_to_fb(obj) }
    } else {
        core::ptr::null_mut()
    }
}

// upstream: drm_framebuffer.c drm_framebuffer_unregister_private()
#[unsafe(no_mangle)]
/// # Safety
///
/// `fb` must point to a valid, live `DrmFramebuffer` being unregistered. Caller must ensure
/// thread safety with respect to any concurrent file release or lookup operations.
pub unsafe extern "C" fn drm_framebuffer_unregister_private(fb: *mut DrmFramebuffer) {
    if fb.is_null() {
        return;
    }
    // SAFETY: `fb` is valid; read its device pointer.
    let dev = unsafe { (*fb).dev };
    // SAFETY: Unregister framebuffer mode object from IDR.
    unsafe { drm_mode_object_unregister(dev, core::ptr::addr_of_mut!((*fb).base)) };
}

// upstream: drm_framebuffer.c drm_framebuffer_cleanup()
#[unsafe(no_mangle)]
/// # Safety
///
/// `fb` must point to a valid, initialized `DrmFramebuffer` whose `dev` pointer is live.
/// Caller must ensure `fb` has been removed from all active CRTC/plane states and no lookups can find it.
pub unsafe extern "C" fn drm_framebuffer_cleanup(fb: *mut DrmFramebuffer) {
    // SAFETY: `fb` is valid; read its device pointer.
    let dev = unsafe { (*fb).dev };
    // SAFETY: `fb` is valid; read number of planes.
    let num_planes = unsafe { (*(*fb).format).num_planes } as u32;
    for i in 0..num_planes {
        // SAFETY: Check if handle reference is held for plane `i`.
        if unsafe { (*fb).internal_flags } & drm_framebuffer_has_handle_ref(i) != 0 {
            // SAFETY: Release handle reference on backing GEM object.
            unsafe { drm_gem_object_handle_put_unlocked((*fb).obj[i as usize] as *mut c_void) };
        }
    }

    // SAFETY: `dev` is valid; get pointer to `fb_lock`.
    let fb_lock = unsafe { core::ptr::addr_of_mut!((*dev).mode_config.fb_lock) };
    // SAFETY: Acquire `fb_lock`.
    unsafe { mutex_lock(fb_lock) };
    // SAFETY: Remove framebuffer from `fb_list` and decrement count; release lock.
    unsafe {
        list_del(core::ptr::addr_of_mut!((*fb).head));
        (*dev).mode_config.num_fb -= 1;
        mutex_unlock(fb_lock);
    }
}

// upstream: drm_framebuffer.c atomic_remove_fb()
/// # Safety
///
/// `fb` must point to a live `DrmFramebuffer` being removed.
unsafe fn atomic_remove_fb(fb: *mut DrmFramebuffer) -> i32 {
    let mut ctx = core::mem::MaybeUninit::<DrmModesetAcquireCtx>::zeroed();
    let ctx = ctx.as_mut_ptr();
    // SAFETY: `fb` is valid; read its device pointer.
    let dev = unsafe { (*fb).dev };
    let mut ret: i32;
    let mut disable_crtcs = false;

    loop {
        // retry_disable:
        // SAFETY: Initialize modeset acquire context on stack.
        unsafe { drm_modeset_acquire_init(ctx, 0) };
        // SAFETY: Allocate atomic commit state for device.
        let state = unsafe { drm_atomic_commit_alloc(dev) };
        if state.is_null() {
            ret = -ENOMEM;
            // SAFETY: Drop acquired modeset locks on error.
            unsafe { drm_modeset_drop_locks(ctx) };
            // SAFETY: Finalize modeset acquire context on error.
            unsafe { drm_modeset_acquire_fini(ctx) };
            return ret;
        }
        // SAFETY: Associate acquire context with atomic commit state.
        unsafe { (*state).acquire_ctx = ctx };

        'retry: loop {
            let mut plane_mask: u32 = 0;
            // SAFETY: Acquire all modeset locks with deadlock backoff handling.
            ret = unsafe { drm_modeset_lock_all_ctx(dev, ctx) };
            if ret != 0 {
                break 'retry;
            }

            // drm_for_each_plane()
            // SAFETY: `dev` is valid; get pointer to `plane_list`.
            let head = unsafe { core::ptr::addr_of_mut!((*dev).mode_config.plane_list) };
            // SAFETY: Read first link in `plane_list`.
            let mut link = unsafe { (*head).next };
            let mut bail = false;
            while link != head {
                let plane =
                    // SAFETY: Compute `DrmPlane` pointer from `head` list node.
                    unsafe { link.cast::<u8>().sub(offset_of!(DrmPlane, head)) as *mut DrmPlane };
                // SAFETY: Read next link in `plane_list`.
                link = unsafe { (*link).next };
                // SAFETY: `plane->state` is valid; check if plane is displaying `fb`.
                if unsafe { (*(*plane).state).fb } != fb {
                    continue;
                }

                dbg_kms(
                    dev,
                    &format!(
                        "Disabling [PLANE:{}] because [FB:{}] is removed",
                        // SAFETY: Read plane mode object ID for debug log.
                        unsafe { (*plane).base.id },
                        // SAFETY: Read framebuffer mode object ID for debug log.
                        unsafe { (*fb).base.id }
                    ),
                );
                // SAFETY: Get or create atomic plane state for `plane`.
                let plane_state = unsafe { drm_atomic_get_plane_state(state, plane) };
                if IS_ERR(plane_state) {
                    ret = PTR_ERR(plane_state);
                    bail = true;
                    break;
                }

                // SAFETY: `plane_state` is valid; read its assigned CRTC.
                let pcrtc = unsafe { (*plane_state).crtc };
                // SAFETY: Check if plane is the primary plane for `pcrtc`.
                if disable_crtcs && unsafe { (*pcrtc).primary } == plane {
                    dbg_kms(
                        dev,
                        &format!(
                            "Disabling [CRTC:{}] because [FB:{}] is removed",
                            // SAFETY: Read CRTC mode object ID for debug log.
                            unsafe { (*pcrtc).base.id },
                            // SAFETY: Read framebuffer mode object ID for debug log.
                            unsafe { (*fb).base.id }
                        ),
                    );
                    // SAFETY: Get new CRTC state for `pcrtc`.
                    let crtc_state = unsafe { drm_atomic_get_new_crtc_state_inline(state, pcrtc) };
                    // SAFETY: Add connectors affected by disabling CRTC to atomic state.
                    ret = unsafe { drm_atomic_add_affected_connectors(state, pcrtc) };
                    if ret != 0 {
                        bail = true;
                        break;
                    }
                    // SAFETY: Deactivate CRTC in new state.
                    unsafe { (*crtc_state).active = false };
                    // SAFETY: Clear mode for CRTC in new state.
                    ret = unsafe { drm_atomic_set_mode_for_crtc(crtc_state, core::ptr::null()) };
                    if ret != 0 {
                        bail = true;
                        break;
                    }
                }

                // SAFETY: Clear framebuffer pointer for plane in atomic state.
                unsafe { drm_atomic_set_fb_for_plane(plane_state, core::ptr::null_mut()) };
                // SAFETY: Clear CRTC pointer for plane in atomic state.
                ret = unsafe { drm_atomic_set_crtc_for_plane(plane_state, core::ptr::null_mut()) };
                if ret != 0 {
                    bail = true;
                    break;
                }
                // SAFETY: Update plane bitmask with modified plane index.
                plane_mask |= 1u32 << unsafe { (*plane).index };
            }
            if bail {
                // goto unlock
            } else {
                // for_each_new_connector_in_state(): connectors are walked by the
                // atomic state's connector array.
                // SAFETY: `state` is valid; read number of affected connectors.
                let nconn = unsafe { (*state).num_connector } as usize;
                for i in 0..nconn {
                    // SAFETY: Read connector state entry at index `i`.
                    let entry = unsafe { &*(*state).connectors.add(i) };
                    if entry.ptr.is_null() {
                        continue;
                    }
                    let conn_state = entry.new_state;
                    // SAFETY: Reset CRTC pointer on connector state.
                    ret = unsafe {
                        drm_atomic_set_crtc_for_connector(conn_state, core::ptr::null_mut())
                    };
                    if ret != 0 {
                        break;
                    }
                }
                if ret == 0 && plane_mask != 0 {
                    // SAFETY: Commit atomic state changes to hardware.
                    ret = unsafe { drm_atomic_commit(state) };
                }
            }

            // unlock:
            if ret == -EDEADLK {
                // SAFETY: Clear atomic commit state on deadlock backoff.
                unsafe { drm_atomic_commit_clear(state) };
                // SAFETY: Back off modeset locks after deadlock detected.
                unsafe { drm_modeset_backoff(ctx) };
                continue 'retry;
            }
            break 'retry;
        }

        // SAFETY: Free allocated atomic commit state.
        unsafe { drm_atomic_commit_put(state) };
        // out:
        // SAFETY: Drop acquired modeset locks.
        unsafe { drm_modeset_drop_locks(ctx) };
        // SAFETY: Finalize modeset acquire context.
        unsafe { drm_modeset_acquire_fini(ctx) };
        if ret == -EINVAL && !disable_crtcs {
            disable_crtcs = true;
            continue;
        }
        return ret;
    }
}

// upstream: drm_crtc_internal / drm_atomic.h drm_atomic_get_new_crtc_state()
#[inline]
/// # Safety
///
/// `state` and `crtc` must point to valid atomic state and CRTC objects.
unsafe fn drm_atomic_get_new_crtc_state_inline(
    state: *mut DrmAtomicCommit,
    crtc: *mut DrmCrtc,
) -> *mut DrmCrtcState {
    // SAFETY: Read new CRTC state pointer from atomic state array.
    unsafe { (*(*state).crtcs.add((*crtc).index as usize)).new_state }
}

// upstream: drm_framebuffer.c legacy_remove_fb()
/// # Safety
///
/// `fb` must point to a live `DrmFramebuffer` being removed.
unsafe fn legacy_remove_fb(fb: *mut DrmFramebuffer) {
    // SAFETY: `fb` is valid; read its device pointer.
    let dev = unsafe { (*fb).dev };

    // SAFETY: Acquire legacy modeset lock across all CRTCs and planes.
    unsafe { drm_modeset_lock_all(dev) };

    // SAFETY: `dev` is valid; get pointer to `crtc_list`.
    let head = unsafe { core::ptr::addr_of_mut!((*dev).mode_config.crtc_list) };
    // SAFETY: Read first link in `crtc_list`.
    let mut link = unsafe { (*head).next };
    while link != head {
        // SAFETY: Compute `DrmCrtc` pointer from list node.
        let crtc = unsafe { link.cast::<u8>().sub(offset_of!(DrmCrtc, head)) as *mut DrmCrtc };
        // SAFETY: Read next link in `crtc_list`.
        link = unsafe { (*link).next };
        // SAFETY: `crtc->primary` is valid; check if primary plane displays `fb`.
        if unsafe { (*(*crtc).primary).fb } == fb {
            dbg_kms(
                dev,
                &format!(
                    "Disabling [CRTC:{}] because [FB:{}] is removed",
                    // SAFETY: Read CRTC mode object ID for log.
                    unsafe { (*crtc).base.id },
                    // SAFETY: Read framebuffer ID for log.
                    unsafe { (*fb).base.id }
                ),
            );
            // SAFETY: Force disable CRTC displaying the framebuffer.
            if unsafe { drm_crtc_force_disable(crtc) } != 0 {
                drm_log_at(
                    DrmLogLevel::Error,
                    "drm",
                    file!(),
                    line!(),
                    &format!(
                        "*ERROR* failed to reset crtc {:p} when fb was deleted",
                        crtc
                    ),
                );
            }
        }
    }

    // SAFETY: `dev` is valid; get pointer to `plane_list`.
    let head = unsafe { core::ptr::addr_of_mut!((*dev).mode_config.plane_list) };
    // SAFETY: Read first link in `plane_list`.
    let mut link = unsafe { (*head).next };
    while link != head {
        // SAFETY: Compute `DrmPlane` pointer from list node.
        let plane = unsafe { link.cast::<u8>().sub(offset_of!(DrmPlane, head)) as *mut DrmPlane };
        // SAFETY: Read next link in `plane_list`.
        link = unsafe { (*link).next };
        // SAFETY: Check if plane displays `fb`.
        if unsafe { (*plane).fb } == fb {
            dbg_kms(
                dev,
                &format!(
                    "Disabling [PLANE:{}] because [FB:{}] is removed",
                    // SAFETY: Read plane mode object ID for log.
                    unsafe { (*plane).base.id },
                    // SAFETY: Read framebuffer ID for log.
                    unsafe { (*fb).base.id }
                ),
            );
            // SAFETY: Force disable plane displaying the framebuffer.
            unsafe { drm_plane_force_disable(plane) };
        }
    }

    // SAFETY: Unlock legacy modeset lock.
    unsafe { drm_modeset_unlock_all(dev) };
}

// upstream: drm_framebuffer.c drm_framebuffer_remove()
#[unsafe(no_mangle)]
/// # Safety
///
/// `fb` must be null or point to a valid, initialized `DrmFramebuffer`. If non-null, caller must
/// be in process context able to acquire mode_config and modeset locks to detach the framebuffer.
pub unsafe extern "C" fn drm_framebuffer_remove(fb: *mut DrmFramebuffer) {
    if fb.is_null() {
        return;
    }
    // SAFETY: `fb` is valid; read its device pointer.
    let dev = unsafe { (*fb).dev };

    // SAFETY: Check if framebuffer was unlinked from file list.
    warn_on(!unsafe { list_empty(&(*fb).filp_head) });

    // SAFETY: Check if framebuffer refcount is greater than 1.
    if unsafe { drm_framebuffer_read_refcount(fb) } > 1 {
        if drm_drv_uses_atomic_modeset(dev) {
            // SAFETY: Remove framebuffer via atomic modeset pipeline.
            let ret = unsafe { atomic_remove_fb(fb) };
            warn_on(ret != 0);
        } else {
            // SAFETY: Remove framebuffer via legacy modeset pipeline.
            unsafe { legacy_remove_fb(fb) };
        }
    }

    // SAFETY: Drop caller's reference to framebuffer.
    unsafe { drm_framebuffer_put(fb) };
}

// upstream: drm_framebuffer.c drm_framebuffer_print_info()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `p` must point to a valid, initialized `DrmPrinter`.
/// - `fb` must point to a valid, readable `DrmFramebuffer` whose format and backing objects can be inspected.
pub unsafe extern "C" fn drm_framebuffer_print_info(
    p: *mut DrmPrinter,
    indent: u32,
    fb: *const DrmFramebuffer,
) {
    // SAFETY: `fb` is valid; dereference to immutable reference.
    let fb_ref = unsafe { &*fb };
    let format = fb_ref.format;

    printf_indent(
        p,
        indent,
        &format!("allocated by = {}\n", cstr_of(fb_ref.comm.as_ptr())),
    );
    printf_indent(
        p,
        indent,
        // SAFETY: Read atomic reference count of framebuffer for debug print.
        &format!("refcount={}\n", unsafe {
            drm_framebuffer_read_refcount(fb)
        }),
    );
    printf_indent(
        p,
        indent,
        // SAFETY: Read pixel format fourcc code from format info.
        &format!("format={:#010x}\n", unsafe { (*format).format }),
    );
    printf_indent(p, indent, &format!("modifier=0x{:x}\n", fb_ref.modifier));
    printf_indent(
        p,
        indent,
        &format!("size={}x{}\n", fb_ref.width, fb_ref.height),
    );
    printf_indent(p, indent, "layers:\n");

    // SAFETY: Read number of planes from format info.
    for i in 0..unsafe { (*format).num_planes } as usize {
        printf_indent(
            p,
            indent + 1,
            &format!(
                "size[{}]={}x{}\n",
                i,
                drm_format_info_plane_width(format, fb_ref.width as i32, i as i32),
                drm_format_info_plane_height(format, fb_ref.height as i32, i as i32)
            ),
        );
        printf_indent(
            p,
            indent + 1,
            &format!("pitch[{}]={}\n", i, fb_ref.pitches[i]),
        );
        printf_indent(
            p,
            indent + 1,
            &format!("offset[{}]={}\n", i, fb_ref.offsets[i]),
        );
        let obj = fb_ref.obj[i];
        printf_indent(
            p,
            indent + 1,
            &format!("obj[{}]:{}\n", i, if obj.is_null() { "(null)" } else { "" }),
        );
        if !obj.is_null() {
            // SAFETY: Print GEM object information for backing plane buffer.
            unsafe { drm_gem_print_info(p, indent + 2, obj as *const c_void) };
        }
    }
}

// drm_printf_indent(): one tab per indent level, then the formatted text.
fn printf_indent(p: *mut DrmPrinter, indent: u32, text: &str) {
    let mut line = alloc::string::String::new();
    for _ in 0..indent {
        line.push('\t');
    }
    line.push_str(text);
    // SAFETY: Format and print formatted text through `DrmPrinter`.
    unsafe { crate::drm_print_upstream::drm_printf(p, &line) };
}

// NUL-terminated C char array to &str (for `comm`).
fn cstr_of<'a>(p: *const c_char) -> &'a str {
    // SAFETY: `p` points to a valid null-terminated C string; convert to `&str`.
    unsafe { core::ffi::CStr::from_ptr(p) }
        .to_str()
        .unwrap_or("")
}
