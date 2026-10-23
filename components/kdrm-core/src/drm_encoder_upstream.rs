// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_encoder.c`.
//!
//! Encoder registration, `drm_encoder_init()` / `drm_encoder_cleanup()`,
//! the managed `drmm_encoder_init()` / `__drmm_encoder_alloc()` pair, and the
//! `GETENCODER` ioctl.
//!
//! Deviation: the C `...` format arguments of `drm_encoder_init()`,
//! `__drmm_encoder_alloc()` and `drmm_encoder_init()` are not expanded here
//! (stable Rust has no C variadics in this `no_std` crate). The `name`
//! parameter is taken as an already-formatted C string, duplicated verbatim.
//! The default name (`name == NULL`) is still built as `"%s-%d"`.
//!
//! Deviation: the debugfs hooks (`drm_debugfs_encoder_add/remove`) are
//! omitted; they are dead with `CONFIG_DEBUG_FS` off on this target.

use core::{
    ffi::{c_char, c_void},
    mem::{offset_of, size_of},
};

use drm_kapi::{
    assert::warn_on,
    config::{EINVAL, ENOENT, ENOMEM, EOPNOTSUPP, GFP_KERNEL},
    list::{init_list_head, list_add_tail, list_del},
    memory::{kfree, kmalloc},
    mutex::{mutex_destroy, mutex_init},
    types::ListHead,
};

use crate::{
    drm_connector_types::{DrmConnector, DrmConnectorListIter},
    drm_connector_upstream::{
        drm_connector_list_iter_begin, drm_connector_list_iter_end, drm_connector_list_iter_next,
    },
    drm_crtc_types::DrmCrtc,
    drm_device_types::DrmDevice,
    drm_encoder_types::{DrmEncoder, DrmEncoderFuncs},
    drm_file_types::DrmFile,
    drm_mode_object_types::DrmModeObject,
    drm_mode_object_upstream::{
        DRIVER_MODESET, drm_core_check_feature, drm_mode_object_add, drm_mode_object_find,
        drm_mode_object_unregister, mutex_lock, mutex_unlock,
    },
    drm_modeset_lock_types::{DrmModesetAcquireCtx, DrmModesetLock},
    uapi_drm_consts::DRM_MODE_OBJECT_ENCODER,
    uapi_drm_mode_types::DrmModeGetEncoder,
};

// Cross-batch callees: managed resources (drm_managed.c), modeset lock
// # Safety
//
// Foreign declarations of managed resource and modeset locking helpers linked from DRM core.
unsafe extern "C" {
    fn drmm_kmalloc(dev: *mut DrmDevice, size: usize, gfp: u32) -> *mut c_void;
    fn __drmm_add_action_or_reset(
        dev: *mut DrmDevice,
        action: unsafe extern "C" fn(*mut DrmDevice, *mut c_void),
        data: *mut c_void,
        name: *const c_char,
    ) -> i32;
    fn drm_modeset_lock(lock: *mut DrmModesetLock, ctx: *mut DrmModesetAcquireCtx) -> i32;
    fn drm_modeset_unlock(lock: *mut DrmModesetLock);
    fn drm_lease_held(file_priv: *mut DrmFile, id: i32) -> bool;
    fn drm_lease_filter_crtcs(file_priv: *mut DrmFile, crtcs: u32) -> u32;
}

/// Linux `DRM_MODE_OBJECT_*` string names for the default encoder name.
// upstream: drm_encoder.c drm_encoder_enum_list[]
static DRM_ENCODER_ENUM_NAMES: [&[u8]; 9] = [
    b"None", b"DAC", b"TMDS", b"LVDS", b"TV", b"Virtual", b"DSI", b"DP MST", b"DPI",
];

/// `__drmm_add_action_or_reset()` through the `drmm_add_action_or_reset()`
/// macro (drm_managed.h), which passes the action name as a string.
///
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - `action` must be a safe release callback for `data` when `dev` is released.
/// - `data` must remain valid until `action` is invoked by the DRM managed resource tracker,
///   or if registration fails, `action` will be invoked immediately on `data`.
unsafe fn drmm_add_action_or_reset(
    dev: *mut DrmDevice,
    action: unsafe extern "C" fn(*mut DrmDevice, *mut c_void),
    data: *mut c_void,
) -> i32 {
    // SAFETY: `dev`, `action`, and `data` satisfy the function's safety contract; the name is a static C string literal.
    unsafe { __drmm_add_action_or_reset(dev, action, data, c"drmm_encoder_alloc_release".as_ptr()) }
}

/// `drm_encoder_find()` (drm_encoder.h): object lookup for an encoder id.
// upstream: include/drm/drm_encoder.h drm_encoder_find()
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice` with modeset resources registered.
/// - If `file_priv` is non-null, it must point to an open `DrmFile` for lease permission filtering.
pub(crate) unsafe fn drm_encoder_find(
    dev: *mut DrmDevice,
    file_priv: *mut DrmFile,
    id: u32,
) -> *mut DrmEncoder {
    // SAFETY: `dev` is live and `file_priv` is null or an open `DrmFile` per the function's safety contract.
    let obj = unsafe { drm_mode_object_find(dev, file_priv, id, DRM_MODE_OBJECT_ENCODER) };
    // SAFETY: `obj` comes from the encoder-typed lookup above, so it is null or the `base` member of a live `DrmEncoder`, as `obj_to_encoder` requires.
    unsafe { obj_to_encoder(obj) }
}

/// `drm_encoder_mask()`: one bit per encoder index (drm_encoder.h).
// upstream: include/drm/drm_encoder.h drm_encoder_mask()
/// # Safety
///
/// - `encoder` must be non-null, properly aligned, and point to an initialized `DrmEncoder`.
/// - `(*encoder).index` must be strictly less than 32 to prevent bitshift overflow.
pub(crate) unsafe fn drm_encoder_mask(encoder: *const DrmEncoder) -> u32 {
    // SAFETY: `encoder` is non-null and points to an initialized `DrmEncoder` per the function's safety contract.
    unsafe { 1u32 << (*encoder).index }
}

/// `obj_to_encoder()` (drm_encoder.h); NULL-safe like the C container_of
/// helper's callers.
///
/// # Safety
///
/// If `obj` is non-null, it must point to the `base` field (`DrmModeObject`) embedded within an
/// allocated, live `DrmEncoder`. If null, this function safely returns null.
unsafe fn obj_to_encoder(obj: *mut DrmModeObject) -> *mut DrmEncoder {
    if obj.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `obj` is non-null (checked above) and per the function's safety contract points to the `base` member of a live `DrmEncoder`.
    unsafe {
        obj.cast::<u8>()
            .sub(offset_of!(DrmEncoder, base))
            .cast::<DrmEncoder>()
    }
}

/// `list_entry(link, struct drm_encoder, head)`.
///
/// # Safety
///
/// `link` must be non-null and point to the `head` field (`ListHead`) embedded within an allocated,
/// live `DrmEncoder`.
unsafe fn encoder_from_head(link: *mut ListHead) -> *mut DrmEncoder {
    // SAFETY: per the function's safety contract, `link` is the `head` member of a live `DrmEncoder`, so stepping back by its offset stays within that object.
    unsafe {
        link.cast::<u8>()
            .sub(offset_of!(DrmEncoder, head))
            .cast::<DrmEncoder>()
    }
}

/// `kstrdup()`: a kmalloc'd NUL-terminated copy of `src`.
// upstream: mm/util.c kstrdup()
/// # Safety
///
/// `src` must be non-null, properly aligned, and point to a readable, NUL-terminated C string
/// that remains valid and immutable for the duration of the call.
pub(crate) unsafe fn kstrdup_c(src: *const c_char) -> *mut c_char {
    let mut len = 0usize;
    // SAFETY: `src` is a readable NUL-terminated string per the function's safety contract; the loop stops at the NUL, so `src.add(len)` stays within the string.
    while unsafe { *src.add(len) != 0 } {
        len += 1;
    }
    let dst = kmalloc(len + 1, GFP_KERNEL).cast::<c_char>();
    if dst.is_null() {
        return dst;
    }
    // SAFETY: `src` is readable for `len` bytes up to its NUL terminator per the function's safety contract; `dst` is a fresh non-null `kmalloc` of `len + 1` bytes, so the ranges do not overlap.
    unsafe { core::ptr::copy_nonoverlapping(src, dst, len) };
    // SAFETY: `dst` is a non-null `kmalloc` of `len + 1` bytes, so index `len` is in bounds.
    unsafe { *dst.add(len) = 0 };
    dst
}

/// `kasprintf(GFP_KERNEL, "%s-%d", prefix, id)`.
// upstream: lib/kasprintf.c kasprintf()
/// # Safety
///
/// This function allocates kernel memory using `kmalloc`. The caller is responsible for freeing the
/// returned pointer with `kfree` when it is no longer needed.
pub(crate) unsafe fn kasprintf_name_id(prefix: &[u8], id: u32) -> *mut c_char {
    // Decimal digits of `id`, most significant first.
    let mut digits = [0u8; 10];
    let mut n = id;
    let mut count = 0usize;
    loop {
        digits[count] = b'0' + (n % 10) as u8;
        count += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let total = prefix.len() + 1 + count;
    let dst = kmalloc(total + 1, GFP_KERNEL).cast::<u8>();
    if dst.is_null() {
        return dst.cast();
    }
    let mut at = 0usize;
    for &b in prefix {
        // SAFETY: `at < prefix.len() < total`, within the `total + 1` byte `kmalloc` allocation of `dst`.
        unsafe { *dst.add(at) = b };
        at += 1;
    }
    // SAFETY: `at == prefix.len() < total`, within the `total + 1` byte `kmalloc` allocation of `dst`.
    unsafe { *dst.add(at) = b'-' };
    at += 1;
    for i in (0..count).rev() {
        // SAFETY: `at` stays below `total`, within the `total + 1` byte `kmalloc` allocation of `dst`.
        unsafe { *dst.add(at) = digits[i] };
        at += 1;
    }
    // SAFETY: `at == total` after the writes above, which is within the `total + 1` byte `kmalloc` allocation of `dst`.
    unsafe { *dst.add(at) = 0 };
    dst.cast()
}

// upstream: drm_encoder.c drm_encoder_register_all()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - Caller must hold `dev.mode_config.mutex` to protect `mode_config.encoder_list` traversal.
/// - All encoder objects on `mode_config.encoder_list` must remain live and have valid `funcs` vtables.
pub unsafe extern "C" fn drm_encoder_register_all(dev: *mut DrmDevice) -> i32 {
    // SAFETY:
    // - `dev` points to an initialized `DrmDevice` and `mode_config.encoder_list` is an initialized circular list head.
    // - Each node traversed is linked via `(*encoder).head` in a live `DrmEncoder`.
    // - If `late_register` hook is present in `(*encoder).funcs`, it is invoked with the live `encoder` pointer.
    unsafe {
        let head = core::ptr::addr_of_mut!((*dev).mode_config.encoder_list);
        let mut cur = (*head).next;
        let mut ret = 0;
        while cur != head {
            let encoder = encoder_from_head(cur);
            if !(*encoder).funcs.is_null()
                && let Some(late_register) = (*(*encoder).funcs).late_register
            {
                ret = late_register(encoder);
            }
            if ret != 0 {
                return ret;
            }
            cur = (*cur).next;
        }
        0
    }
}

// upstream: drm_encoder.c drm_encoder_unregister_all()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - Caller must hold `dev.mode_config.mutex` to protect `mode_config.encoder_list` traversal.
/// - All encoder objects on `mode_config.encoder_list` must remain live and have valid `funcs` vtables.
pub unsafe extern "C" fn drm_encoder_unregister_all(dev: *mut DrmDevice) {
    // SAFETY:
    // - `dev` is an initialized `DrmDevice` and `mode_config.encoder_list` is an initialized list head.
    // - Traversed nodes correspond to `head` in live `DrmEncoder` structures.
    // - If `early_unregister` hook is present, it is called with the valid `encoder` pointer.
    unsafe {
        let head = core::ptr::addr_of_mut!((*dev).mode_config.encoder_list);
        let mut cur = (*head).next;
        while cur != head {
            let encoder = encoder_from_head(cur);
            if !(*encoder).funcs.is_null()
                && let Some(early_unregister) = (*(*encoder).funcs).early_unregister
            {
                early_unregister(encoder);
            }
            cur = (*cur).next;
        }
    }
}

// upstream: drm_encoder.c __drm_encoder_init()
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - `encoder` must point to allocated, properly aligned, writable memory of at least
///   `size_of::<DrmEncoder>()` bytes with no conflicting active borrows.
/// - `funcs` must point to a valid `DrmEncoderFuncs` vtable that lives at least as long as `encoder`.
/// - If `name` is non-null, it must point to a valid, live, NUL-terminated C string.
/// - Caller must hold `dev.mode_config.mutex` to serialize encoder index and list registration.
unsafe fn __drm_encoder_init(
    dev: *mut DrmDevice,
    encoder: *mut DrmEncoder,
    funcs: *const DrmEncoderFuncs,
    encoder_type: i32,
    name: *const c_char,
) -> i32 {
    // SAFETY:
    // - `dev` and `encoder` point to valid memory per caller contract.
    // - `drm_mode_object_add` registers `(*encoder).base` in the IDR of `dev`.
    // - `(*encoder).name` is allocated via `kstrdup_c` or `kasprintf_name_id`; on allocation failure,
    //   `drm_mode_object_unregister` cleans up the registered mode object before returning `-ENOMEM`.
    // - `mutex_init` initializes `bridge_chain_mutex` in-place on `encoder`.
    // - `list_add_tail` inserts `(*encoder).head` into `(*dev).mode_config.encoder_list`.
    unsafe {
        let mut ret: i32;

        // encoder index is used with 32bit bitmasks
        if warn_on((*dev).mode_config.num_encoder >= 32) {
            return -EINVAL;
        }

        ret = drm_mode_object_add(
            dev,
            core::ptr::addr_of_mut!((*encoder).base),
            DRM_MODE_OBJECT_ENCODER,
        );
        if ret != 0 {
            return ret;
        }

        (*encoder).dev = dev;
        (*encoder).encoder_type = encoder_type;
        (*encoder).funcs = funcs.cast_mut();

        if !name.is_null() {
            (*encoder).name = kstrdup_c(name);
        } else {
            let prefix = DRM_ENCODER_ENUM_NAMES[encoder_type as usize];
            (*encoder).name = kasprintf_name_id(prefix, (*encoder).base.id);
        }

        if (*encoder).name.is_null() {
            ret = -ENOMEM;
            // out_put:
            drm_mode_object_unregister(dev, core::ptr::addr_of_mut!((*encoder).base));
            return ret;
        }

        init_list_head(core::ptr::addr_of_mut!((*encoder).bridge_chain));
        mutex_init(core::ptr::addr_of_mut!((*encoder).bridge_chain_mutex));
        list_add_tail(
            core::ptr::addr_of_mut!((*encoder).head),
            core::ptr::addr_of_mut!((*dev).mode_config.encoder_list),
        );
        (*encoder).index = (*dev).mode_config.num_encoder as u32;
        (*dev).mode_config.num_encoder += 1;

        0
    }
}

// upstream: drm_encoder.c drm_encoder_init()
//
// The C variadic `name` format is not expanded; see the module comment.
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - `encoder` must point to allocated, properly aligned, writable `DrmEncoder` memory.
/// - `funcs` must point to a valid `DrmEncoderFuncs` table where `destroy` is `Some(...)`.
/// - If `name` is non-null, it must point to a readable NUL-terminated C string.
/// - Caller must hold `dev.mode_config.mutex` to protect mode config encoder list registration.
pub unsafe extern "C" fn drm_encoder_init(
    dev: *mut DrmDevice,
    encoder: *mut DrmEncoder,
    funcs: *const DrmEncoderFuncs,
    encoder_type: i32,
    name: *const c_char,
) -> i32 {
    // SAFETY: `dev` and `encoder` point to allocated DRM structures, `funcs` points to a static
    // callback table with a verified `destroy` hook, and `name` is a valid format string or null.
    unsafe {
        warn_on((*funcs).destroy.is_none());
        __drm_encoder_init(dev, encoder, funcs, encoder_type, name)
    }
}

// upstream: drm_encoder.c drm_encoder_cleanup()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `encoder` must point to an initialized, live `DrmEncoder` previously registered with `drm_encoder_init`.
/// - Caller must hold `dev.mode_config.mutex` during cleanup.
/// - The encoder must not be currently in use by active CRTCs or connectors.
pub unsafe extern "C" fn drm_encoder_cleanup(encoder: *mut DrmEncoder) {
    // SAFETY:
    // - `encoder` points to a live `DrmEncoder` and `(*encoder).dev` points to the owning `DrmDevice`.
    // - `mutex_lock` / `mutex_unlock` synchronize checking `(*encoder).bridge_chain`.
    // - `drm_mode_object_unregister` removes the mode object ID from the device IDR.
    // - `kfree((*encoder).name)` frees the duplicated name string allocated during initialization.
    // - `list_del` unlinks `(*encoder).head` from `mode_config.encoder_list`.
    // - `mutex_destroy` tears down `bridge_chain_mutex`.
    // - `write_bytes` zeroes the entire `DrmEncoder` storage to prevent stale reuse.
    unsafe {
        let dev = (*encoder).dev;

        // Note that the encoder_list is considered to be static; should we
        // remove the drm_encoder at runtime we would have to decrement all
        // the indices on the drm_encoder after us in the encoder_list.

        mutex_lock(core::ptr::addr_of_mut!((*encoder).bridge_chain_mutex));
        let bridge_chain_empty = drm_kapi::list::list_empty(&(*encoder).bridge_chain);
        mutex_unlock(core::ptr::addr_of_mut!((*encoder).bridge_chain_mutex));
        assert!(
            bridge_chain_empty,
            "unsupported bridge chain reached encoder cleanup"
        );

        drm_mode_object_unregister(dev, core::ptr::addr_of_mut!((*encoder).base));

        kfree((*encoder).name);
        list_del(core::ptr::addr_of_mut!((*encoder).head));
        (*dev).mode_config.num_encoder -= 1;

        mutex_destroy(&mut (*encoder).bridge_chain_mutex);

        core::ptr::write_bytes(encoder.cast::<u8>(), 0, size_of::<DrmEncoder>());
    }
}

// upstream: drm_encoder.c drmm_encoder_alloc_release()
/// # Safety
///
/// `ptr` must point to a `DrmEncoder` embedded in a container allocated via `drmm_kmalloc` and
/// initialized with managed encoder initialization.
unsafe extern "C" fn drmm_encoder_alloc_release(_dev: *mut DrmDevice, ptr: *mut c_void) {
    // SAFETY: `ptr` points to the managed `DrmEncoder` whose `dev` field is validated non-null before
    // invoking `drm_encoder_cleanup`.
    unsafe {
        let encoder = ptr.cast::<DrmEncoder>();
        if warn_on((*encoder).dev.is_null()) {
            return;
        }
        drm_encoder_cleanup(encoder);
    }
}

// upstream: drm_encoder.c __drmm_encoder_init()
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - `encoder` must point to allocated, properly aligned, writable `DrmEncoder` storage.
/// - `funcs` must point to a valid `DrmEncoderFuncs` vtable whose `destroy` hook is `None` (cleanup
///   is managed automatically).
/// - If `name` is non-null, it must point to a valid NUL-terminated C string.
unsafe fn __drmm_encoder_init(
    dev: *mut DrmDevice,
    encoder: *mut DrmEncoder,
    funcs: *const DrmEncoderFuncs,
    encoder_type: i32,
    name: *const c_char,
) -> i32 {
    // SAFETY:
    // - `funcs.destroy` is verified to be `None` before delegating to `__drm_encoder_init`.
    // - `drmm_add_action_or_reset` registers `drmm_encoder_alloc_release` on `dev` with `encoder` as context,
    //   ensuring cleanup is called when `dev` is destroyed or if action registration fails.
    unsafe {
        if !funcs.is_null() && (*funcs).destroy.is_some() {
            // drm_WARN_ON(dev, funcs && funcs->destroy)
            warn_on(true);
            return -EINVAL;
        }

        let ret = __drm_encoder_init(dev, encoder, funcs, encoder_type, name);
        if ret != 0 {
            return ret;
        }

        let ret = drmm_add_action_or_reset(dev, drmm_encoder_alloc_release, encoder.cast());
        if ret != 0 {
            return ret;
        }

        0
    }
}

// upstream: drm_encoder.c __drmm_encoder_alloc()
//
// The C variadic `name` format is not expanded; see the module comment.
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - `size` must be at least `offset + size_of::<DrmEncoder>()`.
/// - `offset` must ensure that `container + offset` is properly aligned for `DrmEncoder` (8-byte aligned).
/// - `funcs` must point to a valid `DrmEncoderFuncs` table where `destroy` is `None`.
/// - If `name` is non-null, it must point to a valid NUL-terminated C string.
pub unsafe extern "C" fn __drmm_encoder_alloc(
    dev: *mut DrmDevice,
    size: usize,
    offset: usize,
    funcs: *const DrmEncoderFuncs,
    encoder_type: i32,
    name: *const c_char,
) -> *mut c_void {
    // SAFETY:
    // - `drmm_kmalloc` allocates `size` bytes zeroed and tracks them under `dev`.
    // - If allocation succeeds, `container.add(offset)` is in-bounds and aligned for `DrmEncoder` per caller contract.
    // - `__drmm_encoder_init` initializes the encoder and registers managed destruction.
    unsafe {
        let container = drmm_kmalloc(dev, size, GFP_KERNEL | 0x100 /* __GFP_ZERO */);
        if container.is_null() {
            return drm_kapi::config::err_ptr::<c_void>(-ENOMEM);
        }

        let encoder = container.cast::<u8>().add(offset).cast::<DrmEncoder>();

        let ret = __drmm_encoder_init(dev, encoder, funcs, encoder_type, name);
        if ret != 0 {
            return drm_kapi::config::err_ptr::<c_void>(ret);
        }

        container
    }
}

// upstream: drm_encoder.c drmm_encoder_init()
//
// The C variadic `name` format is not expanded; see the module comment.
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - `encoder` must point to allocated, properly aligned, writable `DrmEncoder` memory.
/// - `funcs` must point to a valid `DrmEncoderFuncs` table where `destroy` is `None`.
/// - If `name` is non-null, it must point to a valid NUL-terminated C string.
pub unsafe extern "C" fn drmm_encoder_init(
    dev: *mut DrmDevice,
    encoder: *mut DrmEncoder,
    funcs: *const DrmEncoderFuncs,
    encoder_type: i32,
    name: *const c_char,
) -> i32 {
    // SAFETY: Forwards valid pointers `dev`, allocated storage `encoder`, function table `funcs`,
    // and optional C-string `name` to initialize and register the encoder with `dev` managed resources.
    unsafe { __drmm_encoder_init(dev, encoder, funcs, encoder_type, name) }
}

// upstream: drm_encoder.c drm_encoder_get_crtc()
/// # Safety
///
/// - `encoder` must point to an initialized, live `DrmEncoder` whose `dev` field points to an
///   initialized `DrmDevice`.
/// - Caller must hold `dev.mode_config.connection_mutex` to protect connector list and state inspection.
unsafe fn drm_encoder_get_crtc(encoder: *mut DrmEncoder) -> *mut DrmCrtc {
    // SAFETY:
    // - `encoder` and `(*encoder).dev` are guaranteed valid by the caller contract while holding `connection_mutex`.
    // - `conn_iter` is initialized with zeroed storage and managed via `drm_connector_list_iter_begin` and `_end`.
    // - `(*connector).state` is null-checked before reading `best_encoder` and `crtc`.
    unsafe {
        let dev = (*encoder).dev;
        let mut uses_atomic = false;
        let mut conn_iter = core::mem::MaybeUninit::<DrmConnectorListIter>::zeroed();
        let conn_iter = conn_iter.as_mut_ptr();

        drm_connector_list_iter_begin(dev, conn_iter);
        loop {
            let connector: *mut DrmConnector = drm_connector_list_iter_next(conn_iter);
            if connector.is_null() {
                break;
            }
            if (*connector).state.is_null() {
                continue;
            }

            uses_atomic = true;
            if (*(*connector).state).best_encoder != encoder {
                continue;
            }

            drm_connector_list_iter_end(conn_iter);
            return (*(*connector).state).crtc;
        }
        drm_connector_list_iter_end(conn_iter);

        // Don't return stale data (e.g. pending async disable).
        if uses_atomic {
            return core::ptr::null_mut();
        }
        (*encoder).crtc
    }
}

// upstream: drm_encoder.c drm_mode_getencoder()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an initialized, live `DrmDevice`.
/// - `data` must be properly aligned and point to a writable `DrmModeGetEncoder` structure.
/// - `file_priv` must point to a live `DrmFile` associated with the calling process.
pub unsafe extern "C" fn drm_mode_getencoder(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY:
    // - `enc_resp` is cast from `data`, which caller guarantees is an aligned, writable `DrmModeGetEncoder`.
    // - `drm_core_check_feature` is safely called on the live `dev`.
    // - `drm_encoder_find` looks up the encoder by `encoder_id` under object idr locks.
    // - `drm_modeset_lock` acquires `connection_mutex` to protect `drm_encoder_get_crtc`.
    // - `drm_lease_held` and `drm_lease_filter_crtcs` safely check lease permissions on `file_priv`.
    // - `drm_modeset_unlock` releases `connection_mutex`.
    unsafe {
        let enc_resp = data.cast::<DrmModeGetEncoder>();

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        let encoder = drm_encoder_find(dev, file_priv, (*enc_resp).encoder_id);
        if encoder.is_null() {
            return -ENOENT;
        }

        drm_modeset_lock(
            core::ptr::addr_of_mut!((*dev).mode_config.connection_mutex),
            core::ptr::null_mut(),
        );
        let crtc = drm_encoder_get_crtc(encoder);
        if !crtc.is_null() && drm_lease_held(file_priv, (*crtc).base.id as i32) {
            (*enc_resp).crtc_id = (*crtc).base.id;
        } else {
            (*enc_resp).crtc_id = 0;
        }
        drm_modeset_unlock(core::ptr::addr_of_mut!((*dev).mode_config.connection_mutex));

        (*enc_resp).encoder_type = (*encoder).encoder_type as u32;
        (*enc_resp).encoder_id = (*encoder).base.id;
        (*enc_resp).possible_crtcs = drm_lease_filter_crtcs(file_priv, (*encoder).possible_crtcs);
        (*enc_resp).possible_clones = (*encoder).possible_clones;

        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_support::TestDrm, uapi_drm_consts::DRM_MODE_ENCODER_DAC};

    /// # Safety
    /// No-op callback for unit testing.
    unsafe extern "C" fn noop_destroy(_encoder: *mut DrmEncoder) {}

    fn funcs() -> DrmEncoderFuncs {
        DrmEncoderFuncs {
            reset: None,
            destroy: Some(noop_destroy),
            late_register: None,
            early_unregister: None,
            debugfs_init: None,
        }
    }

    #[test]
    fn encoder_init_names_indexes_and_cleanup() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        assert_eq!(t.init_mode_config(), 0);
        let dev = t.ptr();
        let f = funcs();
        // SAFETY: `DrmEncoder` is zero-initialized for test setup with null pointers and zero flags.
        let mut enc: DrmEncoder = unsafe { core::mem::zeroed() };
        // SAFETY: Test harness provides valid dummy DrmDevice and stack-allocated encoder.
        unsafe {
            assert_eq!(
                drm_encoder_init(
                    dev,
                    &mut enc,
                    &f,
                    DRM_MODE_ENCODER_DAC as i32,
                    core::ptr::null()
                ),
                0
            );
            assert_eq!(enc.index, 0);
            assert_eq!((*dev).mode_config.num_encoder, 1);
            assert_eq!(drm_encoder_mask(&enc), 1);
            // Default name: "<type>-<object id>".
            let name = core::ffi::CStr::from_ptr(enc.name).to_str().unwrap();
            assert!(name.starts_with("DAC-"), "{name}");

            drm_encoder_cleanup(&mut enc);
            assert_eq!((*dev).mode_config.num_encoder, 0);
            assert!(drm_kapi::list::list_empty(&(*dev).mode_config.encoder_list));
        }
        t.cleanup_mode_config();
    }

    #[test]
    fn second_encoder_gets_the_next_index_and_mask() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        assert_eq!(t.init_mode_config(), 0);
        let dev = t.ptr();
        let f = funcs();
        // SAFETY: First `DrmEncoder` instance zero-initialized for index assignment test.
        let mut a: DrmEncoder = unsafe { core::mem::zeroed() };
        // SAFETY: Second `DrmEncoder` instance zero-initialized for index assignment test.
        let mut b: DrmEncoder = unsafe { core::mem::zeroed() };
        // SAFETY: Test harness provides valid dummy DrmDevice and stack-allocated encoders.
        unsafe {
            assert_eq!(
                drm_encoder_init(
                    dev,
                    &mut a,
                    &f,
                    DRM_MODE_ENCODER_DAC as i32,
                    b"custom\0".as_ptr().cast()
                ),
                0
            );
            assert_eq!(
                drm_encoder_init(
                    dev,
                    &mut b,
                    &f,
                    DRM_MODE_ENCODER_DAC as i32,
                    core::ptr::null()
                ),
                0
            );
            assert_eq!(b.index, 1);
            assert_eq!(drm_encoder_mask(&b), 2);
            assert_eq!(core::ffi::CStr::from_ptr(a.name).to_bytes(), b"custom");
            drm_encoder_cleanup(&mut a);
            drm_encoder_cleanup(&mut b);
        }
        t.cleanup_mode_config();
    }
}
