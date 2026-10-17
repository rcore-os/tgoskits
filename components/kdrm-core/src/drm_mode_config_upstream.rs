// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_mode_config.c`.
//!
//! Mode-config initialisation (`drmm_mode_config_init()`) and teardown
//! (`drm_mode_config_cleanup()`), the standard property set, the
//! encoder/CRTC possible-mask validators, `drm_mode_config_reset()`, the
//! `drm_modeset_register_all()` pair, and the `GETRESOURCES` ioctl.
//!
//! Deviations:
//! - The `CONFIG_LOCKDEP` self-test block of `drmm_mode_config_init()` is
//!   omitted (lockdep is off on this target).
//! - The colorop list (`drm_colorop.c`, optional and not yet ported) is never
//!   populated, so its iteration in `drm_mode_config_reset()` and
//!   `drm_mode_config_cleanup()` is omitted.
//! - The leaked-framebuffer dump prints only the id; the full
//!   `drm_framebuffer_print_info()` dump depends on the print helpers.

#![allow(non_snake_case)]

use core::{
    ffi::{c_char, c_void},
    mem::{offset_of, size_of},
};

use drm_kapi::{
    assert::warn_on,
    config::{EFAULT, ENOMEM, EOPNOTSUPP},
    list::{INIT_LIST_HEAD, list_empty},
    mutex::mutex_init,
    types::{Kref, ListHead},
    workqueue::{INIT_WORK_C, c_flush_work},
};

use crate::{
    drm_connector_types::DrmConnector,
    drm_connector_upstream::{
        drm_connector_create_standard_properties, drm_connector_free_work_fn,
        drm_connector_list_iter_begin, drm_connector_list_iter_end, drm_connector_list_iter_next,
        drm_connector_put, drm_connector_register_all, drm_connector_unregister_all,
    },
    drm_crtc_types::DrmCrtc,
    drm_device_types::DrmDevice,
    drm_encoder_types::DrmEncoder,
    drm_encoder_upstream::{drm_encoder_register_all, drm_encoder_unregister_all},
    drm_file_types::DrmFile,
    drm_framebuffer_types::DrmFramebuffer,
    drm_mode_object_upstream::{
        DRIVER_MODESET, drm_core_check_feature, idr_destroy, idr_init_base, mutex_lock,
        mutex_unlock,
    },
    drm_modeset_lock_types::DrmModesetLock,
    drm_plane_types::{DRM_PLANE_TYPE_PRIMARY, DrmPlane},
    drm_property_types::{DrmPropEnumList, DrmProperty},
    drm_property_upstream::{
        drm_property_blob_put, drm_property_create, drm_property_create_bool,
        drm_property_create_enum, drm_property_create_object, drm_property_create_range,
        drm_property_create_signed_range, drm_property_destroy,
    },
    uapi_drm_consts::{
        DRM_MODE_CONNECTOR_WRITEBACK, DRM_MODE_OBJECT_CRTC, DRM_MODE_OBJECT_FB,
        DRM_MODE_PROP_ATOMIC, DRM_MODE_PROP_BLOB, DRM_MODE_PROP_IMMUTABLE,
    },
    uapi_drm_mode_types::DrmModeCardRes,
};

// Cross-batch callees: plane/CRTC registration (drm_plane.c, drm_crtc.c),
// modeset lock (drm_modeset_lock.c), leases (drm_lease.c), framebuffer
// release (drm_framebuffer.c), and the managed-release hook (drm_managed.c).
unsafe extern "C" {
    fn drm_plane_register_all(dev: *mut DrmDevice) -> i32;
    fn drm_plane_unregister_all(dev: *mut DrmDevice);
    fn drm_crtc_register_all(dev: *mut DrmDevice) -> i32;
    fn drm_crtc_unregister_all(dev: *mut DrmDevice);
    fn drm_modeset_lock_init(lock: *mut DrmModesetLock);
    fn drm_lease_held(file_priv: *mut DrmFile, id: i32) -> bool;
    fn drm_framebuffer_free(kref: *mut Kref);
    fn __drmm_add_action_or_reset(
        dev: *mut DrmDevice,
        action: unsafe extern "C" fn(*mut DrmDevice, *mut c_void),
        data: *mut c_void,
        name: *const c_char,
    ) -> i32;
}

/// `WARN(cond, fmt, ...)`: logs the message when `cond` holds and yields `cond`.
macro_rules! warn_if {
    ($cond:expr, $device:expr, $format:expr $(, $argument:expr)* $(,)?) => {{
        let __cond: bool = $cond;
        if __cond {
            drm_kapi::drm_warn!($device, $format $(, $argument)*);
        }
        __cond
    }};
}

/// `drm_crtc_mask()`: one bit per CRTC index (drm_crtc.h).
// upstream: include/drm/drm_crtc.h drm_crtc_mask()
/// # Safety
///
/// `crtc` must point to an active, valid `DrmCrtc`.
unsafe fn drm_crtc_mask(crtc: *const DrmCrtc) -> u32 {
    // SAFETY: `crtc` is non-null and points to an active, valid `DrmCrtc` per the function's safety contract.
    unsafe { 1u32 << (*crtc).index }
}

/// `drm_plane_mask()`: one bit per plane index (drm_plane.h).
// upstream: include/drm/drm_plane.h drm_plane_mask()
/// # Safety
///
/// `plane` must point to an active, valid `DrmPlane`.
unsafe fn drm_plane_mask(plane: *const DrmPlane) -> u32 {
    // SAFETY: `plane` is non-null and points to an active, valid `DrmPlane` per the function's safety contract.
    unsafe { 1u32 << (*plane).index }
}

/// `drm_encoder_mask()`: one bit per encoder index (drm_encoder.h).
// upstream: include/drm/drm_encoder.h drm_encoder_mask()
/// # Safety
///
/// `encoder` must point to an active, valid `DrmEncoder`.
unsafe fn drm_encoder_mask(encoder: *const DrmEncoder) -> u32 {
    // SAFETY: `encoder` is non-null and points to an active, valid `DrmEncoder` per the function's safety contract.
    unsafe { 1u32 << (*encoder).index }
}

/// `list_entry(link, struct drm_encoder, head)`.
/// # Safety
///
/// `link` must point to the `head` field embedded within an active `DrmEncoder`.
unsafe fn encoder_from_head(link: *mut ListHead) -> *mut DrmEncoder {
    // SAFETY: per the function's safety contract, `link` is the `head` member of a live `DrmEncoder`, so stepping back by its offset stays within that object.
    unsafe {
        link.cast::<u8>()
            .sub(offset_of!(DrmEncoder, head))
            .cast::<DrmEncoder>()
    }
}

/// `list_entry(link, struct drm_crtc, head)`.
/// # Safety
///
/// `link` must point to the `head` field embedded within an active `DrmCrtc`.
unsafe fn crtc_from_head(link: *mut ListHead) -> *mut DrmCrtc {
    // SAFETY: per the function's safety contract, `link` is the `head` member of a live `DrmCrtc`, so stepping back by its offset stays within that object.
    unsafe {
        link.cast::<u8>()
            .sub(offset_of!(DrmCrtc, head))
            .cast::<DrmCrtc>()
    }
}

/// `list_entry(link, struct drm_plane, head)`.
/// # Safety
///
/// `link` must point to the `head` field embedded within an active `DrmPlane`.
unsafe fn plane_from_head(link: *mut ListHead) -> *mut DrmPlane {
    // SAFETY: per the function's safety contract, `link` is the `head` member of a live `DrmPlane`, so stepping back by its offset stays within that object.
    unsafe {
        link.cast::<u8>()
            .sub(offset_of!(DrmPlane, head))
            .cast::<DrmPlane>()
    }
}

/// `list_entry(link, struct drm_framebuffer, head)`.
/// # Safety
///
/// `link` must point to the `head` field embedded within an active `DrmFramebuffer`.
unsafe fn fb_from_head(link: *mut ListHead) -> *mut DrmFramebuffer {
    // SAFETY: per the function's safety contract, `link` is the `head` member of a live `DrmFramebuffer`, so stepping back by its offset stays within that object.
    unsafe {
        link.cast::<u8>()
            .sub(offset_of!(DrmFramebuffer, head))
            .cast::<DrmFramebuffer>()
    }
}

/// `put_user()` for a single scalar: returns 0 or -EFAULT.
// upstream: include/linux/uaccess.h put_user()
/// # Safety
///
/// `dst` must point to a valid userspace memory location of size `size_of::<T>()`.
unsafe fn put_user<T: Copy>(dst: *mut T, value: T) -> i32 {
    // SAFETY: per the function's safety contract, `dst` is a valid userspace location for `size_of::<T>()` bytes; the source is a local `value` of that size.
    let rest = unsafe {
        drm_kapi::mm_native::copy_to_user_c(
            dst.cast::<c_void>(),
            core::ptr::addr_of!(value).cast::<c_void>(),
            size_of::<T>(),
        )
    };
    if rest == 0 { 0 } else { -EFAULT }
}

// upstream: drm_mode_config.c drm_modeset_register_all()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to an active, valid `DrmDevice` whose KMS plane, CRTC, encoder,
/// and connector lists are initialized and ready to be registered with DRM core.
pub unsafe extern "C" fn drm_modeset_register_all(dev: *mut DrmDevice) -> i32 {
    // SAFETY: `dev` is a valid `DrmDevice` pointer guaranteed by the caller.
    // Registering planes, CRTCs, encoders, and connectors sequentially obeys DRM modeset registration order.
    unsafe {
        let ret = drm_plane_register_all(dev);
        if ret != 0 {
            return ret; // err_plane
        }

        let ret = drm_crtc_register_all(dev);
        if ret != 0 {
            drm_plane_unregister_all(dev); // err_crtc
            return ret;
        }

        let ret = drm_encoder_register_all(dev);
        if ret != 0 {
            drm_crtc_unregister_all(dev); // err_encoder
            drm_plane_unregister_all(dev);
            return ret;
        }

        let ret = drm_connector_register_all(dev);
        if ret != 0 {
            drm_encoder_unregister_all(dev); // err_connector
            drm_crtc_unregister_all(dev);
            drm_plane_unregister_all(dev);
            return ret;
        }

        0
    }
}

// upstream: drm_mode_config.c drm_modeset_unregister_all()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to an active `DrmDevice` whose KMS components were registered.
pub unsafe extern "C" fn drm_modeset_unregister_all(dev: *mut DrmDevice) {
    // SAFETY: `dev` is a valid `DrmDevice` pointer; unregistering connectors, encoders, CRTCs,
    // and planes in reverse order obeys DRM modeset unregistration contracts.
    unsafe {
        drm_connector_unregister_all(dev);
        drm_encoder_unregister_all(dev);
        drm_crtc_unregister_all(dev);
        drm_plane_unregister_all(dev);
    }
}

// upstream: drm_mode_config.c drm_mode_getresources()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an active `DrmDevice` with modesetting enabled.
/// - `data` must point to a valid kernel-space `DrmModeCardRes` ioctl request buffer.
/// - `file_priv` must point to an open, valid `DrmFile` structure.
pub unsafe extern "C" fn drm_mode_getresources(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev`, `data`, and `file_priv` are valid non-null pointers meeting this ioctl's preconditions.
    // List traversals over `file_priv->fbs`, `dev->mode_config.crtc_list`, and `dev->mode_config.encoder_list`
    // access initialized linked lists, while connector iteration safely uses `drm_connector_list_iter_*`.
    unsafe {
        let card_res = data.cast::<DrmModeCardRes>();
        let mut count: u32;

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        mutex_lock(core::ptr::addr_of_mut!((*file_priv).fbs_lock));
        count = 0;
        let fb_id = (*card_res).fb_id_ptr as usize as *mut u32;
        let head = core::ptr::addr_of_mut!((*file_priv).fbs);
        let mut cur = (*head).next;
        while cur != head {
            // list_for_each_entry(fb, &file_priv->fbs, filp_head)
            let fb = filp_head_to_fb(cur);
            if count < (*card_res).count_fbs
                && put_user(fb_id.add(count as usize), (*fb).base.id) != 0
            {
                mutex_unlock(core::ptr::addr_of_mut!((*file_priv).fbs_lock));
                return -EFAULT;
            }
            count += 1;
            cur = (*cur).next;
        }
        (*card_res).count_fbs = count;
        mutex_unlock(core::ptr::addr_of_mut!((*file_priv).fbs_lock));

        (*card_res).max_height = (*dev).mode_config.max_height;
        (*card_res).min_height = (*dev).mode_config.min_height;
        (*card_res).max_width = (*dev).mode_config.max_width;
        (*card_res).min_width = (*dev).mode_config.min_width;

        count = 0;
        let crtc_id = (*card_res).crtc_id_ptr as usize as *mut u32;
        let head = core::ptr::addr_of_mut!((*dev).mode_config.crtc_list);
        let mut cur = (*head).next;
        while cur != head {
            let crtc = crtc_from_head(cur);
            if drm_lease_held(file_priv, (*crtc).base.id as i32) {
                if count < (*card_res).count_crtcs
                    && put_user(crtc_id.add(count as usize), (*crtc).base.id) != 0
                {
                    return -EFAULT;
                }
                count += 1;
            }
            cur = (*cur).next;
        }
        (*card_res).count_crtcs = count;

        count = 0;
        let encoder_id = (*card_res).encoder_id_ptr as usize as *mut u32;
        let head = core::ptr::addr_of_mut!((*dev).mode_config.encoder_list);
        let mut cur = (*head).next;
        while cur != head {
            let encoder = encoder_from_head(cur);
            if count < (*card_res).count_encoders
                && put_user(encoder_id.add(count as usize), (*encoder).base.id) != 0
            {
                return -EFAULT;
            }
            count += 1;
            cur = (*cur).next;
        }
        (*card_res).count_encoders = count;

        let mut conn_iter =
            core::mem::MaybeUninit::<crate::drm_connector_types::DrmConnectorListIter>::zeroed();
        let conn_iter = conn_iter.as_mut_ptr();
        drm_connector_list_iter_begin(dev, conn_iter);
        count = 0;
        let connector_id = (*card_res).connector_id_ptr as usize as *mut u32;
        loop {
            let connector: *mut DrmConnector = drm_connector_list_iter_next(conn_iter);
            if connector.is_null() {
                break;
            }
            if !(*file_priv).writeback_connectors
                && (*connector).connector_type == DRM_MODE_CONNECTOR_WRITEBACK as i32
            {
                continue;
            }
            if drm_lease_held(file_priv, (*connector).base.id as i32) {
                if count < (*card_res).count_connectors
                    && put_user(connector_id.add(count as usize), (*connector).base.id) != 0
                {
                    drm_connector_list_iter_end(conn_iter);
                    return -EFAULT;
                }
                count += 1;
            }
        }
        (*card_res).count_connectors = count;
        drm_connector_list_iter_end(conn_iter);

        0
    }
}

/// `list_entry(link, struct drm_framebuffer, filp_head)`.
/// # Safety
///
/// `link` must point to the `filp_head` field embedded within an active `DrmFramebuffer`.
unsafe fn filp_head_to_fb(link: *mut ListHead) -> *mut DrmFramebuffer {
    // SAFETY: per the function's safety contract, `link` is the `filp_head` member of a live `DrmFramebuffer`, so stepping back by its offset stays within that object.
    unsafe {
        link.cast::<u8>()
            .sub(offset_of!(DrmFramebuffer, filp_head))
            .cast::<DrmFramebuffer>()
    }
}

// upstream: drm_mode_config.c drm_mode_config_reset()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to an active, valid `DrmDevice` whose `mode_config` planes, CRTCs,
/// encoders, and connectors have been initialized.
pub unsafe extern "C" fn drm_mode_config_reset(dev: *mut DrmDevice) {
    // SAFETY: `dev` is a valid `DrmDevice` whose mode_config list heads are valid and
    // populated with mode objects having valid `funcs` tables.
    unsafe {
        // Colorop reset is omitted: the colorop list is never populated.

        let head = core::ptr::addr_of_mut!((*dev).mode_config.plane_list);
        let mut cur = (*head).next;
        while cur != head {
            let plane = plane_from_head(cur);
            if let Some(reset) = (*(*plane).funcs).reset {
                reset(plane);
            }
            cur = (*cur).next;
        }

        let head = core::ptr::addr_of_mut!((*dev).mode_config.crtc_list);
        let mut cur = (*head).next;
        while cur != head {
            let crtc = crtc_from_head(cur);
            if let Some(reset) = (*(*crtc).funcs).reset {
                reset(crtc);
            }
            cur = (*cur).next;
        }

        let head = core::ptr::addr_of_mut!((*dev).mode_config.encoder_list);
        let mut cur = (*head).next;
        while cur != head {
            let encoder = encoder_from_head(cur);
            if !(*encoder).funcs.is_null()
                && let Some(reset) = (*(*encoder).funcs).reset
            {
                reset(encoder);
            }
            cur = (*cur).next;
        }

        let mut conn_iter =
            core::mem::MaybeUninit::<crate::drm_connector_types::DrmConnectorListIter>::zeroed();
        let conn_iter = conn_iter.as_mut_ptr();
        drm_connector_list_iter_begin(dev, conn_iter);
        loop {
            let connector = drm_connector_list_iter_next(conn_iter);
            if connector.is_null() {
                break;
            }
            if let Some(reset) = (*(*connector).funcs).reset {
                reset(connector);
            }
        }
        drm_connector_list_iter_end(conn_iter);
    }
}

/// `drm_plane_type_enum_list[]` (drm_mode_config.c). The names are NUL
/// terminated for C consumers.
const PLANE_TYPE_NAMES: [&[u8]; 3] = [b"Overlay\0", b"Primary\0", b"Cursor\0"];

// upstream: drm_mode_config.c drm_mode_create_standard_properties()
/// # Safety
///
/// `dev` must point to an active, valid `DrmDevice` whose `mode_config` is being initialized.
unsafe fn drm_mode_create_standard_properties(dev: *mut DrmDevice) -> i32 {
    // SAFETY: `dev` is valid, and the property creation functions attach initialized
    // properties to `(*dev).mode_config`.
    unsafe {
        let ret = drm_connector_create_standard_properties(dev);
        if ret != 0 {
            return ret;
        }

        let plane_type_list = [
            DrmPropEnumList {
                r#type: 0,
                name: PLANE_TYPE_NAMES[0].as_ptr().cast::<c_char>() as *mut c_char,
            },
            DrmPropEnumList {
                r#type: 1,
                name: PLANE_TYPE_NAMES[1].as_ptr().cast::<c_char>() as *mut c_char,
            },
            DrmPropEnumList {
                r#type: 2,
                name: PLANE_TYPE_NAMES[2].as_ptr().cast::<c_char>() as *mut c_char,
            },
        ];

        let mc = core::ptr::addr_of_mut!((*dev).mode_config);

        let prop = drm_property_create_enum(
            dev,
            DRM_MODE_PROP_IMMUTABLE,
            c"type".as_ptr().cast(),
            plane_type_list.as_ptr(),
            plane_type_list.len() as i32,
        );
        if prop.is_null() {
            return -ENOMEM;
        }
        (*mc).plane_type_property = prop;

        macro_rules! range_prop {
            ($field:ident, $flags:expr, $name:literal, $min:expr, $max:expr) => {{
                let prop =
                    drm_property_create_range(dev, $flags, $name.as_ptr().cast(), $min, $max);
                if prop.is_null() {
                    return -ENOMEM;
                }
                (*mc).$field = prop;
            }};
        }
        macro_rules! signed_prop {
            ($field:ident, $flags:expr, $name:literal, $min:expr, $max:expr) => {{
                let prop = drm_property_create_signed_range(
                    dev,
                    $flags,
                    $name.as_ptr().cast(),
                    $min,
                    $max,
                );
                if prop.is_null() {
                    return -ENOMEM;
                }
                (*mc).$field = prop;
            }};
        }
        macro_rules! object_prop {
            ($field:ident, $flags:expr, $name:literal, $type:expr) => {{
                let prop = drm_property_create_object(dev, $flags, $name.as_ptr().cast(), $type);
                if prop.is_null() {
                    return -ENOMEM;
                }
                (*mc).$field = prop;
            }};
        }
        macro_rules! blob_prop {
            ($field:ident, $flags:expr, $name:literal) => {{
                let prop = drm_property_create(dev, $flags, $name.as_ptr().cast(), 0);
                if prop.is_null() {
                    return -ENOMEM;
                }
                (*mc).$field = prop;
            }};
        }
        macro_rules! bool_prop {
            ($field:ident, $flags:expr, $name:literal) => {{
                let prop = drm_property_create_bool(dev, $flags, $name.as_ptr().cast());
                if prop.is_null() {
                    return -ENOMEM;
                }
                (*mc).$field = prop;
            }};
        }

        const U32_MAX: u64 = u32::MAX as u64;
        const U64_MAX: u64 = u64::MAX;

        range_prop!(prop_src_x, DRM_MODE_PROP_ATOMIC, b"SRC_X\0", 0, U32_MAX);
        range_prop!(prop_src_y, DRM_MODE_PROP_ATOMIC, b"SRC_Y\0", 0, U32_MAX);
        range_prop!(prop_src_w, DRM_MODE_PROP_ATOMIC, b"SRC_W\0", 0, U32_MAX);
        range_prop!(prop_src_h, DRM_MODE_PROP_ATOMIC, b"SRC_H\0", 0, U32_MAX);
        signed_prop!(
            prop_crtc_x,
            DRM_MODE_PROP_ATOMIC,
            b"CRTC_X\0",
            i32::MIN as i64,
            i32::MAX as i64
        );
        signed_prop!(
            prop_crtc_y,
            DRM_MODE_PROP_ATOMIC,
            b"CRTC_Y\0",
            i32::MIN as i64,
            i32::MAX as i64
        );
        range_prop!(
            prop_crtc_w,
            DRM_MODE_PROP_ATOMIC,
            b"CRTC_W\0",
            0,
            i32::MAX as u64
        );
        range_prop!(
            prop_crtc_h,
            DRM_MODE_PROP_ATOMIC,
            b"CRTC_H\0",
            0,
            i32::MAX as u64
        );
        object_prop!(
            prop_fb_id,
            DRM_MODE_PROP_ATOMIC,
            b"FB_ID\0",
            DRM_MODE_OBJECT_FB
        );
        signed_prop!(
            prop_in_fence_fd,
            DRM_MODE_PROP_ATOMIC,
            b"IN_FENCE_FD\0",
            -1,
            i32::MAX as i64
        );
        range_prop!(
            prop_out_fence_ptr,
            DRM_MODE_PROP_ATOMIC,
            b"OUT_FENCE_PTR\0",
            0,
            U64_MAX
        );
        object_prop!(
            prop_crtc_id,
            DRM_MODE_PROP_ATOMIC,
            b"CRTC_ID\0",
            DRM_MODE_OBJECT_CRTC
        );
        blob_prop!(
            prop_fb_damage_clips,
            DRM_MODE_PROP_ATOMIC | DRM_MODE_PROP_BLOB,
            b"FB_DAMAGE_CLIPS\0"
        );
        bool_prop!(prop_active, DRM_MODE_PROP_ATOMIC, b"ACTIVE\0");
        blob_prop!(
            prop_mode_id,
            DRM_MODE_PROP_ATOMIC | DRM_MODE_PROP_BLOB,
            b"MODE_ID\0"
        );
        bool_prop!(prop_vrr_enabled, 0, b"VRR_ENABLED\0");
        blob_prop!(degamma_lut_property, DRM_MODE_PROP_BLOB, b"DEGAMMA_LUT\0");
        range_prop!(
            degamma_lut_size_property,
            DRM_MODE_PROP_IMMUTABLE,
            b"DEGAMMA_LUT_SIZE\0",
            0,
            U32_MAX
        );
        blob_prop!(ctm_property, DRM_MODE_PROP_BLOB, b"CTM\0");
        blob_prop!(gamma_lut_property, DRM_MODE_PROP_BLOB, b"GAMMA_LUT\0");
        range_prop!(
            gamma_lut_size_property,
            DRM_MODE_PROP_IMMUTABLE,
            b"GAMMA_LUT_SIZE\0",
            0,
            U32_MAX
        );
        range_prop!(
            background_color_property,
            0,
            b"BACKGROUND_COLOR\0",
            0,
            U64_MAX
        );
        blob_prop!(
            modifiers_property,
            DRM_MODE_PROP_IMMUTABLE | DRM_MODE_PROP_BLOB,
            b"IN_FORMATS\0"
        );
        blob_prop!(
            async_modifiers_property,
            DRM_MODE_PROP_IMMUTABLE | DRM_MODE_PROP_BLOB,
            b"IN_FORMATS_ASYNC\0"
        );
        blob_prop!(
            size_hints_property,
            DRM_MODE_PROP_IMMUTABLE | DRM_MODE_PROP_BLOB,
            b"SIZE_HINTS\0"
        );

        0
    }
}

/// `drm_mode_config_init_release()`: the managed release action.
// upstream: drm_mode_config.c drm_mode_config_init_release()
/// # Safety
///
/// `dev` must point to an active `DrmDevice` undergoing managed resource cleanup.
unsafe extern "C" fn drm_mode_config_init_release(dev: *mut DrmDevice, _ptr: *mut c_void) {
    // SAFETY: Managed cleanup callback forwards to `drm_mode_config_cleanup` with the valid `dev`.
    unsafe {
        drm_mode_config_cleanup(dev);
    }
}

// upstream: drm_mode_config.c drmm_mode_config_init()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to an active, valid `DrmDevice` whose `mode_config` is uninitialized.
pub unsafe extern "C" fn drmm_mode_config_init(dev: *mut DrmDevice) -> i32 {
    // SAFETY: `dev` is a valid `DrmDevice`. Initializing list heads, mutexes, IDRs,
    // and work structures sets up `(*dev).mode_config` before registering managed cleanup.
    unsafe {
        let mc = core::ptr::addr_of_mut!((*dev).mode_config);

        mutex_init(core::ptr::addr_of_mut!((*mc).mutex));
        drm_modeset_lock_init(core::ptr::addr_of_mut!((*mc).connection_mutex));
        mutex_init(core::ptr::addr_of_mut!((*mc).idr_mutex));
        mutex_init(core::ptr::addr_of_mut!((*mc).fb_lock));
        mutex_init(core::ptr::addr_of_mut!((*mc).blob_lock));

        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*mc).fb_list));
        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*mc).crtc_list));
        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*mc).connector_list));
        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*mc).encoder_list));
        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*mc).property_list));
        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*mc).property_blob_list));
        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*mc).plane_list));
        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*mc).colorop_list));
        INIT_LIST_HEAD(core::ptr::addr_of_mut!((*mc).privobj_list));

        idr_init_base(core::ptr::addr_of_mut!((*mc).object_idr), 1);
        idr_init_base(core::ptr::addr_of_mut!((*mc).tile_idr), 1);
        drm_kapi::idr::ida_init(&mut (*mc).connector_ida);

        drm_kapi::locks::spin_lock_init(&mut (*mc).connector_list_lock);
        drm_kapi::list::init_llist_head(core::ptr::addr_of_mut!((*mc).connector_free_list));
        // INIT_WORK(&connector_free_work, drm_connector_free_work_fn)
        let work = core::ptr::addr_of_mut!((*mc).connector_free_work);
        if drm_kapi::workqueue::init_workqueue_executor().is_ok() {
            INIT_WORK_C(&mut *work, drm_connector_free_work_fn);
        } else {
            // INIT_WORK_C() also binds the workqueue executor, which needs a
            // context that can block; host tests run where it cannot. Set the
            // work fields only, and let the first queue bind the executor.
            (*work).data = 0;
            INIT_LIST_HEAD(core::ptr::addr_of_mut!((*work).entry));
            (*work).function = Some(drm_connector_free_work_fn);
        }

        let ret = drm_mode_create_standard_properties(dev);
        if ret != 0 {
            drm_mode_config_cleanup(dev);
            return ret;
        }

        (*mc).num_fb = 0;
        (*mc).num_connector = 0;
        (*mc).num_crtc = 0;
        (*mc).num_encoder = 0;
        (*mc).num_total_plane = 0;
        (*mc).num_colorop = 0;

        // CONFIG_LOCKDEP self-test block omitted; see the module comment.

        __drmm_add_action_or_reset(
            dev,
            drm_mode_config_init_release,
            core::ptr::null_mut(),
            c"drm_mode_config_init_release".as_ptr().cast(),
        )
    }
}

// upstream: drm_mode_config.c drm_mode_config_cleanup()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to an active `DrmDevice` whose `mode_config` was initialized with `drmm_mode_config_init`.
pub unsafe extern "C" fn drm_mode_config_cleanup(dev: *mut DrmDevice) {
    // SAFETY: `dev` is a valid `DrmDevice` with initialized `mode_config` lists.
    // Iterating and destroying encoders, connectors, properties, planes, CRTCs, blobs,
    // and framebuffers safely drains all KMS objects.
    unsafe {
        let mc = core::ptr::addr_of_mut!((*dev).mode_config);

        // list_for_each_entry_safe(encoder, enct, &encoder_list, head)
        let head = core::ptr::addr_of_mut!((*mc).encoder_list);
        let mut cur = (*head).next;
        while cur != head {
            let next = (*cur).next;
            let encoder = encoder_from_head(cur);
            if let Some(destroy) = (*(*encoder).funcs).destroy {
                destroy(encoder);
            }
            cur = next;
        }

        let mut conn_iter =
            core::mem::MaybeUninit::<crate::drm_connector_types::DrmConnectorListIter>::zeroed();
        let conn_iter = conn_iter.as_mut_ptr();
        drm_connector_list_iter_begin(dev, conn_iter);
        loop {
            let connector = drm_connector_list_iter_next(conn_iter);
            if connector.is_null() {
                break;
            }
            drm_connector_put(connector);
        }
        drm_connector_list_iter_end(conn_iter);

        // flush_work(&connector_free_work): waiting needs a blocking context. When
        // the executor cannot be bound (host tests), no work was ever queued.
        if drm_kapi::workqueue::init_workqueue_executor().is_ok() {
            c_flush_work(core::ptr::addr_of_mut!((*mc).connector_free_work));
        }

        if warn_on(!list_empty(&(*mc).connector_list)) {
            drm_connector_list_iter_begin(dev, conn_iter);
            loop {
                let connector = drm_connector_list_iter_next(conn_iter);
                if connector.is_null() {
                    break;
                }
                drm_kapi::drm_err!(dev, "connector %s leaked!\n", (*connector).name);
            }
            drm_connector_list_iter_end(conn_iter);
        }

        // list_for_each_entry_safe(property, pt, &property_list, head)
        let head = core::ptr::addr_of_mut!((*mc).property_list);
        let mut cur = (*head).next;
        while cur != head {
            let next = (*cur).next;
            let property = cur
                .cast::<u8>()
                .sub(offset_of!(DrmProperty, head))
                .cast::<DrmProperty>();
            drm_property_destroy(dev, property);
            cur = next;
        }

        // Colorop teardown is omitted: the colorop list is never populated.

        // list_for_each_entry_safe(plane, plt, &plane_list, head)
        let head = core::ptr::addr_of_mut!((*mc).plane_list);
        let mut cur = (*head).next;
        while cur != head {
            let next = (*cur).next;
            let plane = plane_from_head(cur);
            if let Some(destroy) = (*(*plane).funcs).destroy {
                destroy(plane);
            }
            cur = next;
        }

        // list_for_each_entry_safe(crtc, ct, &crtc_list, head)
        let head = core::ptr::addr_of_mut!((*mc).crtc_list);
        let mut cur = (*head).next;
        while cur != head {
            let next = (*cur).next;
            let crtc = crtc_from_head(cur);
            if let Some(destroy) = (*(*crtc).funcs).destroy {
                destroy(crtc);
            }
            cur = next;
        }

        // list_for_each_entry_safe(blob, bt, &property_blob_list, head_global)
        let head = core::ptr::addr_of_mut!((*mc).property_blob_list);
        let mut cur = (*head).next;
        while cur != head {
            let next = (*cur).next;
            let blob = cur
                .cast::<u8>()
                .sub(offset_of!(
                    crate::drm_property_types::DrmPropertyBlob,
                    head_global
                ))
                .cast::<crate::drm_property_types::DrmPropertyBlob>();
            drm_property_blob_put(blob);
            cur = next;
        }

        warn_on(!list_empty(&(*mc).fb_list));

        // list_for_each_entry_safe(fb, fbt, &fb_list, head)
        let head = core::ptr::addr_of_mut!((*mc).fb_list);
        let mut cur = (*head).next;
        while cur != head {
            let next = (*cur).next;
            let fb = fb_from_head(cur);
            // The "[leaked fb]" dump is reduced to the id; see the module comment.
            drm_kapi::drm_err!(dev, "framebuffer[%u]: leaked\n", (*fb).base.id);
            drm_framebuffer_free(core::ptr::addr_of_mut!((*fb).base.refcount));
            cur = next;
        }

        drm_kapi::idr::ida_destroy(&mut (*mc).connector_ida);
        idr_destroy(core::ptr::addr_of_mut!((*mc).tile_idr));
        idr_destroy(core::ptr::addr_of_mut!((*mc).object_idr));

        // drm_modeset_lock_fini(): WARN_ON(!list_empty(&lock->head)).
        warn_on(!list_empty(&(*mc).connection_mutex.head));
    }
}

/// `full_encoder_mask()`: OR of every encoder's mask.
// upstream: drm_mode_config.c full_encoder_mask()
/// # Safety
///
/// `dev` must point to an active, valid `DrmDevice` with initialized `mode_config.encoder_list`.
unsafe fn full_encoder_mask(dev: *mut DrmDevice) -> u32 {
    // SAFETY: `dev` is valid and `encoder_list` contains initialized `DrmEncoder` objects.
    unsafe {
        let mut encoder_mask = 0u32;
        let head = core::ptr::addr_of_mut!((*dev).mode_config.encoder_list);
        let mut cur = (*head).next;
        while cur != head {
            encoder_mask |= drm_encoder_mask(encoder_from_head(cur));
            cur = (*cur).next;
        }
        encoder_mask
    }
}

// upstream: drm_mode_config.c fixup_encoder_possible_clones()
/// # Safety
///
/// `encoder` must point to an active, valid `DrmEncoder`.
unsafe fn fixup_encoder_possible_clones(encoder: *mut DrmEncoder) {
    // SAFETY: `encoder` is a valid pointer to `DrmEncoder`.
    unsafe {
        if (*encoder).possible_clones == 0 {
            (*encoder).possible_clones = drm_encoder_mask(encoder);
        }
    }
}

// upstream: drm_mode_config.c validate_encoder_possible_clones()
/// # Safety
///
/// `encoder` must point to an active, valid `DrmEncoder` attached to an active `DrmDevice`.
unsafe fn validate_encoder_possible_clones(encoder: *mut DrmEncoder) {
    // SAFETY: `encoder` and its associated `dev` and mode_config `encoder_list` are valid.
    unsafe {
        let dev = (*encoder).dev;
        let encoder_mask = full_encoder_mask(dev);

        let head = core::ptr::addr_of_mut!((*dev).mode_config.encoder_list);
        let mut cur = (*head).next;
        while cur != head {
            let other = encoder_from_head(cur);
            warn_if!(
                ((*encoder).possible_clones & drm_encoder_mask(other) != 0)
                    != ((*other).possible_clones & drm_encoder_mask(encoder) != 0),
                dev,
                "possible_clones mismatch: [ENCODER:%d:%s] mask=0x%x possible_clones=0x%x vs. \
                 [ENCODER:%d:%s] mask=0x%x possible_clones=0x%x\n",
                (*encoder).base.id,
                (*encoder).name,
                drm_encoder_mask(encoder),
                (*encoder).possible_clones,
                (*other).base.id,
                (*other).name,
                drm_encoder_mask(other),
                (*other).possible_clones
            );
            cur = (*cur).next;
        }

        warn_if!(
            ((*encoder).possible_clones & drm_encoder_mask(encoder)) == 0
                || ((*encoder).possible_clones & !encoder_mask) != 0,
            dev,
            "Bogus possible_clones: [ENCODER:%d:%s] possible_clones=0x%x (full encoder \
             mask=0x%x)\n",
            (*encoder).base.id,
            (*encoder).name,
            (*encoder).possible_clones,
            encoder_mask
        );
    }
}

/// `full_crtc_mask()`: OR of every CRTC's mask.
// upstream: drm_mode_config.c full_crtc_mask()
/// # Safety
///
/// `dev` must point to an active, valid `DrmDevice` with initialized `mode_config.crtc_list`.
unsafe fn full_crtc_mask(dev: *mut DrmDevice) -> u32 {
    // SAFETY: `dev` is valid and `crtc_list` contains initialized `DrmCrtc` objects.
    unsafe {
        let mut crtc_mask = 0u32;
        let head = core::ptr::addr_of_mut!((*dev).mode_config.crtc_list);
        let mut cur = (*head).next;
        while cur != head {
            crtc_mask |= drm_crtc_mask(crtc_from_head(cur));
            cur = (*cur).next;
        }
        crtc_mask
    }
}

// upstream: drm_mode_config.c validate_encoder_possible_crtcs()
/// # Safety
///
/// `encoder` must point to an active, valid `DrmEncoder` attached to an active `DrmDevice`.
unsafe fn validate_encoder_possible_crtcs(encoder: *mut DrmEncoder) {
    // SAFETY: `encoder` and `(*encoder).dev` are valid.
    unsafe {
        let crtc_mask = full_crtc_mask((*encoder).dev);
        warn_if!(
            ((*encoder).possible_crtcs & crtc_mask) == 0
                || ((*encoder).possible_crtcs & !crtc_mask) != 0,
            (*encoder).dev,
            "Bogus possible_crtcs: [ENCODER:%d:%s] possible_crtcs=0x%x (full crtc mask=0x%x)\n",
            (*encoder).base.id,
            (*encoder).name,
            (*encoder).possible_crtcs,
            crtc_mask
        );
    }
}

// upstream: drm_mode_config.c drm_mode_config_validate()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to an active, valid `DrmDevice` with initialized `mode_config` lists.
pub unsafe extern "C" fn drm_mode_config_validate(dev: *mut DrmDevice) {
    // SAFETY: `dev` is valid, and inspecting encoders, CRTCs, and planes reads valid mode_config lists.
    unsafe {
        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return;
        }

        let head = core::ptr::addr_of_mut!((*dev).mode_config.encoder_list);
        let mut cur = (*head).next;
        while cur != head {
            fixup_encoder_possible_clones(encoder_from_head(cur));
            cur = (*cur).next;
        }

        let mut cur = (*head).next;
        while cur != head {
            let encoder = encoder_from_head(cur);
            validate_encoder_possible_clones(encoder);
            validate_encoder_possible_crtcs(encoder);
            cur = (*cur).next;
        }

        let mut primary_with_crtc: u32 = 0;
        let mut cursor_with_crtc: u32 = 0;

        let head = core::ptr::addr_of_mut!((*dev).mode_config.crtc_list);
        let mut cur = (*head).next;
        while cur != head {
            let crtc = crtc_from_head(cur);
            let funcs = (*crtc).funcs;
            let has_cursor = !(*crtc).cursor.is_null();

            warn_if!(
                (*crtc).primary.is_null(),
                dev,
                "Missing primary plane on [CRTC:%d:%s]\n",
                (*crtc).base.id,
                (*crtc).name
            );
            warn_if!(
                has_cursor && (*funcs).cursor_set.is_some(),
                dev,
                "[CRTC:%d:%s] must not have both a cursor plane and a cursor_set func",
                (*crtc).base.id,
                (*crtc).name
            );
            warn_if!(
                has_cursor && (*funcs).cursor_set2.is_some(),
                dev,
                "[CRTC:%d:%s] must not have both a cursor plane and a cursor_set2 func",
                (*crtc).base.id,
                (*crtc).name
            );
            warn_if!(
                has_cursor && (*funcs).cursor_move.is_some(),
                dev,
                "[CRTC:%d:%s] must not have both a cursor plane and a cursor_move func",
                (*crtc).base.id,
                (*crtc).name
            );

            if !(*crtc).primary.is_null() {
                let primary = (*crtc).primary;
                warn_if!(
                    (*primary).possible_crtcs & drm_crtc_mask(crtc) == 0,
                    dev,
                    "Bogus primary plane possible_crtcs: [PLANE:%d:%s] must be compatible with \
                     [CRTC:%d:%s]\n",
                    (*primary).base.id,
                    (*primary).name,
                    (*crtc).base.id,
                    (*crtc).name
                );
                warn_if!(
                    primary_with_crtc & drm_plane_mask(primary) != 0,
                    dev,
                    "Primary plane [PLANE:%d:%s] used for multiple CRTCs",
                    (*primary).base.id,
                    (*primary).name
                );
                primary_with_crtc |= drm_plane_mask(primary);
            }

            if has_cursor {
                let cursor = (*crtc).cursor;
                warn_if!(
                    (*cursor).possible_crtcs & drm_crtc_mask(crtc) == 0,
                    dev,
                    "Bogus cursor plane possible_crtcs: [PLANE:%d:%s] must be compatible with \
                     [CRTC:%d:%s]\n",
                    (*cursor).base.id,
                    (*cursor).name,
                    (*crtc).base.id,
                    (*crtc).name
                );
                warn_if!(
                    cursor_with_crtc & drm_plane_mask(cursor) != 0,
                    dev,
                    "Cursor plane [PLANE:%d:%s] used for multiple CRTCs",
                    (*cursor).base.id,
                    (*cursor).name
                );
                cursor_with_crtc |= drm_plane_mask(cursor);
            }

            cur = (*cur).next;
        }

        let mut num_primary: u32 = 0;
        let head = core::ptr::addr_of_mut!((*dev).mode_config.plane_list);
        let mut cur = (*head).next;
        while cur != head {
            if (*plane_from_head(cur)).r#type == DRM_PLANE_TYPE_PRIMARY {
                num_primary += 1;
            }
            cur = (*cur).next;
        }

        warn_if!(
            num_primary != (*dev).mode_config.num_crtc as u32,
            dev,
            "Must have as many primary planes as there are CRTCs, but have %u primary planes and \
             %u CRTCs",
            num_primary,
            (*dev).mode_config.num_crtc
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDrm;

    #[test]
    fn mode_config_init_creates_standard_properties_and_cleans_up() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        assert_eq!(t.init_mode_config(), 0);
        let dev = t.ptr();
        // SAFETY: `dev` points to a live `DrmDevice` allocated in `TestDrm::new`.
        unsafe {
            let mc = &(*dev).mode_config;
            assert!(!mc.dpms_property.is_null());
            assert!(!mc.edid_property.is_null());
            assert!(!mc.prop_src_x.is_null());
            assert!(!mc.prop_crtc_id.is_null());
            assert!(!drm_kapi::list::list_empty(&mc.property_list));
            assert_eq!(mc.num_connector, 0);
            assert_eq!(mc.num_encoder, 0);
        }
        t.cleanup_mode_config();
        // SAFETY: `dev` points to a live `DrmDevice` allocated in `TestDrm::new`.
        unsafe {
            let mc = &(*dev).mode_config;
            assert!(drm_kapi::list::list_empty(&mc.property_list));
            assert!(drm_kapi::list::list_empty(&mc.encoder_list));
            assert!(drm_kapi::list::list_empty(&mc.connector_list));
        }
    }

    #[test]
    fn getresources_requires_modeset_feature() {
        let mut t = TestDrm::new(0);
        assert_eq!(t.init_mode_config(), 0);
        let dev = t.ptr();
        // SAFETY: `DrmModeCardRes` is a C-compatible struct where all-zero is a valid bit pattern.
        let mut res: crate::uapi_drm_mode_types::DrmModeCardRes = unsafe { core::mem::zeroed() };
        // SAFETY: `DrmFile` is a C-compatible struct where all-zero is a valid bit pattern.
        let mut file: crate::drm_file_types::DrmFile = unsafe { core::mem::zeroed() };
        // SAFETY: `dev` is valid from TestDrm and `res`/`file` are valid stack-allocated buffers.
        let rc =
            unsafe { drm_mode_getresources(dev, core::ptr::addr_of_mut!(res).cast(), &mut file) };
        assert_eq!(rc, -EOPNOTSUPP);
        t.cleanup_mode_config();
    }
}
