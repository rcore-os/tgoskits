// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_mode_object.c`.
//!
//! Object id registry (`idr_alloc`/`idr_replace`/`idr_remove`/`idr_find` on
//! `mode_config.object_idr`), refcounted object lookup, property attachment
//! and value access, and the `OBJ_GET_PROPERTIES` / `OBJ_SET_PROPERTY` ioctls.
//! The `idr_*` helpers are exported to `drm_connector_upstream.rs` and
//! `drm_mode_config_upstream.rs` (tile and object ids) through `pub(crate)`.

use core::{
    ffi::{c_ulong, c_void},
    mem::{MaybeUninit, offset_of, size_of},
};

/// Mutex entry points for the B1a batch.
///
/// Production builds use the kapi mutexes unchanged. Host tests run without a
/// blocking task context (kapi asserts one in `mutex_lock`), so under
/// `cfg(test)` these are no-op shims; the test binary runs single-threaded.
#[cfg(not(test))]
pub(crate) use drm_kapi::mutex::{mutex_lock, mutex_unlock};
use drm_kapi::{
    assert::warn_on,
    config::{EDEADLK, EFAULT, EINVAL, ENOENT, ENOMEM, EOPNOTSUPP},
    memory::{kref_get, kref_get_unless_zero, kref_put, kref_read},
    mm_native::copy_to_user_c,
    types::{Kref, Mutex},
};

use crate::{
    drm_atomic_types::DrmAtomicCommit,
    drm_connector_types::{DRM_CONNECTOR_REGISTERED, DrmConnector},
    drm_connector_upstream::drm_connector_set_obj_prop,
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_mode_object_types::{DrmModeObject, DrmObjectProperties},
    drm_modeset_lock_types::DrmModesetAcquireCtx,
    drm_plane_types::DrmPlane,
    drm_property_types::DrmProperty,
    drm_property_upstream::{drm_property_change_valid_get, drm_property_change_valid_put},
    uapi_drm_consts::{
        DRM_MODE_OBJECT_ANY, DRM_MODE_OBJECT_CONNECTOR, DRM_MODE_OBJECT_CRTC,
        DRM_MODE_OBJECT_PLANE, DRM_MODE_PROP_ATOMIC, DRM_MODE_PROP_IMMUTABLE,
    },
    uapi_drm_mode_types::{DrmModeObjGetProperties, DrmModeObjSetProperty},
};

/// `mutex_lock()`: host-test shim (see the module comment above).
///
/// # Safety
///
/// In test shims this is a no-op; caller must ensure lock ownership rules are respected in non-test configurations.
#[cfg(test)]
pub(crate) unsafe fn mutex_lock<M: drm_kapi::mutex::MutexPointer>(_mutex: M) {}

/// `mutex_unlock()`: host-test shim (see the module comment above).
///
/// # Safety
///
/// In test shims this is a no-op; caller must ensure lock ownership rules are respected in non-test configurations.
#[cfg(test)]
pub(crate) unsafe fn mutex_unlock<M: drm_kapi::mutex::MutexPointer>(_mutex: M) {}

/// `DRM_OBJECT_MAX_PROPERTY` (include/drm/drm_mode_object.h).
pub const DRM_OBJECT_MAX_PROPERTY: i32 = 64;

/// `DRIVER_MODESET` (include/drm/drm_drv.h).
pub const DRIVER_MODESET: u32 = 1 << 1;
/// `DRIVER_ATOMIC` (include/drm/drm_drv.h).
pub const DRIVER_ATOMIC: u32 = 1 << 4;
/// `DRIVER_GEM` (include/drm/drm_drv.h).
pub const DRIVER_GEM: u32 = 1 << 0;
/// `DRIVER_RENDER` (include/drm/drm_drv.h).
pub const DRIVER_RENDER: u32 = 1 << 3;

// Cross-batch callees: modeset locking (drm_modeset_lock.c), atomic uapi
// (drm_atomic_uapi.c), CRTC/plane property setters (drm_crtc.c, drm_plane.c),
// and drm_lease.c.
unsafe extern "C" {
    fn drm_modeset_acquire_init(ctx: *mut DrmModesetAcquireCtx, flags: u32);
    fn drm_modeset_acquire_fini(ctx: *mut DrmModesetAcquireCtx);
    fn drm_modeset_drop_locks(ctx: *mut DrmModesetAcquireCtx);
    fn drm_modeset_backoff(ctx: *mut DrmModesetAcquireCtx) -> i32;
    fn drm_modeset_lock_all_ctx(dev: *mut DrmDevice, ctx: *mut DrmModesetAcquireCtx) -> i32;
    fn drm_mode_crtc_set_obj_prop(
        obj: *mut DrmModeObject,
        prop: *mut DrmProperty,
        value: u64,
    ) -> i32;
    fn drm_mode_plane_set_obj_prop(plane: *mut DrmPlane, prop: *mut DrmProperty, value: u64)
    -> i32;
    fn drm_atomic_get_property(
        obj: *mut DrmModeObject,
        prop: *mut DrmProperty,
        val: *mut u64,
    ) -> i32;
    fn drm_atomic_commit_alloc(dev: *mut DrmDevice) -> *mut DrmAtomicCommit;
    fn drm_atomic_commit_clear(state: *mut DrmAtomicCommit);
    fn drm_atomic_commit(state: *mut DrmAtomicCommit) -> i32;
    fn drm_atomic_connector_commit_dpms(
        state: *mut DrmAtomicCommit,
        connector: *mut DrmConnector,
        mode: i32,
    ) -> i32;
    fn drm_atomic_set_property(
        state: *mut DrmAtomicCommit,
        file_priv: *mut DrmFile,
        obj: *mut DrmModeObject,
        prop: *mut DrmProperty,
        prop_value: u64,
        async_update: bool,
    ) -> i32;
    fn __drm_atomic_commit_free(kref: *mut Kref);
    fn _drm_lease_held(file_priv: *mut DrmFile, id: i32) -> bool;
}

/// Logs a DRM KMS debug message (`drm_dbg_kms()`), via the kapi DRM logger.
macro_rules! drm_dbg_kms {
    ($device:expr, $format:expr $(, $argument:expr)* $(,)?) => {
        drm_kapi::drm_dbg!($device, $format $(, $argument)*)
    };
}

/// Linux `WARN_ON()` on a C-style condition.
macro_rules! WARN_ON_C {
    ($cond:expr) => {
        warn_on($cond)
    };
}

pub use drm_kapi::idr::{
    idr_alloc, idr_destroy, idr_find, idr_for_each, idr_get_next, idr_init, idr_init_base,
    idr_is_empty, idr_preload, idr_preload_end, idr_remove, idr_replace,
};

/// `to_drm_device()`-style helper: `drm_core_check_feature()` from drm_drv.h.
// upstream: include/drm/drm_drv.h drm_core_check_feature()
/// # Safety
///
/// `dev` must point to an active, valid `DrmDevice` with an initialized `driver`.
pub(crate) unsafe fn drm_core_check_feature(dev: *const DrmDevice, feature: u32) -> bool {
    // SAFETY: `dev` is live with an initialized `driver` per the function's safety contract.
    let supported = unsafe { (*(*dev).driver).driver_features & (*dev).driver_features };
    feature != 0 && (supported & feature) == feature
}

/// `drm_drv_uses_atomic_modeset()` from drm_drv.h.
// upstream: include/drm/drm_drv.h drm_drv_uses_atomic_modeset()
/// # Safety
///
/// `dev` must point to an active, valid `DrmDevice`.
pub(crate) unsafe fn drm_drv_uses_atomic_modeset(dev: *mut DrmDevice) -> bool {
    // SAFETY: `dev` is live per the function's safety contract; `mode_config.funcs` is checked non-null before it is dereferenced.
    unsafe {
        drm_core_check_feature(dev, DRIVER_ATOMIC)
            || (!(*dev).mode_config.funcs.is_null()
                && (*(*dev).mode_config.funcs).atomic_commit.is_some())
    }
}

/// `obj_to_connector()` from drm_connector.h.
/// # Safety
///
/// `obj` must point to the embedded `base` field of an active `DrmConnector`.
pub(crate) unsafe fn obj_to_connector(obj: *mut DrmModeObject) -> *mut DrmConnector {
    // SAFETY: per the function's safety contract, `obj` points to the `base` member of a live `DrmConnector`, so stepping back by its offset stays within that object.
    unsafe {
        obj.cast::<u8>()
            .sub(offset_of!(DrmConnector, base))
            .cast::<DrmConnector>()
    }
}

/// `obj_to_plane()` from drm_plane.h.
/// # Safety
///
/// `obj` must point to the embedded `base` field of an active `DrmPlane`.
pub(crate) unsafe fn obj_to_plane(obj: *mut DrmModeObject) -> *mut DrmPlane {
    // SAFETY: per the function's safety contract, `obj` points to the `base` member of a live `DrmPlane`, so stepping back by its offset stays within that object.
    unsafe {
        obj.cast::<u8>()
            .sub(offset_of!(DrmPlane, base))
            .cast::<DrmPlane>()
    }
}

/// `put_user()` for a single scalar: returns 0 or -EFAULT.
// upstream: include/linux/uaccess.h put_user()
/// # Safety
///
/// `dst` must point to a valid userspace memory location of size `size_of::<T>()`.
pub(crate) unsafe fn put_user<T: Copy>(dst: *mut T, value: T) -> i32 {
    // SAFETY: per the function's safety contract, `dst` is a valid userspace location for `size_of::<T>()` bytes; the source is a local `value` of that size.
    let rest = unsafe {
        copy_to_user_c(
            dst.cast::<c_void>(),
            core::ptr::addr_of!(value).cast::<c_void>(),
            size_of::<T>(),
        )
    };
    if rest == 0 { 0 } else { -EFAULT }
}

// upstream: drm_mode_object.c __drm_mode_object_add()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to an active, valid `DrmDevice` with initialized `mode_config.object_idr`.
/// - `obj` must point to a writable `DrmModeObject` whose lifetime covers its presence in the IDR.
pub unsafe extern "C" fn __drm_mode_object_add(
    dev: *mut DrmDevice,
    obj: *mut DrmModeObject,
    obj_type: u32,
    register_obj: bool,
    obj_free_cb: Option<unsafe extern "C" fn(*mut Kref)>,
) -> i32 {
    // SAFETY: `dev` and `obj` are valid; allocating ID from `mode_config.object_idr`
    // under `idr_mutex` sets up the object ID and initializes kref if refcounted.
    unsafe {
        WARN_ON_C!((*(*dev).driver).load.is_none() && (*dev).registered && obj_free_cb.is_none());

        let idr_mutex = &mut (*dev).mode_config.idr_mutex as *mut Mutex;
        mutex_lock(idr_mutex);
        let ptr = if register_obj {
            obj.cast::<c_void>()
        } else {
            core::ptr::null_mut()
        };
        let ret = idr_alloc(&mut (*dev).mode_config.object_idr, ptr, 1, 0);
        if ret >= 0 {
            // Set up the object linking under the protection of the idr lock
            // so that other users can't see inconsistent state.
            (*obj).id = ret as u32;
            (*obj).r#type = obj_type;
            if let Some(free_cb) = obj_free_cb {
                (*obj).free_cb = Some(free_cb);
                drm_kapi::memory::kref_init(core::ptr::addr_of_mut!((*obj).refcount));
            }
        }
        mutex_unlock(idr_mutex);
        if ret < 0 { ret } else { 0 }
    }
}

// upstream: drm_mode_object.c drm_mode_object_add()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to an active `DrmDevice`, and `obj` must point to a valid `DrmModeObject`.
pub unsafe extern "C" fn drm_mode_object_add(
    dev: *mut DrmDevice,
    obj: *mut DrmModeObject,
    obj_type: u32,
) -> i32 {
    // SAFETY: Forwarding valid device and mode object pointers to `__drm_mode_object_add`.
    unsafe { __drm_mode_object_add(dev, obj, obj_type, true, None) }
}

// upstream: drm_mode_object.c drm_mode_object_register()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to an active `DrmDevice`, and `obj` must point to a previously allocated `DrmModeObject`.
pub unsafe extern "C" fn drm_mode_object_register(dev: *mut DrmDevice, obj: *mut DrmModeObject) {
    // SAFETY: `dev` and `obj` are valid; replacing IDR slot under `idr_mutex` publishes the object pointer.
    unsafe {
        let idr_mutex = &mut (*dev).mode_config.idr_mutex as *mut Mutex;
        mutex_lock(idr_mutex);
        idr_replace(
            &mut (*dev).mode_config.object_idr,
            obj.cast::<c_void>(),
            (*obj).id as i32,
        );
        mutex_unlock(idr_mutex);
    }
}

// upstream: drm_mode_object.c drm_mode_object_unregister()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to an active `DrmDevice`, and `object` must point to a valid registered `DrmModeObject`.
pub unsafe extern "C" fn drm_mode_object_unregister(
    dev: *mut DrmDevice,
    object: *mut DrmModeObject,
) {
    // SAFETY: `dev` and `object` are valid; removing ID from `object_idr` under `idr_mutex` unregisters the object.
    unsafe {
        WARN_ON_C!(
            (*(*dev).driver).load.is_none() && (*dev).registered && (*object).free_cb.is_none()
        );

        let idr_mutex = &mut (*dev).mode_config.idr_mutex as *mut Mutex;
        mutex_lock(idr_mutex);
        if (*object).id != 0 {
            idr_remove(&mut (*dev).mode_config.object_idr, (*object).id as i32);
            (*object).id = 0;
        }
        mutex_unlock(idr_mutex);
    }
}

// upstream: drm_mode_object.c drm_mode_object_lease_required()
#[unsafe(no_mangle)]
pub extern "C" fn drm_mode_object_lease_required(r#type: u32) -> bool {
    matches!(
        r#type,
        DRM_MODE_OBJECT_CRTC | DRM_MODE_OBJECT_CONNECTOR | DRM_MODE_OBJECT_PLANE
    )
}

// upstream: drm_mode_object.c __drm_mode_object_find()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to an active `DrmDevice`, and `file_priv` (if non-null) must point to a valid `DrmFile`.
pub unsafe extern "C" fn __drm_mode_object_find(
    dev: *mut DrmDevice,
    file_priv: *mut DrmFile,
    id: u32,
    r#type: u32,
) -> *mut DrmModeObject {
    // SAFETY: `dev` is valid; looking up object by ID in `object_idr` under `idr_mutex`
    // checks lease permissions and acquires a kref reference if refcounted.
    unsafe {
        let idr_mutex = &mut (*dev).mode_config.idr_mutex as *mut Mutex;
        mutex_lock(idr_mutex);
        let mut obj: *mut DrmModeObject = idr_find(
            core::ptr::addr_of_mut!((*dev).mode_config.object_idr),
            id as c_ulong,
        )
        .cast::<DrmModeObject>();
        if !obj.is_null() && r#type != DRM_MODE_OBJECT_ANY && (*obj).r#type != r#type {
            obj = core::ptr::null_mut();
        }
        if !obj.is_null() && (*obj).id != id {
            obj = core::ptr::null_mut();
        }
        if !obj.is_null()
            && drm_mode_object_lease_required((*obj).r#type)
            && !_drm_lease_held(file_priv, id as i32)
        {
            drm_dbg_kms!(dev, "[OBJECT:%d] not included in lease", id);
            obj = core::ptr::null_mut();
        }
        if !obj.is_null() && (*obj).free_cb.is_some() && !kref_get_unless_zero(&(*obj).refcount) {
            obj = core::ptr::null_mut();
        }
        mutex_unlock(idr_mutex);
        obj
    }
}

// upstream: drm_mode_object.c drm_mode_object_find()
#[unsafe(no_mangle)]
/// # Safety
///
/// `dev` must point to an active `DrmDevice`, and `file_priv` (if non-null) must point to a valid `DrmFile`.
pub unsafe extern "C" fn drm_mode_object_find(
    dev: *mut DrmDevice,
    file_priv: *mut DrmFile,
    id: u32,
    r#type: u32,
) -> *mut DrmModeObject {
    // SAFETY: Forwarding to `__drm_mode_object_find`.
    unsafe { __drm_mode_object_find(dev, file_priv, id, r#type) }
}

// upstream: drm_mode_object.c drm_mode_object_put()
#[unsafe(no_mangle)]
/// # Safety
///
/// `obj` must point to an active `DrmModeObject` with a non-zero reference count.
pub unsafe extern "C" fn drm_mode_object_put(obj: *mut DrmModeObject) {
    // SAFETY: `obj` is valid; decrementing `obj->refcount` via `kref_put` executes `free_cb` on final release.
    unsafe {
        if let Some(free_cb) = (*obj).free_cb {
            drm_dbg_kms!(
                core::ptr::null_mut::<DrmDevice>(),
                "OBJ ID: %d (%d)\n",
                (*obj).id,
                kref_read(&(*obj).refcount)
            );
            kref_put(core::ptr::addr_of_mut!((*obj).refcount), free_cb);
        }
    }
}

// upstream: drm_mode_object.c drm_mode_object_get()
#[unsafe(no_mangle)]
/// # Safety
///
/// `obj` must point to an active `DrmModeObject`.
pub unsafe extern "C" fn drm_mode_object_get(obj: *mut DrmModeObject) {
    // SAFETY: `obj` is valid; incrementing `obj->refcount` via `kref_get`.
    unsafe {
        if (*obj).free_cb.is_some() {
            drm_dbg_kms!(
                core::ptr::null_mut::<DrmDevice>(),
                "OBJ ID: %d (%d)\n",
                (*obj).id,
                kref_read(&(*obj).refcount)
            );
            kref_get(core::ptr::addr_of_mut!((*obj).refcount));
        }
    }
}

// upstream: drm_mode_object.c drm_object_attach_property()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `obj` must point to an initialized `DrmModeObject` whose `properties` field points to a valid, mutable `DrmObjectProperties`.
/// - `property` must point to an initialized, valid `DrmProperty` associated with a valid `DrmDevice`.
/// - Must be called prior to device registration if required by the DRM subsystem (e.g., driver load time).
pub unsafe extern "C" fn drm_object_attach_property(
    obj: *mut DrmModeObject,
    property: *mut DrmProperty,
    init_val: u64,
) {
    // SAFETY: Caller guarantees `obj` and `property` are valid non-null pointers, `(*obj).properties`
    // is valid and writable, and `(*property).dev` points to an initialized `DrmDevice`.
    unsafe {
        let count = (*(*obj).properties).count;
        let dev = (*property).dev;

        if (*obj).r#type == DRM_MODE_OBJECT_CONNECTOR {
            let connector = obj_to_connector(obj);
            WARN_ON_C!(
                (*(*dev).driver).load.is_none()
                    && (*connector).registration_state == DRM_CONNECTOR_REGISTERED
            );
        } else {
            WARN_ON_C!((*(*dev).driver).load.is_none() && (*dev).registered);
        }

        if count == DRM_OBJECT_MAX_PROPERTY {
            // WARN(1, "Failed to attach object property (type: 0x%x) ...")
            warn_on(true);
            return;
        }

        (*(*obj).properties).properties[count as usize] = property;
        (*(*obj).properties).values[count as usize] = init_val;
        (*(*obj).properties).count += 1;
    }
}

// upstream: drm_mode_object.c drm_object_property_set_value()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `obj` must point to a valid `DrmModeObject` whose `properties` points to a valid `DrmObjectProperties`.
/// - `property` must point to a valid `DrmProperty`.
/// - Must only be called for non-atomic drivers or immutable properties under proper modeset locking.
pub unsafe extern "C" fn drm_object_property_set_value(
    obj: *mut DrmModeObject,
    property: *mut DrmProperty,
    val: u64,
) -> i32 {
    // SAFETY: Caller ensures `obj` and `property` are valid pointers with active lifetimes,
    // and `(*obj).properties` points to a valid `DrmObjectProperties` structure.
    unsafe {
        WARN_ON_C!(
            drm_drv_uses_atomic_modeset((*property).dev)
                && ((*property).flags & DRM_MODE_PROP_IMMUTABLE) == 0
        );

        let props: *mut DrmObjectProperties = (*obj).properties;
        for i in 0..(*props).count {
            if (*props).properties[i as usize] == property {
                (*props).values[i as usize] = val;
                return 0;
            }
        }
        -EINVAL
    }
}

// upstream: drm_mode_object.c __drm_object_property_get_prop_value()
/// # Safety
///
/// - `obj` must point to a valid `DrmModeObject` with a valid `properties` pointer.
/// - `property` must point to a valid `DrmProperty`.
/// - `val` must point to a valid, aligned, and writable `u64`.
unsafe fn __drm_object_property_get_prop_value(
    obj: *mut DrmModeObject,
    property: *mut DrmProperty,
    val: *mut u64,
) -> i32 {
    // SAFETY: Caller guarantees `obj`, `(*obj).properties`, and `val` are valid, properly aligned, and dereferenceable.
    unsafe {
        let props: *mut DrmObjectProperties = (*obj).properties;
        for i in 0..(*props).count {
            if (*props).properties[i as usize] == property {
                *val = (*props).values[i as usize];
                return 0;
            }
        }
        -EINVAL
    }
}

// upstream: drm_mode_object.c __drm_object_property_get_value()
/// # Safety
///
/// - `obj` must point to a valid `DrmModeObject`.
/// - `property` must point to a valid `DrmProperty`.
/// - `val` must point to a valid, aligned, and writable `u64`.
unsafe fn __drm_object_property_get_value(
    obj: *mut DrmModeObject,
    property: *mut DrmProperty,
    val: *mut u64,
) -> i32 {
    // SAFETY: Caller ensures `obj`, `property`, and `val` are valid pointers; subcalls inherit these preconditions.
    unsafe {
        // Read-only properties bypass the atomic mechanism and still store
        // their value in the object, so the atomic path is only for mutable
        // properties.
        if drm_drv_uses_atomic_modeset((*property).dev)
            && ((*property).flags & DRM_MODE_PROP_IMMUTABLE) == 0
        {
            return drm_atomic_get_property(obj, property, val);
        }
        __drm_object_property_get_prop_value(obj, property, val)
    }
}

// upstream: drm_mode_object.c drm_object_property_get_value()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `obj` must point to a valid `DrmModeObject`.
/// - `property` must point to a valid `DrmProperty` belonging to a legacy (non-atomic) driver.
/// - `val` must point to a valid, aligned, and writable `u64`.
pub unsafe extern "C" fn drm_object_property_get_value(
    obj: *mut DrmModeObject,
    property: *mut DrmProperty,
    val: *mut u64,
) -> i32 {
    // SAFETY: Caller ensures `obj`, `property`, and `val` are valid pointers; delegated to `__drm_object_property_get_value`.
    unsafe {
        WARN_ON_C!(drm_drv_uses_atomic_modeset((*property).dev));
        __drm_object_property_get_value(obj, property, val)
    }
}

// upstream: drm_mode_object.c drm_object_property_get_default_value()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `obj` must point to a valid `DrmModeObject`.
/// - `property` must point to a valid `DrmProperty` belonging to an atomic driver.
/// - `val` must point to a valid, aligned, and writable `u64`.
pub unsafe extern "C" fn drm_object_property_get_default_value(
    obj: *mut DrmModeObject,
    property: *mut DrmProperty,
    val: *mut u64,
) -> i32 {
    // SAFETY: Caller ensures `obj`, `property`, and `val` are valid pointers; delegated to `__drm_object_property_get_prop_value`.
    unsafe {
        WARN_ON_C!(!drm_drv_uses_atomic_modeset((*property).dev));
        __drm_object_property_get_prop_value(obj, property, val)
    }
}

// upstream: drm_mode_object.c drm_object_immutable_property_get_value()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `obj` must point to a valid `DrmModeObject`.
/// - `property` must point to a valid `DrmProperty` with `DRM_MODE_PROP_IMMUTABLE` set.
/// - `val` must point to a valid, aligned, and writable `u64`.
pub unsafe extern "C" fn drm_object_immutable_property_get_value(
    obj: *mut DrmModeObject,
    property: *mut DrmProperty,
    val: *mut u64,
) -> i32 {
    // SAFETY: Caller ensures `obj`, `property`, and `val` are valid pointers; delegated to `__drm_object_property_get_prop_value`.
    unsafe {
        if WARN_ON_C!(((*property).flags & DRM_MODE_PROP_IMMUTABLE) == 0) {
            return -EINVAL;
        }
        __drm_object_property_get_prop_value(obj, property, val)
    }
}

// upstream: drm_mode_object.c drm_mode_object_get_properties()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `obj` must point to a valid `DrmModeObject` with initialized `properties`.
/// - If `*arg_count_props > 0`, `prop_ptr` must point to a buffer of at least `*arg_count_props` `u32` elements,
///   and `prop_values` must point to a buffer of at least `*arg_count_props` `u64` elements.
/// - `arg_count_props` must point to a valid, aligned, writable `u32`.
pub unsafe extern "C" fn drm_mode_object_get_properties(
    obj: *mut DrmModeObject,
    atomic: bool,
    plane_color_pipeline: bool,
    prop_ptr: *mut u32,
    prop_values: *mut u64,
    arg_count_props: *mut u32,
) -> i32 {
    // SAFETY: Caller guarantees `obj` and its `properties` table are valid, and `prop_ptr`/`prop_values`
    // have sufficient capacity for `*arg_count_props` elements via `put_user`.
    unsafe {
        let props: *mut DrmObjectProperties = (*obj).properties;
        let mut count: u32 = 0;
        for i in 0..(*props).count {
            let prop = (*props).properties[i as usize];
            let mut val: u64 = 0;

            if ((*prop).flags & DRM_MODE_PROP_ATOMIC) != 0 && !atomic {
                continue;
            }

            if plane_color_pipeline && (*obj).r#type == DRM_MODE_OBJECT_PLANE {
                let plane = obj_to_plane(obj);
                if prop == (*plane).color_encoding_property || prop == (*plane).color_range_property
                {
                    continue;
                }
            }

            if !plane_color_pipeline && (*obj).r#type == DRM_MODE_OBJECT_PLANE {
                let plane = obj_to_plane(obj);
                if prop == (*plane).color_pipeline_property {
                    continue;
                }
            }

            if *arg_count_props > count {
                let ret = __drm_object_property_get_value(obj, prop, &mut val);
                if ret != 0 {
                    return ret;
                }

                if put_user(prop_ptr.add(count as usize), (*(prop)).base.id) != 0 {
                    return -EFAULT;
                }
                if put_user(prop_values.add(count as usize), val) != 0 {
                    return -EFAULT;
                }
            }

            count += 1;
        }
        *arg_count_props = count;

        0
    }
}

/// Runs `body` under the `DRM_MODESET_LOCK_ALL_BEGIN()` / `_END()` macro pair
/// from drm_modeset_lock.h: the legacy `mode_config.mutex` is taken only when
/// the driver is not atomic, and `-EDEADLK` backs off and retries the whole
/// lock-all + body sequence.
// upstream: include/drm/drm_modeset_lock.h DRM_MODESET_LOCK_ALL_BEGIN/END
/// # Safety
///
/// - `dev` must point to a valid `DrmDevice` with initialized `mode_config`.
/// - `body` must not violate locking invariants or cause deadlocks with other locks held.
pub(crate) unsafe fn modeset_lock_all_scope(
    dev: *mut DrmDevice,
    flags: u32,
    body: &mut dyn FnMut() -> i32,
) -> i32 {
    // SAFETY: Caller ensures `dev` is valid and dereferenceable; `ctx_storage` is stack-allocated
    // and properly initialized with `drm_modeset_acquire_init`.
    unsafe {
        let mut ctx_storage = MaybeUninit::<DrmModesetAcquireCtx>::zeroed();
        let ctx = ctx_storage.as_mut_ptr();

        if !drm_drv_uses_atomic_modeset(dev) {
            mutex_lock(&mut (*dev).mode_config.mutex as *mut Mutex);
        }

        drm_modeset_acquire_init(ctx, flags);
        let mut ret;
        loop {
            ret = drm_modeset_lock_all_ctx(dev, ctx);
            if ret == 0 {
                ret = body();
            }

            // modeset_lock_fail:
            if ret == -EDEADLK {
                ret = drm_modeset_backoff(ctx);
                if ret == 0 {
                    continue;
                }
            }
            break;
        }
        drm_modeset_drop_locks(ctx);
        drm_modeset_acquire_fini(ctx);

        if !drm_drv_uses_atomic_modeset(dev) {
            mutex_unlock(&mut (*dev).mode_config.mutex as *mut Mutex);
        }
        ret
    }
}

// upstream: drm_mode_object.c drm_mode_obj_get_properties_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid `DrmDevice`.
/// - `data` must point to a valid, readable and writable `DrmModeObjGetProperties` struct.
/// - `file_priv` must point to a valid `DrmFile`.
pub unsafe extern "C" fn drm_mode_obj_get_properties_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: Caller ensures `dev`, `data`, and `file_priv` point to valid structures;
    // user buffers in `arg` are accessed via `put_user` inside `drm_mode_object_get_properties`.
    unsafe {
        let arg = data.cast::<DrmModeObjGetProperties>();

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        modeset_lock_all_scope(dev, 0, &mut || {
            let obj = drm_mode_object_find(dev, file_priv, (*arg).obj_id, (*arg).obj_type);
            if obj.is_null() {
                return -ENOENT;
            }
            if (*obj).properties.is_null() {
                drm_mode_object_put(obj);
                return -EINVAL;
            }
            let ret = drm_mode_object_get_properties(
                obj,
                (*file_priv)
                    .atomic
                    .load(core::sync::atomic::Ordering::Relaxed),
                (*file_priv)
                    .plane_color_pipeline
                    .load(core::sync::atomic::Ordering::Relaxed),
                (*arg).props_ptr as usize as *mut u32,
                (*arg).prop_values_ptr as usize as *mut u64,
                core::ptr::addr_of_mut!((*arg).count_props),
            );
            drm_mode_object_put(obj);
            ret
        })
    }
}

// upstream: drm_mode_object.c drm_mode_obj_find_prop_id()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `obj` must point to a valid `DrmModeObject` with initialized `properties`.
pub unsafe extern "C" fn drm_mode_obj_find_prop_id(
    obj: *mut DrmModeObject,
    prop_id: u32,
) -> *mut DrmProperty {
    // SAFETY: Caller guarantees `obj` and `(*obj).properties` are valid, dereferenceable, and properly synchronized.
    unsafe {
        let props: *mut DrmObjectProperties = (*obj).properties;
        for i in 0..(*props).count {
            if (*(*props).properties[i as usize]).base.id == prop_id {
                return (*props).properties[i as usize];
            }
        }
        core::ptr::null_mut()
    }
}

// upstream: drm_mode_object.c set_property_legacy()
/// # Safety
///
/// - `obj` must point to a valid `DrmModeObject`.
/// - `prop` must point to a valid `DrmProperty` with non-null `dev`.
unsafe fn set_property_legacy(
    obj: *mut DrmModeObject,
    prop: *mut DrmProperty,
    prop_value: u64,
) -> i32 {
    // SAFETY: Caller guarantees `obj` and `prop` are valid and remain alive for the duration of this call.
    unsafe {
        let dev = (*prop).dev;
        let mut r#ref: *mut DrmModeObject = core::ptr::null_mut();

        if !drm_property_change_valid_get(prop, prop_value, &mut r#ref) {
            return -EINVAL;
        }

        let ret = modeset_lock_all_scope(dev, 0, &mut || {
            let mut ret = -EINVAL;
            match (*obj).r#type {
                DRM_MODE_OBJECT_CONNECTOR => {
                    ret = drm_connector_set_obj_prop(obj, prop, prop_value);
                }
                DRM_MODE_OBJECT_CRTC => {
                    ret = drm_mode_crtc_set_obj_prop(obj, prop, prop_value);
                }
                DRM_MODE_OBJECT_PLANE => {
                    ret = drm_mode_plane_set_obj_prop(obj_to_plane(obj), prop, prop_value);
                }
                _ => {}
            }
            ret
        });

        drm_property_change_valid_put(prop, r#ref);
        ret
    }
}

// upstream: drm_mode_object.c set_property_atomic()
/// # Safety
///
/// - `obj` must point to a valid `DrmModeObject`.
/// - `file_priv` must point to a valid `DrmFile`.
/// - `prop` must point to a valid `DrmProperty` with non-null `dev`.
unsafe fn set_property_atomic(
    obj: *mut DrmModeObject,
    file_priv: *mut DrmFile,
    prop: *mut DrmProperty,
    prop_value: u64,
) -> i32 {
    // SAFETY: Caller ensures `obj`, `file_priv`, and `prop` are valid pointers;
    // modeset acquire context is safely initialized on stack and cleaned up before return.
    unsafe {
        let dev = (*prop).dev;
        let state = drm_atomic_commit_alloc(dev);
        if state.is_null() {
            return -ENOMEM;
        }

        let mut ctx_storage = MaybeUninit::<DrmModesetAcquireCtx>::zeroed();
        let ctx = ctx_storage.as_mut_ptr();
        drm_modeset_acquire_init(ctx, 0);
        (*state).acquire_ctx = ctx;

        let mut ret;
        loop {
            // retry:
            if prop == (*(*state).dev).mode_config.dpms_property {
                if (*obj).r#type != DRM_MODE_OBJECT_CONNECTOR {
                    ret = -EINVAL;
                } else {
                    ret = drm_atomic_connector_commit_dpms(
                        state,
                        obj_to_connector(obj),
                        prop_value as i32,
                    );
                }
            } else {
                ret = drm_atomic_set_property(state, file_priv, obj, prop, prop_value, false);
                if ret == 0 {
                    ret = drm_atomic_commit(state);
                }
            }

            // out:
            if ret == -EDEADLK {
                drm_atomic_commit_clear(state);
                drm_modeset_backoff(ctx);
                continue;
            }
            break;
        }

        drm_atomic_commit_put(state);
        drm_modeset_drop_locks(ctx);
        drm_modeset_acquire_fini(ctx);
        ret
    }
}

// upstream: include/drm/drm_atomic.h drm_atomic_commit_put()
/// # Safety
///
/// `state` must point to a valid `DrmAtomicCommit` with an active reference count.
unsafe fn drm_atomic_commit_put(state: *mut DrmAtomicCommit) {
    // SAFETY: `state` is a valid pointer; `kref_put` atomically decrements the reference count.
    unsafe {
        kref_put(
            core::ptr::addr_of_mut!((*state).r#ref),
            __drm_atomic_commit_free,
        );
    }
}

// upstream: drm_mode_object.c drm_mode_obj_set_property_ioctl()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `dev` must point to a valid `DrmDevice`.
/// - `data` must point to a valid, readable and writable `DrmModeObjSetProperty` struct.
/// - `file_priv` must point to a valid `DrmFile`.
pub unsafe extern "C" fn drm_mode_obj_set_property_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file_priv: *mut DrmFile,
) -> i32 {
    // SAFETY: Caller ensures `dev`, `data`, and `file_priv` are valid, dereferenceable pointers.
    unsafe {
        let arg = data.cast::<DrmModeObjSetProperty>();
        let mut ret = -EINVAL;

        if !drm_core_check_feature(dev, DRIVER_MODESET) {
            return -EOPNOTSUPP;
        }

        let arg_obj = drm_mode_object_find(dev, file_priv, (*arg).obj_id, (*arg).obj_type);
        if arg_obj.is_null() {
            return -ENOENT;
        }

        'out_unref: {
            if (*arg_obj).properties.is_null() {
                break 'out_unref;
            }

            let property = drm_mode_obj_find_prop_id(arg_obj, (*arg).prop_id);
            if property.is_null() {
                break 'out_unref;
            }

            if drm_drv_uses_atomic_modeset((*property).dev) {
                ret = set_property_atomic(arg_obj, file_priv, property, (*arg).value);
            } else {
                ret = set_property_legacy(arg_obj, property, (*arg).value);
            }
        }

        drm_mode_object_put(arg_obj);
        ret
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::{
        drm_property_upstream::drm_property_create_range,
        test_support::TestDrm,
        uapi_drm_consts::{DRM_MODE_OBJECT_CRTC, DRM_MODE_OBJECT_FB},
    };

    /// A zeroed `drm_mode_object` with its property table attached.
    fn empty_object(props: *mut DrmObjectProperties) -> DrmModeObject {
        // SAFETY: `DrmModeObject` is a C-compatible representation with integer/pointer fields,
        // valid to initialize as all-zeros.
        let mut obj: DrmModeObject = unsafe { core::mem::zeroed() };
        obj.properties = props;
        obj
    }

    #[test]
    fn object_ids_allocate_lookup_and_unregister() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        t.init_registry();
        let dev = t.ptr();
        let mut obj = empty_object(core::ptr::null_mut());

        // SAFETY: Stack-allocated `obj` and `dev` from `TestDrm` are valid pointers for the test scope.
        unsafe {
            assert_eq!(drm_mode_object_add(dev, &mut obj, DRM_MODE_OBJECT_CRTC), 0);
            assert!(obj.id >= 1);
            assert_eq!(obj.r#type, DRM_MODE_OBJECT_CRTC);

            let found =
                drm_mode_object_find(dev, core::ptr::null_mut(), obj.id, DRM_MODE_OBJECT_ANY);
            assert_eq!(found, &mut obj as *mut DrmModeObject);
            // Wrong type: the lookup must not return the object.
            let wrong =
                drm_mode_object_find(dev, core::ptr::null_mut(), obj.id, DRM_MODE_OBJECT_FB);
            assert!(wrong.is_null());

            let id = obj.id;
            drm_mode_object_unregister(dev, &mut obj);
            assert_eq!(obj.id, 0);
            assert!(
                drm_mode_object_find(dev, core::ptr::null_mut(), id, DRM_MODE_OBJECT_ANY).is_null()
            );
        }
    }

    #[test]
    fn two_objects_get_distinct_ids() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        t.init_registry();
        let dev = t.ptr();
        let mut a = empty_object(core::ptr::null_mut());
        let mut b = empty_object(core::ptr::null_mut());
        // SAFETY: Stack-allocated objects and `TestDrm` are valid for the duration of the test.
        unsafe {
            assert_eq!(drm_mode_object_add(dev, &mut a, DRM_MODE_OBJECT_CRTC), 0);
            assert_eq!(drm_mode_object_add(dev, &mut b, DRM_MODE_OBJECT_CRTC), 0);
        }
        assert_ne!(a.id, b.id);
        assert_eq!(a.id.min(b.id), 1);
    }

    static FREED: AtomicU32 = AtomicU32::new(0);

    /// # Safety
    ///
    /// `_kref` must point to a valid `Kref` being released.
    unsafe extern "C" fn free_cb(_kref: *mut Kref) {
        FREED.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn refcounted_object_runs_free_callback_on_last_put() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        t.init_registry();
        let dev = t.ptr();
        let mut obj = empty_object(core::ptr::null_mut());
        FREED.store(0, Ordering::SeqCst);

        // SAFETY: Stack-allocated object and `TestDrm` are valid; refcount lifecycle exercised within scope.
        unsafe {
            assert_eq!(
                __drm_mode_object_add(dev, &mut obj, DRM_MODE_OBJECT_CRTC, true, Some(free_cb)),
                0
            );
            assert_eq!(drm_kapi::memory::kref_read(&obj.refcount), 1);

            drm_mode_object_get(&mut obj);
            assert_eq!(drm_kapi::memory::kref_read(&obj.refcount), 2);
            drm_mode_object_put(&mut obj);
            assert_eq!(FREED.load(Ordering::SeqCst), 0);
            drm_mode_object_put(&mut obj);
            assert_eq!(FREED.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn attached_property_values_get_and_set() {
        let mut t = TestDrm::new(DRIVER_MODESET);
        t.init_registry();
        let dev = t.ptr();
        // SAFETY: `DrmObjectProperties` is POD and valid to zero-initialize.
        let mut props: DrmObjectProperties = unsafe { core::mem::zeroed() };
        let mut obj = empty_object(&mut props);
        // SAFETY: Stack-allocated `obj`, `props`, and `TestDrm` device are valid throughout the test.
        unsafe {
            assert_eq!(drm_mode_object_add(dev, &mut obj, DRM_MODE_OBJECT_CRTC), 0);
            let prop = drm_property_create_range(dev, 0, b"test\0".as_ptr().cast(), 0, 100);
            assert!(!prop.is_null());

            drm_object_attach_property(&mut obj, prop, 5);
            assert_eq!(props.count, 1);

            let mut val = 0u64;
            assert_eq!(drm_object_property_get_value(&mut obj, prop, &mut val), 0);
            assert_eq!(val, 5);

            assert_eq!(drm_object_property_set_value(&mut obj, prop, 42), 0);
            assert_eq!(drm_object_property_get_value(&mut obj, prop, &mut val), 0);
            assert_eq!(val, 42);

            // A property that was never attached to this object.
            let other = drm_property_create_range(dev, 0, b"other\0".as_ptr().cast(), 0, 1);
            assert_eq!(drm_object_property_set_value(&mut obj, other, 1), -EINVAL);
            assert_eq!(
                drm_object_property_get_value(&mut obj, other, &mut val),
                -EINVAL
            );

            assert_eq!(drm_mode_obj_find_prop_id(&mut obj, (*prop).base.id), prop);
        }
    }

    #[test]
    fn lease_required_only_for_crtc_connector_plane() {
        assert!(drm_mode_object_lease_required(DRM_MODE_OBJECT_CRTC));
        assert!(!drm_mode_object_lease_required(DRM_MODE_OBJECT_FB));
    }
}
